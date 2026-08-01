use hyper::Uri;
use hyper::header::HeaderName;

/// 解析出的代理请求目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestTarget {
    pub scheme: String,
    pub authority: String,
    pub path_and_query: String,
    /// 是否为 URL 前缀式请求（origin-form，路径以 /http:// 或 /https:// 开头）。
    /// false 表示 standard absolute-form 正向代理请求。
    pub prefix: bool,
}

/// 自动识别请求目标，支持两种形式：
/// - absolute-form（标准正向代理）：`GET http://host:port/path HTTP/1.1`
/// - URL 前缀式（盒子应用无法配置代理时，把 tape 当服务器直接加前缀）：
///   `GET /http://host:port/path HTTP/1.1`
///
/// 其余普通 origin-form 请求返回 `None`，由调用方按“直连本机”处理。
pub fn parse_proxy_target(uri: &Uri) -> Option<RequestTarget> {
    if let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) {
        return Some(RequestTarget {
            scheme: scheme.to_ascii_lowercase(),
            authority: authority.as_str().to_string(),
            path_and_query: uri
                .path_and_query()
                .map(|p| {
                    let s = p.as_str();
                    if s.is_empty() {
                        "/".to_string()
                    } else {
                        s.to_string()
                    }
                })
                .unwrap_or_else(|| "/".to_string()),
            prefix: false,
        });
    }

    let target_text = uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or(uri.path());
    let rest = target_text.strip_prefix('/')?;
    let (scheme, consumed, slashes) = split_scheme_prefix(rest)?;
    let after_scheme = &rest[consumed..];

    let after_scheme = after_scheme.to_string();
    // 单斜杠形式（`/https:/host/...`，个别库会把 scheme 后的 `//` 折叠成一个斜杠）：
    // 分隔斜杠只剩一个，剥掉后再找 authority 边界；双斜杠且 authority 为空
    // （`/http:///api`）保持原语义（authority 为空 -> None）。
    let after_scheme = if slashes == 1 {
        after_scheme.strip_prefix('/').unwrap_or(after_scheme.as_str())
    } else {
        after_scheme.as_str()
    };
    let mut end = after_scheme.len();
    if let Some(i) = after_scheme.find('/') {
        end = i;
    } else if let Some(i) = after_scheme.find('?') {
        end = i;
    }
    let authority_raw = &after_scheme[..end];
    if authority_raw.is_empty() {
        return None;
    }
    let authority = percent_decode(authority_raw);
    if authority.is_empty() || authority.contains('#') {
        return None;
    }
    let mut path_and_query = after_scheme[end..].to_string();
    if path_and_query.is_empty() {
        path_and_query = "/".to_string();
    }
    Some(RequestTarget {
        scheme: scheme.to_string(),
        authority: authority.to_string(),
        path_and_query,
        prefix: true,
    })
}

/// 识别前缀式路径开头的 scheme 标记（`http://` / `https://`），返回 (scheme, 已消费字节数, 斜杠数)。
///
/// 容忍浏览器的百分号编码：冒号可能被编成 `%3A`，个别浏览器编成 `%20`（空格）；
/// 斜杠可能被编成 `%2F`。只消费 scheme 标记本身，authority/path 保持原始未解码，
/// 避免误伤路径/query 中本来的编码内容。
fn split_scheme_prefix(rest: &str) -> Option<(&'static str, usize, usize)> {
    let b = rest.as_bytes();
    let (scheme, mut i) = if b.len() >= 5 && rest[..5].eq_ignore_ascii_case("https") {
        ("https", 5)
    } else if b.len() >= 4 && rest[..4].eq_ignore_ascii_case("http") {
        ("http", 4)
    } else {
        return None;
    };

    // 冒号：字面 ':'、%3A，或个别浏览器编码成的空格 %20
    if b.get(i) == Some(&b':') {
        i += 1;
    } else if rest[i..].starts_with("%3A") || rest[i..].starts_with("%3a") {
        i += 3;
    } else if b.get(i) == Some(&b' ') || rest[i..].starts_with("%20") {
        i += if b.get(i) == Some(&b' ') { 1 } else { 3 };
    } else {
        return None;
    }

    // 斜杠 1~2 个：字面 '/' 或 %2F。标准前缀是 `//`，个别库会把 `//` 折叠成单斜杠
    // （OkHttp 对 `http://127.0.0.1:8888/https://host/` 做根路径解析时的形态），都接受。
    let mut slashes = 0;
    for _ in 0..2 {
        if b.get(i) == Some(&b'/') {
            i += 1;
            slashes += 1;
        } else if rest[i..].starts_with("%2F") || rest[i..].starts_with("%2f") {
            i += 3;
            slashes += 1;
        } else {
            break;
        }
    }
    if slashes == 0 {
        return None;
    }
    Some((scheme, i, slashes))
}

/// 解码 `%XX` 百分号编码（authority 段的端口冒号等可能被浏览器编码）。
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push(hi * 16 + lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// RFC 7230 hop-by-hop 头，转发/回传时需要剥离（由 hyper 依据 body 重建）。
pub fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

pub fn is_hop_by_hop_str(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(s: &str) -> Uri {
        s.parse().unwrap()
    }

    #[test]
    fn absolute_form_is_recognized() {
        let t = parse_proxy_target(&uri("http://10.1.2.3:8080/api/user?x=1")).unwrap();
        assert_eq!(t.scheme, "http");
        assert_eq!(t.authority, "10.1.2.3:8080");
        assert_eq!(t.path_and_query, "/api/user?x=1");
        assert!(!t.prefix);
    }

    #[test]
    fn prefix_form_is_recognized() {
        let t = parse_proxy_target(&uri("/http://10.1.2.3:8080/api/user?x=1")).unwrap();
        assert_eq!(t.scheme, "http");
        assert_eq!(t.authority, "10.1.2.3:8080");
        assert_eq!(t.path_and_query, "/api/user?x=1");
        assert!(t.prefix);
    }

    #[test]
    fn prefix_https_is_recognized() {
        let t = parse_proxy_target(&uri("/https://api.company.com/v1/login")).unwrap();
        assert_eq!(t.scheme, "https");
        assert_eq!(t.authority, "api.company.com");
        assert_eq!(t.path_and_query, "/v1/login");
    }

    #[test]
    fn prefix_single_slash_is_recognized() {
        // 个别库把 scheme 后的 `//` 折叠成单斜杠（如 OkHttp 对
        // `http://127.0.0.1:8888/https://host/` 做根路径解析时的形态）
        let t = parse_proxy_target(&uri("/http:/10.1.2.3:8080/api/user?x=1")).unwrap();
        assert_eq!(t.scheme, "http");
        assert_eq!(t.authority, "10.1.2.3:8080");
        assert_eq!(t.path_and_query, "/api/user?x=1");
        assert!(t.prefix);
        let t = parse_proxy_target(&uri("/https:/www.dingtalk.com/api/v1/login")).unwrap();
        assert_eq!(t.scheme, "https");
        assert_eq!(t.authority, "www.dingtalk.com");
        assert_eq!(t.path_and_query, "/api/v1/login");
    }

    #[test]
    fn prefix_single_slash_without_path_defaults_to_root() {
        let t = parse_proxy_target(&uri("/https:/www.dingtalk.com")).unwrap();
        assert_eq!(t.scheme, "https");
        assert_eq!(t.authority, "www.dingtalk.com");
        assert_eq!(t.path_and_query, "/");
    }

    #[test]
    fn prefix_without_path_defaults_to_root() {
        let t = parse_proxy_target(&uri("/http://10.1.2.3:8080")).unwrap();
        assert_eq!(t.authority, "10.1.2.3:8080");
        assert_eq!(t.path_and_query, "/");
    }

    #[test]
    fn prefix_ignores_case_of_scheme() {
        let t = parse_proxy_target(&uri("/HTTP://10.1.2.3:8080/img/a.png")).unwrap();
        assert_eq!(t.scheme, "http");
        assert_eq!(t.authority, "10.1.2.3:8080");
        assert_eq!(t.path_and_query, "/img/a.png");
    }

    #[test]
    fn prefix_tolerates_percent_encoded_scheme() {
        // 浏览器把 : 编成 %3A
        let t = parse_proxy_target(&uri("/https%3A//login.dingtalk.com/api/v1")).unwrap();
        assert_eq!(t.scheme, "https");
        assert_eq!(t.authority, "login.dingtalk.com");
        assert_eq!(t.path_and_query, "/api/v1");
        // 全编码标记
        let t = parse_proxy_target(&uri("/http%3A%2F%2F10.1.2.3%3A8080/x")).unwrap();
        assert_eq!(t.scheme, "http");
        assert_eq!(t.authority, "10.1.2.3:8080");
        assert_eq!(t.path_and_query, "/x");
    }

    #[test]
    fn prefix_tolerates_browser_space_quirk() {
        // 个别浏览器把 : 编成 %20（空格）
        let t = parse_proxy_target(&uri("/https%20//login.dingtalk.com/api/v1")).unwrap();
        assert_eq!(t.scheme, "https");
        assert_eq!(t.authority, "login.dingtalk.com");
        assert_eq!(t.path_and_query, "/api/v1");
    }

    #[test]
    fn prefix_keeps_path_and_query_encoded() {
        // 只识别 scheme 标记，路径/query 保持原始编码，避免误解码
        let t = parse_proxy_target(&uri("/http://10.1.2.3:8080/api?x=1%202")).unwrap();
        assert_eq!(t.authority, "10.1.2.3:8080");
        assert_eq!(t.path_and_query, "/api?x=1%202");
    }

    #[test]
    fn plain_origin_form_is_none() {
        assert!(parse_proxy_target(&uri("/api/user?x=1")).is_none());
    }

    #[test]
    fn malformed_prefix_is_none() {
        assert!(parse_proxy_target(&uri("/http:///api")).is_none());
        assert!(parse_proxy_target(&uri("/ftp://10.1.2.3/api")).is_none());
        assert!(parse_proxy_target(&uri("/api/http://10.1.2.3/x")).is_none());
    }
}
