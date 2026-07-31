use base64::Engine;
use serde::{Deserialize, Serialize};

pub const ENCODING_UTF8: &str = "utf8";
pub const ENCODING_BASE64: &str = "base64";

/// 单接口快照：录制时的原始请求/响应全量数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// 6 位录制序号
    pub id: String,
    /// 原始上游地址，如 http://10.1.2.3:8080
    pub origin: String,
    /// RFC3339 录制时间
    pub recorded_at: String,
    /// 接口耗时（毫秒）
    pub duration_ms: u64,
    pub request: RequestRecord,
    pub response: ResponseRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestRecord {
    pub method: String,
    /// 原始绝对 URL（含 path 与 query）
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub body_encoding: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseRecord {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub body_encoding: String,
}

/// UTF-8 可解码时按文本存储，否则 base64，保证字节级无损。
pub fn encode_body(bytes: &[u8]) -> (String, String) {
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_string(), ENCODING_UTF8.to_string()),
        Err(_) => (
            base64::engine::general_purpose::STANDARD.encode(bytes),
            ENCODING_BASE64.to_string(),
        ),
    }
}

pub fn decode_body(body: &str, encoding: &str) -> Vec<u8> {
    if encoding == ENCODING_BASE64 {
        base64::engine::general_purpose::STANDARD
            .decode(body)
            .unwrap_or_default()
    } else {
        body.as_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_utf8_body_roundtrip() {
        let body = "{\"a\":\"中文\"}";
        let (encoded, encoding) = encode_body(body.as_bytes());
        assert_eq!(encoding, ENCODING_UTF8);
        assert_eq!(encoded, body);
        assert_eq!(decode_body(&encoded, &encoding), body.as_bytes());
    }

    #[test]
    fn encode_binary_body_roundtrip() {
        let body = vec![0u8, 159, 146, 150, 255, 1, 2, 3];
        let (encoded, encoding) = encode_body(&body);
        assert_eq!(encoding, ENCODING_BASE64);
        assert_eq!(decode_body(&encoded, &encoding), body);
    }
}
