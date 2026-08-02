//! 网络基础设施：http/https 转发客户端，供 record 与 download 共用。
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::Full;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

/// 同时支持 http/https 上游的转发客户端。
pub type HttpClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

/// 构建转发客户端：默认校验系统根证书；设置 `TAPE_INSECURE_TLS=1` 时跳过上游证书校验。
pub fn build_client() -> Result<HttpClient> {
    let https = if insecure_tls_enabled() {
        let config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();
        HttpsConnectorBuilder::new()
            .with_tls_config(config)
            .https_or_http()
            .enable_http1()
            .build()
    } else {
        HttpsConnectorBuilder::new()
            .with_native_roots()
            .context("加载系统 TLS 根证书失败")?
            .https_or_http()
            .enable_http1()
            .build()
    };
    Ok(Client::builder(TokioExecutor::new()).build(https))
}

/// 专网自签证书场景：设置 TAPE_INSECURE_TLS=1 跳过上游 TLS 证书校验。
fn insecure_tls_enabled() -> bool {
    // 显式白名单：只有 1 / true / yes / on（大小写不敏感）才启用，
    // 其余取值（0 / false / no / off / 空）一律视为关闭，避免 TAPE_INSECURE_TLS=false 反而开启跳过校验。
    matches!(
        std::env::var("TAPE_INSECURE_TLS").as_deref(),
        Ok(v) if matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
    )
}

/// 跳过证书校验的验证器（仅 TAPE_INSECURE_TLS=1 时启用）。
#[derive(Debug)]
struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::CryptoProvider::get_default()
            .map(|p| p.signature_verification_algorithms.supported_schemes())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insecure_tls_whitelist_semantics() {
        // 显式开启值
        for v in ["1", "true", "TRUE", "Yes", "on", " 1 "] {
            unsafe { std::env::set_var("TAPE_INSECURE_TLS", v) };
            assert!(insecure_tls_enabled(), "{v:?} 应启用跳过校验");
        }
        // 其余取值一律关闭（尤其 false 不应误开启）
        for v in ["0", "false", "FALSE", "no", "off", ""] {
            unsafe { std::env::set_var("TAPE_INSECURE_TLS", v) };
            assert!(!insecure_tls_enabled(), "{v:?} 不应启用跳过校验");
        }
        // 未设置同样关闭
        unsafe { std::env::remove_var("TAPE_INSECURE_TLS") };
        assert!(!insecure_tls_enabled());
    }
}
