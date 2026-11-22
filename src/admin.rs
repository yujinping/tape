//! 管理界面：本机浏览器里的控制台。
//!
//! 设计上刻意保持「零侵入」：管理界面不接管录制/回放的运行时状态，而是把
//! `tape record` / `tape replay` 当作子进程拉起，采集它们的 stdout 当日志。
//! 这样录制路径不需要为界面做任何热加载或状态共享，界面崩了也不影响录制。

use std::collections::VecDeque;
use std::convert::Infallible;
use std::io::{BufRead, BufReader};
use std::net::{IpAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tracing::warn;

use crate::cli::AdminArgs;
use crate::config::{
    CONFIG_FILE_NAME, RawConfig, ReplaySection, TransparentConfig, TransparentSection,
    read_raw_config, save_raw_config,
};
use crate::snapshot::{Snapshot, decode_body};
use crate::store;

const MAX_LOG_LINES: usize = 2000;
const MAX_JSON_BODY: usize = 1024 * 1024;

const INDEX_HTML: &str = include_str!("../web/dist/index.html");
const APP_JS: &str = include_str!("../web/dist/assets/app.js");
const APP_CSS: &str = include_str!("../web/dist/assets/app.css");

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Record,
    Replay,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Record => "record",
            Mode::Replay => "replay",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Mode::Record => "录制",
            Mode::Replay => "回放",
        }
    }
}

struct Running {
    mode: Mode,
    child: Child,
    port: u16,
    started: Instant,
    started_at: String,
}

struct Cache {
    stamp: Option<SystemTime>,
    snaps: Arc<Vec<Snapshot>>,
}

#[derive(Serialize, Clone)]
pub struct LogLine {
    at: String,
    level: String,
    text: String,
}

pub struct AdminState {
    dir: PathBuf,
    record_port: u16,
    running: Mutex<Option<Running>>,
    logs: Mutex<VecDeque<LogLine>>,
    cache: Mutex<Option<Cache>>,
    last_error: Mutex<Option<String>>,
    config_path: PathBuf,
    started: Instant,
}

impl AdminState {
    fn new(dir: PathBuf, record_port: u16) -> Self {
        let config_path = dir.join(CONFIG_FILE_NAME);
        Self {
            dir,
            record_port,
            running: Mutex::new(None),
            logs: Mutex::new(VecDeque::new()),
            cache: Mutex::new(None),
            last_error: Mutex::new(None),
            config_path,
            started: Instant::now(),
        }
    }

    fn push_log(&self, text: String) {
        let text = strip_ansi(&text);
        if text.trim().is_empty() {
            return;
        }
        let level = if text.contains("ERROR") || text.contains("WARN") {
            if text.contains("ERROR") {
                "ERROR"
            } else {
                "WARN"
            }
        } else {
            "INFO"
        };
        let at = clock_now();
        let mut logs = self.logs.lock().unwrap();
        logs.push_back(LogLine {
            at,
            level: level.to_string(),
            text,
        });
        while logs.len() > MAX_LOG_LINES {
            logs.pop_front();
        }
    }

    /// 快照缓存：以 `session.json` 的修改时间作为失效信号。
    ///
    /// 录制进程每写一条快照都会更新它，因此比扫描目录更可靠也更便宜。
    fn snapshots(&self) -> Result<Arc<Vec<Snapshot>>> {
        let stamp = std::fs::metadata(self.dir.join("session.json"))
            .and_then(|m| m.modified())
            .ok();
        if let Some(cache) = self.cache.lock().unwrap().as_ref()
            && cache.stamp == stamp
        {
            return Ok(cache.snaps.clone());
        }
        let snaps = Arc::new(store::load_snapshots(&self.dir).unwrap_or_default());
        *self.cache.lock().unwrap() = Some(Cache {
            stamp,
            snaps: snaps.clone(),
        });
        Ok(snaps)
    }

    fn running_snapshot(&self) -> Option<(Mode, u16, u64, String)> {
        let mut slot = self.running.lock().unwrap();
        let run = slot.as_mut()?;
        match run.child.try_wait() {
            Ok(Some(status)) => {
                let message = format!(
                    "{}进程已退出（{}），请查看日志",
                    run.mode.label(),
                    status
                        .code()
                        .map(|c| format!("退出码 {c}"))
                        .unwrap_or_else(|| "被终止".to_string())
                );
                warn!("{message}");
                *self.last_error.lock().unwrap() = Some(message);
                *slot = None;
                None
            }
            Ok(None) => Some((
                run.mode,
                run.port,
                run.started.elapsed().as_secs(),
                run.started_at.clone(),
            )),
            Err(_) => None,
        }
    }
}

impl Drop for AdminState {
    fn drop(&mut self) {
        if let Some(mut run) = self.running.lock().unwrap().take() {
            let _ = run.child.kill();
            let _ = run.child.wait();
        }
    }
}

pub async fn run(args: AdminArgs) -> Result<()> {
    std::fs::create_dir_all(&args.dir)
        .with_context(|| format!("无法创建数据目录 {}", args.dir.display()))?;
    let state = Arc::new(AdminState::new(args.dir.clone(), args.record_port));

    let addr = (args.bind, args.port);
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("管理界面端口被占用或无权监听: {addr:?}"))?;
    let actual = listener.local_addr()?;
    let url = format!("http://{}:{}/", display_host(args.bind), actual.port());

    print_banner(&url, &args.dir, args.record_port);

    if !args.no_open
        && let Err(e) = open_browser(&url)
    {
        warn!("无法自动打开浏览器（可手动访问 {url}）: {e}");
    }

    loop {
        tokio::select! {
            _ = shutdown_signal() => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let state = state.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |req| handle(req, state.clone()));
                    if let Err(e) = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, service)
                        .await
                    {
                        tracing::debug!("管理界面连接结束: {e}");
                    }
                });
            }
        }
    }

    if let Some(mut run) = state.running.lock().unwrap().take() {
        let _ = run.child.kill();
        let _ = run.child.wait();
    }
    Ok(())
}

fn display_host(bind: IpAddr) -> String {
    match bind {
        IpAddr::V4(v4) if v4.is_unspecified() => "127.0.0.1".to_string(),
        IpAddr::V6(v6) if v6.is_unspecified() => "127.0.0.1".to_string(),
        other => other.to_string(),
    }
}

fn print_banner(url: &str, dir: &Path, record_port: u16) {
    println!();
    println!("  tape 控制台  {url}");
    println!("  数据目录    {}", dir.display());
    println!("  录制端口    {record_port}");
    println!("  按 Ctrl+C 退出（录制/回放子进程会一起结束）");
    println!();
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("启动浏览器失败")?;
    Ok(())
}

async fn handle(
    req: Request<Incoming>,
    state: Arc<AdminState>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(match route(req, state).await {
        Ok(resp) => resp,
        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    })
}

async fn route(req: Request<Incoming>, state: Arc<AdminState>) -> Result<Response<Full<Bytes>>> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();

    match (method.clone(), path.as_str()) {
        (Method::GET, "/" | "/index.html") => Ok(html_response(INDEX_HTML)),
        (Method::GET, "/assets/app.js") => Ok(bytes_response(
            "application/javascript; charset=utf-8",
            APP_JS.as_bytes(),
        )),
        (Method::GET, "/assets/app.css") => Ok(bytes_response(
            "text/css; charset=utf-8",
            APP_CSS.as_bytes(),
        )),
        (Method::GET, "/favicon.ico") => Ok(Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Full::new(Bytes::new()))?),
        (Method::GET, "/api/status") => Ok(json_response(status_json(&state))),
        (Method::GET, "/api/snapshots") => Ok(json_response(list_snapshots(&state)?)),
        (Method::GET, "/api/logs") => {
            let limit = query_param(&query, "limit")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(400)
                .min(MAX_LOG_LINES);
            let logs = state.logs.lock().unwrap();
            let skip = logs.len().saturating_sub(limit);
            Ok(json_response(json!(
                logs.iter().skip(skip).cloned().collect::<Vec<_>>()
            )))
        }
        (Method::GET, "/api/config") => Ok(json_response(config_json(&state)?)),
        (Method::PUT, "/api/config") => {
            let raw: RawConfig = read_json(req).await?;
            save_raw_config(&state.config_path, &raw)?;
            *state.last_error.lock().unwrap() = None;
            state.push_log(format!("配置已保存：{}", state.config_path.display()));
            Ok(json_response(json!({ "ok": true })))
        }
        (Method::POST, "/api/record/start") => {
            let body: RecordStart = read_json(req).await.unwrap_or_default();
            if let Err(e) = apply_record_options(&state, &body) {
                return Ok(error_response(StatusCode::BAD_REQUEST, &format!("{e:#}")));
            }
            if let Err(e) = start_child(&state, Mode::Record, body.port) {
                return Ok(error_response(StatusCode::BAD_REQUEST, &format!("{e:#}")));
            }
            Ok(json_response(json!({ "ok": true })))
        }
        (Method::POST, "/api/record/stop") => {
            stop_child(&state, Mode::Record)?;
            Ok(json_response(json!({ "ok": true })))
        }
        (Method::POST, "/api/replay/start") => {
            let body: ReplayStart = read_json(req).await.unwrap_or_default();
            if let Err(e) = apply_replay_options(&state, &body) {
                return Ok(error_response(StatusCode::BAD_REQUEST, &format!("{e:#}")));
            }
            if let Err(e) = start_child(&state, Mode::Replay, body.port) {
                return Ok(error_response(StatusCode::BAD_REQUEST, &format!("{e:#}")));
            }
            Ok(json_response(json!({ "ok": true })))
        }
        (Method::POST, "/api/replay/stop") => {
            stop_child(&state, Mode::Replay)?;
            Ok(json_response(json!({ "ok": true })))
        }
        (Method::GET, p) if p.starts_with("/api/snapshots/") => {
            let rest = &p["/api/snapshots/".len()..];
            if let Some(id) = rest.strip_suffix("/body") {
                let side = query_param(&query, "side").unwrap_or_else(|| "response".to_string());
                return snapshot_body(&state, id, &side);
            }
            snapshot_detail(&state, rest)
        }
        _ => Ok(error_response(StatusCode::NOT_FOUND, "没有这个接口")),
    }
}

#[derive(Deserialize, Default)]
struct RecordStart {
    port: Option<u16>,
    /// 界面里的透明模式设置；缺省表示不改配置文件。
    transparent: Option<TransparentStart>,
}

#[derive(Deserialize, Default)]
struct TransparentStart {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    allow_ports: Vec<String>,
    #[serde(default)]
    allow_hosts: Vec<String>,
}

#[derive(Deserialize, Default)]
struct ReplayStart {
    port: Option<u16>,
    rewrite: Option<String>,
    absolute_base: Option<String>,
}

/// 把界面选择的透明模式写回配置文件，再由子进程按配置文件启动。
///
/// 这样「界面里看到的」与「实际生效的」是同一份配置，重启后仍然记得上次选择。
fn apply_record_options(state: &AdminState, body: &RecordStart) -> Result<()> {
    let Some(t) = &body.transparent else {
        return Ok(());
    };
    let mut raw = read_raw_config(&state.config_path).unwrap_or_default();
    let section = TransparentSection {
        enabled: t.enabled,
        allow_ports: clean_list(&t.allow_ports),
        allow_hosts: clean_list(&t.allow_hosts),
    };
    if section.enabled {
        TransparentConfig::parse(
            &section.allow_ports,
            &section.allow_hosts,
            "0.0.0.0".parse().expect("固定地址合法"),
            state.record_port,
        )
        .context("透明模式配置无效")?;
    }
    raw.transparent = Some(section);
    save_raw_config(&state.config_path, &raw)?;
    Ok(())
}

/// 回放选项同样落配置，命令行只传端口与数据目录。
fn apply_replay_options(state: &AdminState, body: &ReplayStart) -> Result<()> {
    if body.rewrite.is_none() && body.absolute_base.is_none() {
        return Ok(());
    }
    let mut raw = read_raw_config(&state.config_path).unwrap_or_default();
    let mut section: ReplaySection = raw.replay.take().unwrap_or_default();
    if let Some(rewrite) = &body.rewrite {
        if !matches!(rewrite.as_str(), "relative" | "absolute" | "none") {
            bail!("未知改写模式：{rewrite}（可选 relative / absolute / none）");
        }
        section.rewrite = Some(rewrite.clone());
    }
    if let Some(base) = &body.absolute_base {
        let base = base.trim();
        section.absolute_base = if base.is_empty() {
            None
        } else {
            if !base.starts_with("http://") && !base.starts_with("https://") {
                bail!("绝对基地址需以 http:// 或 https:// 开头");
            }
            Some(base.to_string())
        };
    }
    raw.replay = Some(section);
    save_raw_config(&state.config_path, &raw)?;
    Ok(())
}

fn clean_list(items: &[String]) -> Vec<String> {
    items
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn status_json(state: &AdminState) -> Value {
    let running = state.running_snapshot();
    let snaps = state.snapshots().unwrap_or_default();

    let bytes: usize = snaps
        .iter()
        .map(|s| s.request.body.len() + s.response.body.len())
        .sum();
    let origins = {
        let mut list: Vec<&str> = snaps.iter().map(|s| s.origin.as_str()).collect();
        list.sort_unstable();
        list.dedup();
        list.len()
    };

    let transparent = read_raw_config(&state.config_path)
        .ok()
        .and_then(|cfg| cfg.transparent)
        .map(|section| {
            let ports = if section.enabled {
                TransparentConfig::parse(
                    &section.allow_ports,
                    &section.allow_hosts,
                    "0.0.0.0".parse().expect("固定地址合法"),
                    state.record_port,
                )
                .map(|c| c.allowed_ports().to_vec())
                .unwrap_or_default()
            } else {
                Vec::new()
            };
            json!({ "enabled": section.enabled, "ports": ports })
        });

    let last_error = state.last_error.lock().unwrap().clone();
    let (mode, port, uptime, since) = match &running {
        Some((mode, port, uptime, since)) => (
            Value::String(mode.as_str().to_string()),
            json!(port),
            json!(uptime),
            Value::String(since.clone()),
        ),
        None => (Value::Null, Value::Null, Value::Null, Value::Null),
    };

    json!({
        "running": mode,
        "since": since,
        "uptime_secs": uptime,
        "port": port,
        "record_port": state.record_port,
        "dir": state.dir.display().to_string(),
        "version": env!("CARGO_PKG_VERSION"),
        "counts": {
            "snapshots": snaps.len(),
            "origins": origins,
            "bytes": bytes,
        },
        "local_ips": local_ips(),
        "transparent": transparent,
        "config_path": if state.config_path.is_file() {
            Value::String(state.config_path.display().to_string())
        } else {
            Value::Null
        },
        "last_error": last_error,
        "admin_uptime_secs": state.started.elapsed().as_secs(),
    })
}

fn list_snapshots(state: &AdminState) -> Result<Value> {
    let snaps = state.snapshots()?;
    let list: Vec<Value> = snaps.iter().map(summary_json).collect();
    Ok(Value::Array(list))
}

fn snapshot_detail(state: &AdminState, id: &str) -> Result<Response<Full<Bytes>>> {
    let snaps = state.snapshots()?;
    let Some(snap) = snaps.iter().find(|s| s.id == id) else {
        return Ok(error_response(StatusCode::NOT_FOUND, "没有这条快照"));
    };
    let mut value = serde_json::to_value(snap)?;
    if let Value::Object(map) = &mut value {
        for (k, v) in summary_object(snap) {
            map.insert(k, v);
        }
    }
    Ok(json_response(value))
}

fn snapshot_body(state: &AdminState, id: &str, side: &str) -> Result<Response<Full<Bytes>>> {
    let snaps = state.snapshots()?;
    let Some(snap) = snaps.iter().find(|s| s.id == id) else {
        return Ok(error_response(StatusCode::NOT_FOUND, "没有这条快照"));
    };
    let (body, encoding, headers) = if side == "request" {
        (
            &snap.request.body,
            &snap.request.body_encoding,
            &snap.request.headers,
        )
    } else {
        (
            &snap.response.body,
            &snap.response.body_encoding,
            &snap.response.headers,
        )
    };
    let bytes = decode_body(body, encoding);
    let content_type = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "application/octet-stream".to_string());
    Ok(bytes_response(&content_type, &bytes))
}

fn summary_json(snap: &Snapshot) -> Value {
    Value::Object(summary_object(snap))
}

fn summary_object(snap: &Snapshot) -> serde_json::Map<String, Value> {
    let (host, path) = split_url(&snap.request.url);
    let content_type = snap
        .response
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let mut map = serde_json::Map::new();
    map.insert("id".into(), json!(snap.id));
    map.insert("method".into(), json!(snap.request.method));
    map.insert("url".into(), json!(snap.request.url));
    map.insert("host".into(), json!(host));
    map.insert("path".into(), json!(path));
    map.insert("status".into(), json!(snap.response.status));
    map.insert("duration_ms".into(), json!(snap.duration_ms));
    map.insert("recorded_at".into(), json!(snap.recorded_at));
    map.insert("req_bytes".into(), json!(snap.request.body.len()));
    map.insert("resp_bytes".into(), json!(snap.response.body.len()));
    map.insert("content_type".into(), json!(content_type));
    map.insert("origin".into(), json!(snap.origin));
    map
}

fn split_url(url: &str) -> (String, String) {
    if let Ok(uri) = url.parse::<hyper::Uri>() {
        let host = uri
            .authority()
            .map(|a| a.as_str().to_string())
            .or_else(|| uri.host().map(|h| h.to_string()))
            .unwrap_or_default();
        let path = uri
            .path_and_query()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "/".to_string());
        return (host, path);
    }
    (String::new(), url.to_string())
}

fn config_json(state: &AdminState) -> Result<Value> {
    let raw = if state.config_path.is_file() {
        read_raw_config(&state.config_path)?
    } else {
        RawConfig::default()
    };
    Ok(serde_json::to_value(raw)?)
}

fn start_child(state: &Arc<AdminState>, mode: Mode, port: Option<u16>) -> Result<()> {
    let port = port.unwrap_or(if mode == Mode::Record {
        state.record_port
    } else {
        crate::config::DEFAULT_PORT
    });
    if port == 0 {
        bail!("端口不能为 0");
    }

    // 先把已退出的旧进程清掉，避免「看起来还在跑」。
    if let Some(run) = state.running.lock().unwrap().as_mut()
        && run.child.try_wait()?.is_none()
    {
        bail!("已有{}进程在运行，请先停止", run.mode.label());
    }

    // 预检端口：子进程报错要等它在日志里出现，不如提前给一句人话。
    if let Ok(probe) = std::net::TcpListener::bind(("0.0.0.0", port)) {
        drop(probe);
    } else {
        bail!("端口 {port} 已被占用，换一个端口或先关掉占用程序");
    }

    let exe = std::env::current_exe().context("无法定位 tape 可执行文件")?;
    let mut cmd = Command::new(exe);
    cmd.arg(mode.as_str())
        .arg("--dir")
        .arg(&state.dir)
        .arg("--port")
        .arg(port.to_string())
        .arg("--bind")
        .arg("0.0.0.0")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("启动 {} 进程失败", mode.label()))?;

    if let Some(stdout) = child.stdout.take() {
        spawn_reader(state.clone(), stdout);
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_reader(state.clone(), stderr);
    }

    let started_at = rfc3339_now();
    *state.running.lock().unwrap() = Some(Running {
        mode,
        child,
        port,
        started: Instant::now(),
        started_at: started_at.clone(),
    });
    *state.last_error.lock().unwrap() = None;
    state.push_log(format!(
        "已启动{}：端口 {port}，数据目录 {}",
        mode.label(),
        state.dir.display()
    ));

    // 给子进程一点时间绑定端口；立刻退出通常意味着端口冲突/参数错误。
    std::thread::sleep(Duration::from_millis(400));
    state.running_snapshot();
    if state.running.lock().unwrap().is_none() {
        let tail = state
            .logs
            .lock()
            .unwrap()
            .iter()
            .rev()
            .take(4)
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
            .join(" / ");
        bail!("{}启动失败：{tail}", mode.label());
    }
    Ok(())
}

fn spawn_reader<R: std::io::Read + Send + 'static>(state: Arc<AdminState>, reader: R) {
    std::thread::spawn(move || {
        for line in BufReader::new(reader)
            .lines()
            .map_while(std::result::Result::ok)
        {
            state.push_log(line);
        }
    });
}

fn stop_child(state: &AdminState, mode: Mode) -> Result<()> {
    let mut slot = state.running.lock().unwrap();
    let Some(run) = slot.as_mut() else {
        bail!("当前没有{}进程", mode.label());
    };
    if run.mode != mode {
        bail!("当前运行的是{}，不是{}", run.mode.label(), mode.label());
    }
    run.child.kill().context("结束子进程失败")?;
    let _ = run.child.wait();
    state.push_log(format!("已停止{}", mode.label()));
    *slot = None;
    Ok(())
}

async fn read_json<T: for<'de> Deserialize<'de>>(req: Request<Incoming>) -> Result<T> {
    let collected = req
        .into_body()
        .collect()
        .await
        .context("读取请求体失败")?
        .to_bytes();
    if collected.len() > MAX_JSON_BODY {
        bail!("请求体过大");
    }
    serde_json::from_slice(&collected).context("请求体不是合法 JSON")
}

fn json_response(value: Value) -> Response<Full<Bytes>> {
    let body = serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec());
    bytes_response("application/json; charset=utf-8", &body)
}

fn html_response(body: &'static str) -> Response<Full<Bytes>> {
    bytes_response("text/html; charset=utf-8", body.as_bytes())
}

fn bytes_response(content_type: &str, bytes: &[u8]) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .body(Full::new(Bytes::copy_from_slice(bytes)))
        .expect("响应构造失败")
}

fn error_response(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    let body = serde_json::to_vec(&json!({ "error": message })).unwrap();
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json; charset=utf-8")
        .body(Full::new(Bytes::from(body)))
        .expect("响应构造失败")
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && input.is_char_boundary(i + 1)
            && input.is_char_boundary(i + 3)
            && let Ok(byte) = u8::from_str_radix(&input[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn local_ips() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(sock) = UdpSocket::bind("0.0.0.0:0")
        && sock.connect("8.8.8.8:80").is_ok()
        && let Ok(addr) = sock.local_addr()
        && !addr.ip().is_loopback()
    {
        out.push(addr.ip().to_string());
    }
    out.push("127.0.0.1".to_string());
    out
}

fn clock_now() -> String {
    const CLOCK: &[time::format_description::FormatItem<'static>] =
        time::macros::format_description!("[hour]:[minute]:[second]");
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    now.format(CLOCK).unwrap_or_else(|_| "00:00:00".to_string())
}

fn rfc3339_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// 去掉 tracing 的 ANSI 颜色，浏览器里直接显示纯文本。
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                // CSI：ESC [ ... 终止符为字母
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                // OSC：ESC ] ... BEL 或 ESC \
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        out.push(c);
    }
    out
}
/// Ctrl+C 与 Unix SIGTERM 都按正常退出处理，避免 kill 掉 admin 后录制子进程变孤儿。
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async {
                match term.as_mut() {
                    Some(sig) => {
                        sig.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_handles_escapes_and_plus() {
        assert_eq!(percent_decode("a%20b+c"), "a b c");
        assert_eq!(percent_decode("%2Fapi%2Fx"), "/api/x");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("中文"), "中文");
    }

    #[test]
    fn query_param_reads_first_match_and_decodes() {
        assert_eq!(
            query_param("side=request&limit=20", "side").as_deref(),
            Some("request")
        );
        assert_eq!(
            query_param("side=request&limit=20", "limit").as_deref(),
            Some("20")
        );
        assert_eq!(
            query_param("side=%72equest", "side").as_deref(),
            Some("request")
        );
        assert_eq!(query_param("side=request", "missing"), None);
        assert_eq!(query_param("", "side"), None);
    }

    #[test]
    fn strip_ansi_removes_color_codes() {
        assert_eq!(strip_ansi("\u{1b}[32mINFO\u{1b}[0m tape"), "INFO tape");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}text"), "text");
    }
}
