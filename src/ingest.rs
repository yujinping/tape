//! 通用 HTTP 日志接收组件：启动一个 HTTP 服务，接收 GET / POST 推送的调试日志，
//! 终端实时打印并自动落盘 `{log-dir}/{prefix}-YYYYMMDD-HHMMSSmmm.log`。
//!
//! 被多个子命令复用，仅 `prefix` 不同（见 `tape console` / `tape app`）：
//! - `console-`：盒子 WebView / 网页 console 日志；
//! - `app-`：无法使用 logcat 的盒子应用网络日志。
//!
//! 协议：
//! - GET  `/?msg=...&level=warn&tag=...&url=...&line=12`
//! - POST `Content-Type: text/plain`（整段纯文本，多行按行记）
//! - POST `Content-Type: application/x-www-form-urlencoded`（同 GET 参数）
//! - POST `Content-Type: application/json`：`{"level","message","tag","url","line"}` 对象或数组
//!
//! 所有响应带 CORS 头（跨域推送必需），OPTIONS 预检返回 204。

use std::io::{IsTerminal, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

use crate::log_file;

/// 一条待落盘的日志。
#[derive(Debug, Clone)]
pub struct RawEntry {
    pub level: String,
    pub tag: String,
    pub message: String,
}

/// 通用接收服务参数。
pub struct IngestParams {
    pub port: u16,
    pub log_dir: PathBuf,
    /// 落盘文件前缀（`console` / `app`），文件名 `{prefix}-YYYYMMDD-HHMMSSmmm.log`。
    pub prefix: &'static str,
    pub no_color: bool,
}

/// 接收服务共享状态。
pub struct IngestState {
    prefix: &'static str,
    log_path: PathBuf,
    /// 落盘文件（写失败后置 None，降级为仅终端打印）。
    file: Mutex<Option<std::fs::File>>,
    color: bool,
}

impl IngestState {
    fn new(log_dir: &std::path::Path, prefix: &'static str, color: bool) -> Result<Arc<Self>> {
        std::fs::create_dir_all(log_dir)
            .with_context(|| format!("无法创建日志目录 {}", log_dir.display()))?;
        let log_path = log_file::path(log_dir, prefix);
        let file = match std::fs::File::create(&log_path) {
            Ok(f) => Some(f),
            Err(e) => {
                warn!(
                    "日志落盘失败 {}: {}（继续接收，仅终端打印）",
                    log_path.display(),
                    e
                );
                None
            }
        };
        Ok(Arc::new(Self {
            prefix,
            log_path,
            file: Mutex::new(file),
            color,
        }))
    }
}

/// 启动 HTTP 接收服务，Ctrl-C 优雅停止。
pub async fn run(params: IngestParams) -> Result<()> {
    let state = IngestState::new(
        &params.log_dir,
        params.prefix,
        !params.no_color && std::io::stdout().is_terminal(),
    )?;
    let listener = TcpListener::bind(("0.0.0.0", params.port)).await?;
    info!(
        "tape {} 已启动: 0.0.0.0:{} （接收 GET/POST 调试日志，落盘 {}）",
        params.prefix,
        params.port,
        state.log_path.display()
    );
    accept_loop(listener, state).await
}

async fn accept_loop(listener: TcpListener, state: Arc<IngestState>) -> Result<()> {
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, addr) = accepted?;
                let state = state.clone();
                let prefix = state.prefix;
                tokio::spawn(async move {
                    if let Err(e) = serve_conn(state, stream, addr).await {
                        warn!("{} 连接处理失败: {e}", prefix);
                    }
                });
            }
            _ = tokio::signal::ctrl_c() => {
                info!("收到 Ctrl-C，退出（日志已保存到 {}）", state.log_path.display());
                break;
            }
        }
    }
    Ok(())
}

async fn serve_conn(state: Arc<IngestState>, stream: TcpStream, _addr: SocketAddr) -> Result<()> {
    let io = TokioIo::new(stream);
    let service = service_fn(move |req| {
        let state = state.clone();
        async move { Ok::<_, std::convert::Infallible>(handle_request(state, req).await) }
    });
    http1::Builder::new()
        .keep_alive(true)
        .serve_connection(io, service)
        .await
        .map_err(|e| anyhow::anyhow!("http 服务错误: {e}"))?;
    Ok(())
}

async fn handle_request(state: Arc<IngestState>, req: Request<Incoming>) -> Response<Full<Bytes>> {
    match *req.method() {
        Method::OPTIONS => response(StatusCode::NO_CONTENT, String::new()),
        Method::GET => {
            let entries = entries_from_query(req.uri().query().unwrap_or(""));
            let n = write_entries(&state, &entries);
            response(StatusCode::OK, format!("ok: {n} line(s) recorded\n"))
        }
        Method::POST => {
            let content_type = req
                .headers()
                .get(hyper::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            let body = req
                .collect()
                .await
                .map(|b| b.to_bytes())
                .unwrap_or_else(|_| Bytes::new());
            let entries = entries_from_body(&content_type, &body);
            let n = write_entries(&state, &entries);
            response(StatusCode::OK, format!("ok: {n} line(s) recorded\n"))
        }
        _ => response(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed\n".to_string(),
        ),
    }
}

/// 打印 + 落盘一批日志，返回条数。
fn write_entries(state: &IngestState, entries: &[RawEntry]) -> usize {
    let mut stdout = std::io::stdout().lock();
    let mut file = state.file.lock().expect("ingest file lock poisoned");
    let mut n = 0;
    for entry in entries {
        let ts = now_ts();
        let line = format_line(&ts, entry);
        if state.color {
            let colored = line.replacen(
                &format!("[{}]", entry.level),
                &format!("[{}{}\x1b[0m]", level_color(&entry.level), entry.level),
                1,
            );
            let _ = writeln!(stdout, "{}", colored);
        } else {
            let _ = writeln!(stdout, "{}", line);
        }
        if let Some(f) = file.as_mut()
            && let Err(e) = writeln!(f, "{}", line)
        {
            warn!("写入日志文件失败: {}（停止落盘，仅终端打印）", e);
            *file = None;
        }
        n += 1;
    }
    if let Some(f) = file.as_mut() {
        let _ = f.flush();
    }
    n
}

/// 落盘 / 无颜色终端的行格式：`[YYYY-MM-DD HH:MM:SS] [level] [tag] message`。
pub fn format_line(ts: &str, entry: &RawEntry) -> String {
    format!(
        "{} [{}] {}{}",
        ts,
        entry.level,
        render_tag(&entry.tag),
        entry.message
    )
}

/// tag 段：有 tag 用 tag；否则有 url 用 `url:line`；都没有则空串。
pub fn compose_tag(tag: &str, url: &str, line: Option<u32>) -> String {
    if !tag.is_empty() {
        tag.to_string()
    } else if !url.is_empty() {
        match line {
            Some(l) => format!("{url}:{l}"),
            None => url.to_string(),
        }
    } else {
        String::new()
    }
}

fn render_tag(tag: &str) -> String {
    if tag.is_empty() {
        String::new()
    } else {
        format!("[{tag}] ")
    }
}

fn level_color(level: &str) -> &'static str {
    match level.to_ascii_lowercase().as_str() {
        "debug" | "verbose" => "\x1b[32m", // 绿
        "log" | "info" => "\x1b[34m",      // 蓝
        "warn" | "warning" => "\x1b[33m",  // 黄
        "error" | "fatal" => "\x1b[31m",   // 红
        _ => "",
    }
}

/// 本地时间 `YYYY-MM-DD HH:MM:SS`（失败回退空串）。
fn now_ts() -> String {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    now.format(&time::macros::format_description!(
        "[year]-[month]-[day] [hour]:[minute]:[second]"
    ))
    .unwrap_or_default()
}

/// 从 GET query 解析参数（`msg` / `level` / `tag` / `url` / `line`）。
pub fn entries_from_query(query: &str) -> Vec<RawEntry> {
    let mut msg = String::new();
    let mut level = String::new();
    let mut tag = String::new();
    let mut url = String::new();
    let mut line = None;
    for (k, v) in parse_params(query) {
        match k.as_str() {
            "msg" | "message" | "text" | "log" => msg = v,
            "level" | "l" => level = v,
            "tag" | "t" => tag = v,
            "url" | "page" | "src" => url = v,
            "line" | "ln" => line = v.parse().ok(),
            _ => {}
        }
    }
    entries_from_parts(&msg, &level, &tag, &url, line)
}

/// 从 POST body 解析：按 Content-Type 分发（json / form / 纯文本）。
pub fn entries_from_body(content_type: &str, body: &[u8]) -> Vec<RawEntry> {
    let text = std::str::from_utf8(body).unwrap_or("");
    if content_type.contains("application/json") {
        return match serde_json::from_str::<serde_json::Value>(text) {
            Ok(v) => entries_from_json(&v),
            Err(e) => vec![RawEntry {
                level: "error".into(),
                tag: "tape".into(),
                message: format!("JSON 解析失败: {e}: {text}"),
            }],
        };
    }
    if content_type.contains("application/x-www-form-urlencoded") {
        return entries_from_query(text);
    }
    // text/plain 或未知类型：整段按行记录
    entries_from_parts(text, "", "", "", None)
}

/// 从 JSON（对象或对象数组）解析。
pub fn entries_from_json(v: &serde_json::Value) -> Vec<RawEntry> {
    match v {
        serde_json::Value::Array(items) => items.iter().flat_map(entries_from_json).collect(),
        serde_json::Value::Object(map) => {
            let get = |k: &str| map.get(k).and_then(|v| v.as_str()).unwrap_or("");
            let level = get("level").to_string();
            let message = {
                let m = get("message");
                let msg = get("msg");
                if !m.is_empty() {
                    m.to_string()
                } else {
                    msg.to_string()
                }
            };
            let tag = get("tag").to_string();
            let url = get("url").to_string();
            let line = map.get("line").and_then(|v| v.as_u64()).map(|n| n as u32);
            entries_from_parts(&message, &level, &tag, &url, line)
        }
        _ => Vec::new(),
    }
}

/// 由原始字段组装条目（空 message 丢弃；level 归一化小写）。
fn entries_from_parts(
    msg: &str,
    level: &str,
    tag: &str,
    url: &str,
    line: Option<u32>,
) -> Vec<RawEntry> {
    let level = {
        let l = level.trim().to_ascii_lowercase();
        if l.is_empty() { "log".to_string() } else { l }
    };
    msg.lines()
        .map(|m| RawEntry {
            level: level.clone(),
            tag: compose_tag(tag, url, line),
            message: m.to_string(),
        })
        .filter(|e| !e.message.is_empty())
        .collect()
}

/// 解析 `k=v&k2=v2` 参数（percent 解码，`+` 视为空格）。
fn parse_params(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            Some((percent_decode(k), percent_decode(v)))
        })
        .collect()
}

/// 简单 percent 解码（`%XX` → 字节；`+` → 空格）。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_val(bytes[i + 1]);
                let lo = hex_val(bytes[i + 2]);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push(hi * 16 + lo);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
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

/// 构造带 CORS 头的响应。
fn response(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    let mut resp = Response::new(Full::new(Bytes::from(body)));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        "text/plain; charset=utf-8".parse().unwrap(),
    );
    resp.headers_mut().insert(
        hyper::header::HeaderName::from_static("access-control-allow-origin"),
        hyper::header::HeaderValue::from_static("*"),
    );
    resp.headers_mut().insert(
        hyper::header::HeaderName::from_static("access-control-allow-methods"),
        hyper::header::HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    resp.headers_mut().insert(
        hyper::header::HeaderName::from_static("access-control-allow-headers"),
        hyper::header::HeaderValue::from_static("content-type"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_simple() {
        let entries = entries_from_query("msg=hello%20world&level=warn&tag=page");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, "warn");
        assert_eq!(entries[0].tag, "page");
        assert_eq!(entries[0].message, "hello world");
    }

    #[test]
    fn query_with_url_line_and_plus() {
        let entries = entries_from_query("msg=a+b&url=https://x.com/p&line=12");
        assert_eq!(entries[0].message, "a b");
        assert_eq!(entries[0].tag, "https://x.com/p:12");
    }

    #[test]
    fn query_multiline_msg_splits_lines() {
        let entries = entries_from_query("msg=line1%0Aline2");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].message, "line1");
        assert_eq!(entries[1].message, "line2");
    }

    #[test]
    fn plain_text_body_multiline() {
        let entries = entries_from_body("text/plain", b"first\nsecond\n");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].level, "log");
    }

    #[test]
    fn form_body() {
        let entries = entries_from_body(
            "application/x-www-form-urlencoded",
            b"msg=hi&level=error&tag=t1",
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, "error");
        assert_eq!(entries[0].tag, "t1");
    }

    #[test]
    fn json_object_and_msg_alias() {
        let entries = entries_from_body(
            "application/json",
            br#"{"msg":"hi","level":"debug","url":"https://a/b","line":3}"#,
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, "debug");
        assert_eq!(entries[0].tag, "https://a/b:3");
        assert_eq!(entries[0].message, "hi");
    }

    #[test]
    fn json_array() {
        let entries = entries_from_body(
            "application/json",
            br#"[{"message":"a"},{"message":"b","level":"error"}]"#,
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].level, "error");
    }

    #[test]
    fn json_invalid_falls_back_to_error_entry() {
        let entries = entries_from_body("application/json", b"not-json");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, "error");
    }

    #[test]
    fn empty_message_is_dropped() {
        assert!(entries_from_query("msg=&level=info").is_empty());
    }

    #[test]
    fn format_line_shape() {
        let entry = RawEntry {
            level: "warn".into(),
            tag: "page".into(),
            message: "boom".into(),
        };
        assert_eq!(
            format_line("2026-08-02 15:44:41", &entry),
            "2026-08-02 15:44:41 [warn] [page] boom"
        );
    }
}
