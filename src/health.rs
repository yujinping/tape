//! `GET /__tape/health`：供 App、管理界面与脚本探测 tape 是否可用、当前处于哪种模式。
//!
//! 该端点必须**不产生录制数据**，也不能拦截正常业务流量，因此识别条件很严格：
//! 只接受直连 tape 的 origin-form 请求（无 authority）。
//! - 正向代理请求 `GET http://host/__tape/health` 带 authority，继续转发上游；
//! - 前缀式请求路径是 `/http://host/__tape/health`，不会与健康检查路径相等。
//!
//! 透明模式下客户端不会配置代理，上游自身的 `/health` 与 tape 的健康检查会撞车，
//! 所以主路径改为带保留前缀的 `/__tape/health`。旧路径 `/health` 仍然可用，
//! 但只在「没有 Host 头」或「Host 端口等于 tape 监听端口」时生效：
//! 前者是 HTTP/1.0 或本机脚本，后者是 `curl http://本机IP:端口/health`。
//! 透明 DNAT 进来的请求 Host 端口是上游端口（通常是 80），不会被误判。

use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{CACHE_CONTROL, CONTENT_TYPE, HOST};
use hyper::{Method, Request, Response, StatusCode};

/// 健康检查主路径（不与任何上游路径冲突）。
pub const PATH: &str = "/__tape/health";

/// 旧健康检查路径，仅为兼容已有脚本与文档保留。
pub const LEGACY_PATH: &str = "/health";

/// 是否为直连 tape 的健康检查请求。
///
/// `listen_port` 用于判断旧路径 `/health` 是否指向 tape 自身：Host 里没写端口时
/// 按 HTTP 默认端口 80 计算。
pub fn is_request<B>(req: &Request<B>, listen_port: u16) -> bool {
    if req.method() != Method::GET || req.uri().authority().is_some() {
        return false;
    }
    let path = req.uri().path();
    if path == PATH {
        return true;
    }
    if path != LEGACY_PATH {
        return false;
    }
    match req.headers().get(HOST).and_then(|v| v.to_str().ok()) {
        // 没有 Host：不可能是转发到上游的请求，按直连处理
        None => true,
        // Host 端口等于 tape 监听端口：视为访问 tape 自身
        Some(host) => host_port(host).is_none_or(|port| port == listen_port),
    }
}

/// 从 Host 头解析端口；没有端口时按 HTTP 默认 80。解析失败返回 None。
fn host_port(host: &str) -> Option<u16> {
    match host.parse::<hyper::http::uri::Authority>() {
        Ok(authority) => Some(authority.port_u16().unwrap_or(80)),
        Err(_) => None,
    }
}

/// 构造健康检查响应：`{"ok":true,"mode":"record","version":"0.2.0"}`。
pub fn response(mode: &str) -> Response<Full<Bytes>> {
    let body = serde_json::json!({
        "ok": true,
        "mode": mode,
        "version": env!("CARGO_PKG_VERSION"),
    })
    .to_string();
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .header(CACHE_CONTROL, "no-store")
        .body(Full::new(Bytes::from(body)))
        .expect("健康检查响应构造失败")
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::Empty;

    fn request(line: &str, host: Option<&str>) -> Request<Empty<Bytes>> {
        let mut builder = Request::builder().method("GET").uri(line);
        if let Some(host) = host {
            builder = builder.header(HOST, host);
        }
        builder.body(Empty::new()).unwrap()
    }

    #[test]
    fn reserved_path_always_matches_origin_form() {
        assert!(is_request(&request("/__tape/health", None), 8888));
        assert!(is_request(
            &request("/__tape/health", Some("10.1.2.3:8888")),
            8888
        ));
        // 代理到上游的同名路径：带 authority，不能拦截，否则上游永远收不到
        assert!(!is_request(
            &request("http://10.1.2.3:8080/__tape/health", None),
            8888
        ));
        // 前缀式请求
        assert!(!is_request(
            &request("/http://10.1.2.3:8080/__tape/health", None),
            8888
        ));
    }

    #[test]
    fn legacy_health_only_matches_tape_itself() {
        // curl http://本机IP:8888/health：Host 端口等于监听端口
        assert!(is_request(
            &request("/health", Some("192.168.1.5:8888")),
            8888
        ));
        // 没有 Host（HTTP/1.0 或本机脚本）
        assert!(is_request(&request("/health", None), 8888));
        // 透明模式转发的上游 /health：Host 是上游地址，端口 80 不等于监听端口
        assert!(!is_request(&request("/health", Some("10.1.2.3:80")), 8888));
        assert!(!is_request(&request("/health", Some("10.1.2.3")), 8888));
        // 其它路径
        assert!(!is_request(&request("/healthz", None), 8888));
    }
}
