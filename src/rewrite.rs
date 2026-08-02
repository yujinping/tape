use std::io::{Read, Write};
use std::sync::LazyLock;

use bytes::Bytes;
use flate2::Compression;
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use flate2::write::{GzEncoder, ZlibEncoder};
use regex::{Captures, Regex};

static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    // group4 同时接受「/path?query」与「?query」（无 path 直接带 query 的形态）
    Regex::new(
        r#"(?i)\b(https?)://([a-z0-9.\-]+)(?::(\d{1,5}))?((?:/[^\s"'<>(){}]*)?(?:\?[^\s"'<>(){}]*)?)"#,
    )
    .expect("invalid URL regex")
});

/// 协议相对 URL：`//host/path`（HTML src/href、CSS url()、JS 字符串里很常见）。
/// 前缀式模式下浏览器会按页面 scheme 解析成 `http://host/path` 直连公网，必须改写回 tape。
/// 前导字符组 `(^|[^:\w-])` 用于排除 `http://` 这类 `scheme://` 里的 `//`
/// （前导为 `:` 或字母数字时不视为协议相对 URL），同时排除 `-`：
/// DOCTYPE 公共标识符 `-//W3C//DTD ...` 里的 `//W3C` 不能当作协议相对链接。
static PROTOCOL_REL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(^|[^:\w-])//([a-z0-9.\-]+)(?::(\d{1,5}))?((?:/[^\s"'<>(){}]*)?(?:\?[^\s"'<>(){}]*)?)"#,
    )
    .expect("invalid protocol-relative regex")
});

/// HTML 标签属性里的根相对路径（`href="/assets/x.css"`、`src="/js/a.js"`、`action="/login"` 等）。
/// 前缀式模式下浏览器会把 `/assets/x.css` 解析成 `http://<tape>/assets/x.css`（丢失前缀），
/// 必须改写成 `{base}/{scheme}://{origin}{path}`。
static HTML_ATTR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(\b(?:href|src|action|poster)\s*=\s*["'])(/[^"'\s>]*)("|')"#)
        .expect("invalid html attr regex")
});

/// CSS 里的根相对路径 `url(/fonts/x.woff2)`（含带引号形式）。
/// 浏览器会相对 CSS 文件解析成 `http://<tape>/fonts/x.woff2`，同样丢失前缀。
static CSS_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(url\(\s*)(["']?)(/[^"'\s)]+)"#).expect("invalid css url regex")
});

/// 响应改写规则。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewriteRule {
    /// 改写为相对路径（推荐）
    Relative,
    /// 改写为绝对地址（拼接到 base 后）
    Absolute { base: String },
    /// 改写为回到 tape 的前缀式地址：`{base}/{scheme}://{host}{:port}{path}`
    /// （供无法配置代理、直接加前缀访问的盒子应用使用，保证跳转/链接继续回到 tape）
    /// `scheme` 为当前请求的上游 scheme（http/https），用于改写协议相对链接 `//host/path`
    /// 与根相对路径 `/path`；`origin` 为当前请求的目标 authority（host[:port]），
    /// 根相对路径没有自己的 host，须按请求目标解析。
    Prefix {
        base: String,
        scheme: String,
        origin: String,
    },
    /// 不改写
    None,
}

/// 对文本响应做全局 URL 改写，保持其余字节不变。
pub fn rewrite_text(input: &str, rule: &RewriteRule) -> String {
    match rule {
        RewriteRule::None => input.to_string(),
        RewriteRule::Prefix { .. } => {
            let first = URL_RE.replace_all(input, |caps: &Captures| replaced(rule, caps));
            let second = PROTOCOL_REL_RE.replace_all(&first, |caps: &Captures| {
                replaced_protocol_relative(rule, caps)
            });
            HTML_ATTR_RE
                .replace_all(&second, |caps: &Captures| replaced_html_attr(rule, caps))
                .into_owned()
            // CSS url() 由调用方按内容类型追加（见 rewrite_css_response_bytes）
        }
        _ => URL_RE
            .replace_all(input, |caps: &Captures| replaced(rule, caps))
            .into_owned(),
    }
}

/// 对响应体做 URL 改写，支持 gzip/deflate/br 压缩体（解压 → 改写 → 按原编码重压）。
/// 非文本、编码未知或解压/重压失败时原样返回，保证不破坏响应。
pub fn rewrite_response_bytes(body: &[u8], encoding: &str, rule: &RewriteRule) -> Bytes {
    let enc = encoding.trim().to_ascii_lowercase();
    if enc.is_empty() || enc == "identity" {
        return match std::str::from_utf8(body) {
            Ok(text) => Bytes::from(rewrite_text(text, rule)),
            Err(_) => Bytes::from(body.to_vec()),
        };
    }
    let Some(plain) = decompress(&enc, body) else {
        return Bytes::from(body.to_vec());
    };
    let Ok(text) = std::str::from_utf8(&plain) else {
        return Bytes::from(body.to_vec());
    };
    let rewritten = rewrite_text(text, rule);
    match recompress(&enc, rewritten.as_bytes()) {
        Some(compressed) => Bytes::from(compressed),
        None => Bytes::from(body.to_vec()),
    }
}

/// 按内容类型选择改写：CSS 额外处理 `url(/path)` 根相对路径，其余走通用文本改写。
pub fn rewrite_response_bytes_for(
    body: &[u8],
    encoding: &str,
    content_type: &str,
    rule: &RewriteRule,
) -> Bytes {
    if content_type.to_ascii_lowercase().contains("text/css")
        && let Some(out) = rewrite_css_response_bytes(body, encoding, rule)
    {
        return out;
    }
    rewrite_response_bytes(body, encoding, rule)
}

fn decompress(encoding: &str, body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    match encoding {
        "gzip" => {
            let mut d = GzDecoder::new(body);
            d.read_to_end(&mut out).ok()?;
        }
        "deflate" => {
            // 规范里 deflate 是 zlib 包装；个别服务器发裸 deflate，回退再试
            let mut d = ZlibDecoder::new(body);
            if d.read_to_end(&mut out).is_err() {
                out.clear();
                let mut d = DeflateDecoder::new(body);
                d.read_to_end(&mut out).ok()?;
            }
        }
        "br" => {
            brotli::BrotliDecompress(&mut &body[..], &mut out).ok()?;
        }
        _ => return None,
    }
    Some(out)
}

fn recompress(encoding: &str, body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    match encoding {
        "gzip" => {
            let mut e = GzEncoder::new(&mut out, Compression::default());
            e.write_all(body).ok()?;
            e.finish().ok()?;
        }
        "deflate" => {
            let mut e = ZlibEncoder::new(&mut out, Compression::default());
            e.write_all(body).ok()?;
            e.finish().ok()?;
        }
        "br" => {
            let params = brotli::enc::BrotliEncoderParams {
                quality: 5,
                lgwin: 20,
                ..Default::default()
            };
            brotli::BrotliCompress(&mut &body[..], &mut out, &params).ok()?;
        }
        _ => return None,
    }
    Some(out)
}

fn replaced(rule: &RewriteRule, caps: &Captures) -> String {
    let host = &caps[2];
    if is_local_host(host) || is_namespace_host(host) {
        return caps[0].to_string();
    }
    let base = match rule {
        RewriteRule::Absolute { base } | RewriteRule::Prefix { base, .. } => base,
        RewriteRule::Relative => {
            let p = caps.get(4).map(|m| m.as_str()).unwrap_or("/");
            return if p.starts_with('/') {
                p.to_string()
            } else if p.starts_with('?') {
                // 无 path 直接带 query（http://host?x=1）：主机部分改为根路径，保留 query
                format!("/{p}")
            } else {
                "/".to_string()
            };
        }
        RewriteRule::None => return caps[0].to_string(),
    };
    if base_host(base).is_some_and(|bh| bh == host.to_ascii_lowercase()) {
        return caps[0].to_string();
    }
    let scheme = caps.get(1).map(|m| m.as_str()).unwrap_or("http");
    let port = caps.get(3).map(|m| m.as_str()).unwrap_or("");
    let path = caps.get(4).map(|m| m.as_str()).unwrap_or("/");
    let path = if path.starts_with('/') {
        path.to_string()
    } else if path.starts_with('?') {
        format!("/{path}")
    } else {
        "/".to_string()
    };
    match rule {
        RewriteRule::Absolute { base } => {
            format!(
                "{}/{}",
                base.trim_end_matches('/'),
                path.trim_start_matches('/')
            )
        }
        RewriteRule::Prefix { base, .. } => {
            let host_port = if port.is_empty() {
                host.to_string()
            } else {
                format!("{host}:{port}")
            };
            format!(
                "{}/{scheme}://{host_port}{path}",
                base.trim_end_matches('/')
            )
        }
        RewriteRule::Relative | RewriteRule::None => caps[0].to_string(),
    }
}

/// 协议相对 URL（`//host/path`）的前缀式改写：`{lead}{base}/{scheme}://{host}{:port}{path}`。
fn replaced_protocol_relative(rule: &RewriteRule, caps: &Captures) -> String {
    let lead = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let host = &caps[2];
    let RewriteRule::Prefix { base, scheme, .. } = rule else {
        return caps[0].to_string();
    };
    if is_local_host(host) || is_namespace_host(host) {
        return caps[0].to_string();
    }
    if base_host(base).is_some_and(|bh| bh == host.to_ascii_lowercase()) {
        return caps[0].to_string();
    }
    let port = caps.get(3).map(|m| m.as_str()).unwrap_or("");
    let host_port = if port.is_empty() {
        host.to_string()
    } else {
        format!("{host}:{port}")
    };
    let path = caps.get(4).map(|m| m.as_str()).unwrap_or("/");
    let path = if path.starts_with('/') {
        path.to_string()
    } else if path.starts_with('?') {
        format!("/{path}")
    } else {
        "/".to_string()
    };
    format!(
        "{lead}{}/{scheme}://{host_port}{path}",
        base.trim_end_matches('/')
    )
}

/// HTML 根相对路径（`/path`）的前缀式改写：`{lead}{base}/{scheme}://{origin}{path}{tail}`。
fn replaced_html_attr(rule: &RewriteRule, caps: &Captures) -> String {
    let lead = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let path = caps.get(2).map(|m| m.as_str()).unwrap_or("");
    let tail = caps.get(3).map(|m| m.as_str()).unwrap_or("");
    if path.starts_with("//") {
        return caps[0].to_string(); // 协议相对由 PROTOCOL_REL_RE 处理
    }
    let RewriteRule::Prefix {
        base,
        scheme,
        origin,
    } = rule
    else {
        return caps[0].to_string();
    };
    format!(
        "{lead}{}/{scheme}://{origin}{path}{tail}",
        base.trim_end_matches('/')
    )
}

/// CSS 响应改写：先走通用改写（绝对/协议相对/HTML 属性），再补 `url(/path)` 根相对路径
/// （CSS 文件按自身 origin 解析根路径）。仅前缀式规则生效，其余规则返回 None。
/// 返回 None 表示不是 CSS 改写规则（无需处理）。
pub fn rewrite_css_response_bytes(
    body: &[u8],
    encoding: &str,
    rule: &RewriteRule,
) -> Option<Bytes> {
    let RewriteRule::Prefix {
        base,
        scheme,
        origin,
    } = rule
    else {
        return None;
    };
    let enc = encoding.trim().to_ascii_lowercase();
    let plain: Vec<u8> = if enc.is_empty() || enc == "identity" {
        body.to_vec()
    } else {
        decompress(&enc, body)?
    };
    let Ok(text) = std::str::from_utf8(&plain) else {
        return Some(Bytes::from(body.to_vec()));
    };
    let general = rewrite_text(text, rule);
    let base = base.trim_end_matches('/');
    let rewritten = CSS_URL_RE
        .replace_all(&general, |caps: &Captures| {
            let lead = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let quote = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let path = caps.get(3).map(|m| m.as_str()).unwrap_or("");
            if path.starts_with("//") {
                return caps[0].to_string();
            }
            format!("{lead}{quote}{base}/{scheme}://{origin}{path}")
        })
        .into_owned();
    let out: Vec<u8> = if enc.is_empty() || enc == "identity" {
        rewritten.into_bytes()
    } else {
        recompress(&enc, rewritten.as_bytes())?
    };
    Some(Bytes::from(out))
}

/// 把响应 `Location` 头改写成“回到 tape 的前缀式地址”，供前缀式请求使用：
/// 否则客户端会按 Location 直连真实上游（302 等跳转直接绕过 tape，专网/离线环境断链）。
/// - 绝对地址 `https://host/path` → `{base}/https://host/path`
/// - 协议相对 `//host/path` → `{base}/{req_scheme}://host/path`
/// - 相对路径 `/path` → `{base}/{req_scheme}://{req_origin}/path`
///
/// `base` 形如 `http://<tape地址>:<端口>`（取自客户端请求的 Host 头）。
pub fn rewrite_location(value: &str, req_scheme: &str, req_origin: &str, base: &str) -> String {
    let v = value.trim();
    if v.is_empty() || v.starts_with('#') {
        return v.to_string();
    }
    let base = base.trim_end_matches('/');
    if let Some(rest) = v
        .strip_prefix("http://")
        .or_else(|| v.strip_prefix("https://"))
    {
        let host = rest.split(['/', ':']).next().unwrap_or(rest);
        if is_local_host(host) {
            return v.to_string();
        }
        return format!("{base}/{v}");
    }
    if let Some(rest) = v.strip_prefix("//") {
        let host = rest.split(['/', ':']).next().unwrap_or(rest);
        if is_local_host(host) {
            return v.to_string();
        }
        return format!("{base}/{req_scheme}://{rest}");
    }
    let path = if v.starts_with('/') {
        v.to_string()
    } else {
        format!("/{v}")
    };
    format!("{base}/{req_scheme}://{req_origin}{path}")
}

fn is_local_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h == "localhost" || h == "127.0.0.1" || h == "::1" || h == "[::1]" || h == "0.0.0.0"
}

/// XML 命名空间/DTD 标识符主机：`xmlns="http://www.w3.org/2000/svg"`、DOCTYPE 里的
/// `http://www.w3.org/Graphics/.../svg11.dtd` 等是标识符而非可导航地址，任何改写都会
/// 破坏 SVG/XML（浏览器将拒绝渲染）。实际可导航的网页 URL 不会落在 w3.org。
fn is_namespace_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("www.w3.org")
}

fn base_host(base: &str) -> Option<String> {
    let rest = base.split_once("://")?.1;
    rest.split(['/', ':'])
        .next()
        .map(|h| h.to_ascii_lowercase())
}

/// 提取文本中出现的全部绝对 http(s) URL（去重，保持出现顺序）。
pub fn extract_http_urls(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in URL_RE.captures_iter(text) {
        let url = cap[0].to_string();
        if seen.insert(url.clone()) {
            out.push(url);
        }
    }
    out
}

static REL_ASSET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(/(?:static|assets|img|images|upload|fonts|media|css|js)/[^\s"'<>(){};]*)"#)
        .expect("invalid relative asset regex")
});

/// 提取文本中根相对资源路径（HTML src/href、CSS url()、JSON 字符串等），
/// 仅匹配 /static|assets|img|images|upload|fonts|media|css|js/ 开头，去重。
pub fn extract_relative_asset_paths(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in REL_ASSET_RE.captures_iter(text) {
        let mut path = cap[1].to_string();
        while path.ends_with(['.', ',', ';']) {
            path.pop();
        }
        if !path.is_empty() && seen.insert(path.clone()) {
            out.push(path);
        }
    }
    out
}

/// 从完整 URL 提取 path 部分（不含 query），无 path 时返回 "/"。
pub fn url_path(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let with_path = after_scheme
        .split_once('/')
        .map(|(_, p)| format!("/{p}"))
        .unwrap_or_else(|| "/".to_string());
    with_path.split('?').next().unwrap_or("/").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_basic() {
        let out = rewrite_text(
            r#"{"url":"http://10.1.2.3:8080/api/user?id=1"}"#,
            &RewriteRule::Relative,
        );
        assert_eq!(out, r#"{"url":"/api/user?id=1"}"#);
    }

    #[test]
    fn relative_keeps_port_and_query() {
        let out = rewrite_text(
            "http://10.1.2.3:9090/img/a.png?x=1&y=2",
            &RewriteRule::Relative,
        );
        assert_eq!(out, "/img/a.png?x=1&y=2");
    }

    #[test]
    fn relative_no_path() {
        assert_eq!(
            rewrite_text("http://10.1.2.3:8080", &RewriteRule::Relative),
            "/"
        );
        assert_eq!(
            rewrite_text("https://10.1.2.3", &RewriteRule::Relative),
            "/"
        );
    }

    #[test]
    fn relative_keeps_query_without_path() {
        // 无 path 直接带 query 的形态：主机部分改为根路径，query 必须保留
        assert_eq!(
            rewrite_text("http://10.1.2.3:8080?x=1", &RewriteRule::Relative),
            "/?x=1"
        );
        let rule = RewriteRule::Prefix {
            base: "http://127.0.0.1:8888".to_string(),
            scheme: "http".to_string(),
            origin: "10.1.2.3:8080".to_string(),
        };
        assert_eq!(
            rewrite_text("http://10.1.2.3:8080?x=1", &rule),
            "http://127.0.0.1:8888/http://10.1.2.3:8080/?x=1"
        );
        // 提取 URL 也应保留无 path 的 query
        assert_eq!(
            extract_http_urls("http://10.1.2.3:8080?x=1"),
            vec!["http://10.1.2.3:8080?x=1"]
        );
    }

    #[test]
    fn relative_in_html() {
        let html = r#"<img src="http://10.1.2.3:8080/img/a.png"><a href="http://10.1.2.3:8080/page">p</a>"#;
        let out = rewrite_text(html, &RewriteRule::Relative);
        assert_eq!(out, r#"<img src="/img/a.png"><a href="/page">p</a>"#);
    }

    #[test]
    fn relative_in_css() {
        let css = "body { background: url(http://10.1.2.3:8080/img/bg.png); }";
        let out = rewrite_text(css, &RewriteRule::Relative);
        assert_eq!(out, "body { background: url(/img/bg.png); }");
    }

    #[test]
    fn absolute_mode() {
        let rule = RewriteRule::Absolute {
            base: "http://127.0.0.1:8080/".to_string(),
        };
        let out = rewrite_text("http://10.1.2.3:8080/api/user?id=1", &rule);
        assert_eq!(out, "http://127.0.0.1:8080/api/user?id=1");
    }

    #[test]
    fn absolute_mode_skips_same_host() {
        let rule = RewriteRule::Absolute {
            base: "http://127.0.0.1:8080/".to_string(),
        };
        let out = rewrite_text("http://127.0.0.1:8080/api/user?id=1", &rule);
        assert_eq!(out, "http://127.0.0.1:8080/api/user?id=1");
    }

    #[test]
    fn prefix_mode_rewrites_to_tape_url() {
        let rule = RewriteRule::Prefix {
            base: "http://127.0.0.1:8888".to_string(),
            scheme: "http".to_string(),
            origin: "10.1.2.3:8080".to_string(),
        };
        assert_eq!(
            rewrite_text("http://10.1.2.3:8080/api/user?id=1", &rule),
            "http://127.0.0.1:8888/http://10.1.2.3:8080/api/user?id=1"
        );
        assert_eq!(
            rewrite_text("https://api.company.com/v1/login", &rule),
            "http://127.0.0.1:8888/https://api.company.com/v1/login"
        );
        // 无端口、有 query
        assert_eq!(
            rewrite_text("https://www.test.com/", &rule),
            "http://127.0.0.1:8888/https://www.test.com/"
        );
        // 已经是 tape 自身地址或 localhost → 保持原样（防循环）
        assert_eq!(
            rewrite_text("http://127.0.0.1:8888/x", &rule),
            "http://127.0.0.1:8888/x"
        );
        assert_eq!(
            rewrite_text("http://localhost:8080/x", &rule),
            "http://localhost:8080/x"
        );
    }

    #[test]
    fn prefix_mode_rewrites_protocol_relative_links() {
        let rule = RewriteRule::Prefix {
            base: "http://127.0.0.1:8888".to_string(),
            scheme: "https".to_string(),
            origin: "www.dingtalk.com".to_string(),
        };
        // HTML src/href 里的协议相对链接（浏览器会解析成 http://g.alicdn.com 直连公网）
        assert_eq!(
            rewrite_text(
                r#"<script src="//g.alicdn.com/alilog/mlog/aplus_v2.js"></script>"#,
                &rule
            ),
            r#"<script src="http://127.0.0.1:8888/https://g.alicdn.com/alilog/mlog/aplus_v2.js"></script>"#
        );
        // CSS url()
        assert_eq!(
            rewrite_text("body{background:url(//img.alicdn.com/bg.png)}", &rule),
            "body{background:url(http://127.0.0.1:8888/https://img.alicdn.com/bg.png)}"
        );
        // 幂等：改写后的输出不应被再次改写
        let once = rewrite_text("src=\"//g.alicdn.com/a.js\"", &rule);
        assert_eq!(rewrite_text(&once, &rule), once);
        // scheme:// 里的 // 与 localhost 不应被误改写
        assert_eq!(
            rewrite_text("https://g.alicdn.com/x", &rule),
            "http://127.0.0.1:8888/https://g.alicdn.com/x"
        );
        assert_eq!(
            rewrite_text("src=\"//localhost/x.js\"", &rule),
            "src=\"//localhost/x.js\""
        );
    }

    #[test]
    fn prefix_mode_rewrites_root_relative_html_attrs() {
        let rule = RewriteRule::Prefix {
            base: "http://127.0.0.1:8888".to_string(),
            scheme: "https".to_string(),
            origin: "qwenwork.cn".to_string(),
        };
        let html = r#"<link rel="stylesheet" href="/assets/website/index.css"><img src="/img/a.png"><form action="/login"><a href="/">home</a>"#;
        let out = rewrite_text(html, &rule);
        assert!(
            out.contains(
                r#"href="http://127.0.0.1:8888/https://qwenwork.cn/assets/website/index.css""#
            ),
            "root-relative css 应改回 tape: {out}"
        );
        assert!(
            out.contains(r#"src="http://127.0.0.1:8888/https://qwenwork.cn/img/a.png""#),
            "root-relative img 应改回 tape: {out}"
        );
        assert!(
            out.contains(r#"action="http://127.0.0.1:8888/https://qwenwork.cn/login""#),
            "root-relative action 应改回 tape: {out}"
        );
        assert!(
            out.contains(r#"href="http://127.0.0.1:8888/https://qwenwork.cn/""#),
            "href=/ 应改回 tape: {out}"
        );
        // 协议相对与绝对地址不被此规则重复改写
        assert_eq!(
            rewrite_text(r#"<script src="//g.alicdn.com/a.js"></script>"#, &rule),
            r#"<script src="http://127.0.0.1:8888/https://g.alicdn.com/a.js"></script>"#
        );
        assert_eq!(
            rewrite_text(r#"<a href="https://www.test.com/x">x</a>"#, &rule),
            r#"<a href="http://127.0.0.1:8888/https://www.test.com/x">x</a>"#
        );
        // 幂等
        let once = rewrite_text(html, &rule);
        assert_eq!(rewrite_text(&once, &rule), once);
    }

    #[test]
    fn prefix_mode_rewrites_root_relative_css_urls() {
        use std::io::Write;

        let rule = RewriteRule::Prefix {
            base: "http://127.0.0.1:8888".to_string(),
            scheme: "https".to_string(),
            origin: "qwenwork.cn".to_string(),
        };
        let css = r#"@font-face{src:url(/fonts/Alimama.woff2)}.a{background:url('/img/bg.png')}"#;
        let out = rewrite_css_response_bytes(css.as_bytes(), "", &rule).unwrap();
        assert_eq!(
            String::from_utf8(out.to_vec()).unwrap(),
            r#"@font-face{src:url(http://127.0.0.1:8888/https://qwenwork.cn/fonts/Alimama.woff2)}.a{background:url('http://127.0.0.1:8888/https://qwenwork.cn/img/bg.png')}"#
        );
        // 相对 url(./assets/x) 不动（浏览器按 CSS 自身路径解析，前缀式下恰好能留对）
        let out = rewrite_css_response_bytes(b"url(./assets/x.ttf)", "", &rule).unwrap();
        assert_eq!(
            String::from_utf8(out.to_vec()).unwrap(),
            "url(./assets/x.ttf)"
        );
        // 非前缀规则返回 None
        assert!(rewrite_css_response_bytes(b"x", "", &RewriteRule::Relative).is_none());
        // gzip 压缩 CSS
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(b"url(/fonts/x.woff2)").unwrap();
        let gz = enc.finish().unwrap();
        let out = rewrite_css_response_bytes(&gz, "gzip", &rule).unwrap();
        let mut d = flate2::read::GzDecoder::new(&out[..]);
        let mut plain = String::new();
        d.read_to_string(&mut plain).unwrap();
        assert_eq!(
            plain,
            "url(http://127.0.0.1:8888/https://qwenwork.cn/fonts/x.woff2)"
        );
    }

    #[test]
    fn rewrite_location_forms() {
        let base = "http://127.0.0.1:8888";
        // 绝对地址 → 前缀式
        assert_eq!(
            rewrite_location(
                "https://www.dingtalk.com/",
                "https",
                "login.dingtalk.com",
                base
            ),
            "http://127.0.0.1:8888/https://www.dingtalk.com/"
        );
        // 协议相对 → 用请求前缀的 scheme
        assert_eq!(
            rewrite_location("//www.dingtalk.com/x", "https", "login.dingtalk.com", base),
            "http://127.0.0.1:8888/https://www.dingtalk.com/x"
        );
        // 相对路径 → 解析到请求目标
        assert_eq!(
            rewrite_location("/redirect", "https", "login.dingtalk.com", base),
            "http://127.0.0.1:8888/https://login.dingtalk.com/redirect"
        );
        // localhost → 原样
        assert_eq!(
            rewrite_location(
                "http://localhost:8080/x",
                "https",
                "login.dingtalk.com",
                base
            ),
            "http://localhost:8080/x"
        );
        assert_eq!(
            rewrite_location(
                "http://127.0.0.1:8888/x",
                "https",
                "login.dingtalk.com",
                base
            ),
            "http://127.0.0.1:8888/x"
        );
        // 空 / 锚点 → 原样
        assert_eq!(
            rewrite_location("", "https", "login.dingtalk.com", base),
            ""
        );
        assert_eq!(
            rewrite_location("#top", "https", "login.dingtalk.com", base),
            "#top"
        );
    }

    #[test]
    fn rewrite_compressed_body_gzip_and_br() {
        use std::io::Read;

        let rule = RewriteRule::Prefix {
            base: "http://127.0.0.1:8888".to_string(),
            scheme: "http".to_string(),
            origin: "10.1.2.3:8080".to_string(),
        };
        let html = r#"<script src="//img.alicdn.com/a.js"></script>"#;

        // gzip：解压 → 改写 → 仍为合法 gzip
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(html.as_bytes()).unwrap();
        let gz = enc.finish().unwrap();
        let out = rewrite_response_bytes(&gz, "gzip", &rule);
        let mut d = flate2::read::GzDecoder::new(&out[..]);
        let mut plain = String::new();
        d.read_to_string(&mut plain).unwrap();
        assert_eq!(
            plain,
            r#"<script src="http://127.0.0.1:8888/http://img.alicdn.com/a.js"></script>"#
        );

        // brotli
        let mut br_out = Vec::new();
        let params = brotli::enc::BrotliEncoderParams {
            quality: 5,
            lgwin: 20,
            ..Default::default()
        };
        brotli::BrotliCompress(&mut &html.as_bytes()[..], &mut br_out, &params).unwrap();
        let out = rewrite_response_bytes(&br_out, "br", &rule);
        let mut plain = Vec::new();
        brotli::BrotliDecompress(&mut &out[..], &mut plain).unwrap();
        assert_eq!(
            String::from_utf8(plain).unwrap(),
            r#"<script src="http://127.0.0.1:8888/http://img.alicdn.com/a.js"></script>"#
        );

        // 二进制体（非文本）→ 原样返回
        let binary = [0u8, 159, 146, 150, 0, 1, 2, 3];
        assert_eq!(
            rewrite_response_bytes(&binary, "", &rule),
            Bytes::from(binary.to_vec())
        );
    }

    #[test]
    fn prefix_mode_keeps_xml_namespaces_intact() {
        let rule = RewriteRule::Prefix {
            base: "http://127.0.0.1:8888".to_string(),
            scheme: "https".to_string(),
            origin: "img.alicdn.com".to_string(),
        };
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><image xlink:href="https://img.alicdn.com/a.png"/></svg>"#;
        let out = rewrite_text(svg, &rule);
        // 命名空间声明保持原样（否则 SVG 变成非法 XML 无法渲染）
        assert!(
            out.contains(r#"xmlns="http://www.w3.org/2000/svg""#),
            "SVG 命名空间不应被改写: {out}"
        );
        assert!(
            out.contains(r#"xmlns:xlink="http://www.w3.org/1999/xlink""#),
            "xlink 命名空间不应被改写: {out}"
        );
        // 真正的图片引用照常改写
        assert!(
            out.contains(r#"xlink:href="http://127.0.0.1:8888/https://img.alicdn.com/a.png""#),
            "SVG 内图片引用应改写回 tape: {out}"
        );
        // DOCTYPE DTD 标识符同样保留
        let dtd = r#"<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd">"#;
        assert_eq!(rewrite_text(dtd, &rule), dtd);
    }

    #[test]
    fn none_mode_keeps_original() {
        let input = "http://10.1.2.3:8080/api/user?id=1";
        assert_eq!(rewrite_text(input, &RewriteRule::None), input);
    }

    #[test]
    fn idempotent_for_local_hosts() {
        for url in [
            "http://localhost:8080/x",
            "http://127.0.0.1/x",
            "http://127.0.0.1:1/x",
        ] {
            assert_eq!(rewrite_text(url, &RewriteRule::Relative), url);
        }
    }

    #[test]
    fn extract_collects_unique_urls() {
        let text =
            r#"{"a":"http://10.1.2.3:8080/x","b":"http://10.1.2.3:8080/x","c":"https://a.b/c"}"#;
        let urls = extract_http_urls(text);
        assert_eq!(urls, vec!["http://10.1.2.3:8080/x", "https://a.b/c"]);
    }

    #[test]
    fn url_path_parsing() {
        assert_eq!(url_path("http://10.1.2.3:8080/img/a.png?x=1"), "/img/a.png");
        assert_eq!(url_path("http://10.1.2.3:8080"), "/");
    }

    #[test]
    fn extract_relative_asset_paths_finds_html_css_json() {
        let html = r#"<link href="/static/css/app.css"><img src="/img/a.png"><script src="/assets/js/app.js"></script>"#;
        let css = "body { background: url(/img/bg.png); } .x { font: url('/fonts/a.woff2'); }";
        let json = r#"{"avatar":"/images/u/1.png"}"#;
        let mut all = extract_relative_asset_paths(&format!("{html} {css} {json}"));
        all.sort();
        assert_eq!(
            all,
            vec![
                "/assets/js/app.js",
                "/fonts/a.woff2",
                "/images/u/1.png",
                "/img/a.png",
                "/img/bg.png",
                "/static/css/app.css"
            ]
        );
    }

    #[test]
    fn relative_asset_extraction_ignores_api_paths() {
        assert!(extract_relative_asset_paths(r#"{"a":"/api/user"}"#).is_empty());
    }
}
