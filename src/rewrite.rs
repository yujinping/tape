use std::sync::LazyLock;

use regex::{Captures, Regex};

static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\bhttps?://([a-z0-9.\-]+)(?::(\d{1,5}))?((?:/[^\s"'<>(){}]*)?)"#)
        .expect("invalid URL regex")
});

/// 响应改写规则。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewriteRule {
    /// 改写为相对路径（推荐）
    Relative,
    /// 改写为绝对地址（拼接到 base 后）
    Absolute { base: String },
    /// 不改写
    None,
}

/// 对文本响应做全局 URL 改写，保持其余字节不变。
pub fn rewrite_text(input: &str, rule: &RewriteRule) -> String {
    match rule {
        RewriteRule::None => input.to_string(),
        _ => URL_RE
            .replace_all(input, |caps: &Captures| replaced(rule, caps))
            .into_owned(),
    }
}

fn replaced(rule: &RewriteRule, caps: &Captures) -> String {
    let host = &caps[1];
    if is_local_host(host) {
        return caps[0].to_string();
    }
    if let RewriteRule::Absolute { base } = rule
        && base_host(base).is_some_and(|bh| bh == host.to_ascii_lowercase())
    {
        return caps[0].to_string();
    }
    let path = caps
        .get(3)
        .map(|m| m.as_str())
        .filter(|p| p.starts_with('/'))
        .unwrap_or("/");
    match rule {
        RewriteRule::Relative => path.to_string(),
        RewriteRule::Absolute { base } => {
            format!(
                "{}/{}",
                base.trim_end_matches('/'),
                path.trim_start_matches('/')
            )
        }
        RewriteRule::None => caps[0].to_string(),
    }
}

fn is_local_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h == "localhost" || h == "127.0.0.1" || h == "::1" || h == "[::1]" || h == "0.0.0.0"
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
