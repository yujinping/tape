use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

use crate::config::ReplayConfig;
use crate::download::{ResourceIndexEntry, ResourceStore};
use crate::http_util::{is_hop_by_hop_str, parse_proxy_target};
use crate::rewrite::{RewriteRule, rewrite_location, rewrite_response_bytes_for};
use crate::snapshot::{self, Snapshot};
use crate::store;

pub struct ReplayState {
    root: PathBuf,
    snapshots: Vec<Snapshot>,
    /// (METHOD, path) -> 快照下标（按 PRD 以 method+path 匹配，忽略 query）
    by_path: HashMap<(String, String), Vec<usize>>,
    /// "/相对路径" -> 资源索引项
    resources: HashMap<String, ResourceIndexEntry>,
    rewrite: RewriteRule,
}

impl ReplayState {
    pub fn new(dir: PathBuf, rewrite: RewriteRule) -> Result<Arc<Self>> {
        let snapshots = store::load_snapshots(&dir)?;
        let mut by_path: HashMap<(String, String), Vec<usize>> = HashMap::new();
        for (i, snap) in snapshots.iter().enumerate() {
            let (method, path) = request_method_path(snap);
            by_path.entry((method, path)).or_default().push(i);
        }
        let resources = ResourceStore::open(&dir.join("resources"))?
            .index()
            .iter()
            .map(|e| (format!("/{}", e.path), e.clone()))
            .collect::<HashMap<_, _>>();
        info!(
            "已加载 {} 条快照、{} 个静态资源（{}）",
            snapshots.len(),
            resources.len(),
            dir.display()
        );
        Ok(Arc::new(Self {
            root: dir,
            snapshots,
            by_path,
            resources,
            rewrite,
        }))
    }

    fn match_snapshot(&self, method: &str, path: &str, host: &str) -> Option<&Snapshot> {
        let key = (method.to_ascii_uppercase(), path.to_string());
        let candidates = self.by_path.get(&key)?;
        for &i in candidates {
            if origin_host(&self.snapshots[i].origin) == host {
                return Some(&self.snapshots[i]);
            }
        }
        candidates
            .iter()
            .max_by_key(|&&i| self.snapshots[i].id.clone())
            .map(|&i| &self.snapshots[i])
    }
}

pub async fn run(cfg: ReplayConfig) -> Result<()> {
    let state = ReplayState::new(cfg.dir.clone(), cfg.rewrite)?;
    let listener = TcpListener::bind(("0.0.0.0", cfg.port)).await?;
    let abs_dir = std::path::absolute(&cfg.dir).unwrap_or_else(|_| cfg.dir.clone());
    info!(
        "tape replay 已启动: 0.0.0.0:{} （数据目录: {}，绝对路径: {}）",
        cfg.port,
        cfg.dir.display(),
        abs_dir.display()
    );
    if let Some(path) = &cfg.config_path {
        info!("配置文件: {}", path.display());
    }
    info!("请将 APP 的服务器 IP 改为 本机IP:{} 后访问", cfg.port);
    accept_loop(listener, state).await
}

pub async fn accept_loop(listener: TcpListener, state: Arc<ReplayState>) -> Result<()> {
    loop {
        let (stream, addr) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_conn(state, stream, addr).await {
                warn!("连接处理失败 {}: {}", addr, e);
            }
        });
    }
}

async fn serve_conn(state: Arc<ReplayState>, stream: TcpStream, _addr: SocketAddr) -> Result<()> {
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

pub async fn handle_request(
    state: Arc<ReplayState>,
    req: Request<Incoming>,
) -> Response<Full<Bytes>> {
    let method = req.method().to_string();
    let host = req
        .headers()
        .get(hyper::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // 自动识别 absolute-form 与 /http://host/path 前缀式请求，剥离前缀后匹配快照；
    // 前缀式/代理式请求用解析出的目标主机做 origin 精确匹配，普通直连请求用 Host 头。
    let target = parse_proxy_target(req.uri());
    let path = target
        .as_ref()
        .map(|t| t.path_and_query.clone())
        .unwrap_or_else(|| req.uri().path().to_string());
    let match_host = target
        .as_ref()
        .map(|t| t.authority.clone())
        .unwrap_or_else(|| host.clone());
    // 前缀式请求：响应里的 Location/链接自动改写成回到 tape 的前缀式地址（无需开关），
    // 否则客户端会按真实地址直连上游，跳转/链接直接绕过 tape 导致断链。
    let prefix_ctx = match &target {
        Some(t) if t.prefix && !host.is_empty() => Some((
            format!("http://{host}"),
            t.scheme.clone(),
            t.authority.clone(),
        )),
        _ => None,
    };

    if let Some(snap) = state.match_snapshot(&method, &path, &match_host) {
        info!("HIT  {} {}", method, path);
        return serve_snapshot(&state, snap, &path, prefix_ctx.as_ref());
    }
    if let Some(entry) = state.resources.get(&path) {
        info!("RES  {} {}", method, path);
        return serve_resource(&state, entry, &path, prefix_ctx.as_ref());
    }
    warn!("MISS {} {}", method, path);
    let body = format!("未找到 {} {} 的录制快照", method, path);
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}

fn serve_snapshot(
    state: &ReplayState,
    snap: &Snapshot,
    path: &str,
    prefix: Option<&(String, String, String)>,
) -> Response<Full<Bytes>> {
    let raw_body = snapshot::decode_body(&snap.response.body, &snap.response.body_encoding);
    let content_encoding = snap
        .response
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-encoding"))
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let content_type = snap
        .response
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let body = match prefix {
        Some((base, scheme, origin)) => rewrite_response_bytes_for(
            &raw_body,
            &content_encoding,
            &content_type,
            &RewriteRule::Prefix {
                base: base.clone(),
                scheme: scheme.clone(),
                origin: origin.clone(),
            },
        ),
        None if matches!(state.rewrite, RewriteRule::None) => Bytes::from(raw_body),
        None => {
            rewrite_response_bytes_for(&raw_body, &content_encoding, &content_type, &state.rewrite)
        }
    };

    let mut builder = Response::builder().status(snap.response.status);
    let mut has_content_type = false;
    for (name, value) in &snap.response.headers {
        if is_hop_by_hop_str(name) || name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        if name.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        if let Some((base, scheme, origin)) = prefix
            && name.eq_ignore_ascii_case("location")
        {
            builder = builder.header(name, rewrite_location(value, scheme, origin, base));
            continue;
        }
        builder = builder.header(name, value);
    }
    if !has_content_type {
        let mime = mime_guess::from_path(path)
            .first_raw()
            .unwrap_or("application/octet-stream");
        builder = builder.header(hyper::header::CONTENT_TYPE, mime);
    }
    builder
        .body(Full::new(body))
        .unwrap_or_else(|_| not_found("构造响应失败"))
}

fn serve_resource(
    state: &ReplayState,
    entry: &ResourceIndexEntry,
    path: &str,
    prefix: Option<&(String, String, String)>,
) -> Response<Full<Bytes>> {
    let blob_path = state.root.join("resources").join("blobs").join(&entry.hash);
    match std::fs::read(&blob_path) {
        Ok(data) => {
            let mime = mime_guess::from_path(path)
                .first_raw()
                .unwrap_or(&entry.content_type);
            let body = match prefix {
                Some((base, scheme, origin)) => rewrite_response_bytes_for(
                    &data,
                    "",
                    mime,
                    &RewriteRule::Prefix {
                        base: base.clone(),
                        scheme: scheme.clone(),
                        origin: origin.clone(),
                    },
                ),
                None => Bytes::from(data),
            };
            Response::builder()
                .status(StatusCode::OK)
                .header(hyper::header::CONTENT_TYPE, mime)
                .header("Cache-Control", "public, max-age=86400")
                .body(Full::new(body))
                .unwrap()
        }
        Err(e) => {
            warn!("静态资源读取失败 {}: {}", entry.path, e);
            not_found("静态资源缺失")
        }
    }
}

fn not_found(message: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(message.to_string())))
        .unwrap()
}

fn request_method_path(snap: &Snapshot) -> (String, String) {
    let method = snap.request.method.to_ascii_uppercase();
    let url = &snap.request.url;
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let with_path = after_scheme
        .split_once('/')
        .map(|(_, p)| format!("/{p}"))
        .unwrap_or_else(|| "/".to_string());
    let path = with_path.split('?').next().unwrap_or("/").to_string();
    (method, path)
}

fn origin_host(origin: &str) -> String {
    origin
        .split_once("://")
        .map(|(_, r)| r.to_string())
        .unwrap_or_else(|| origin.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{RequestRecord, ResponseRecord};

    fn snap(id: &str, method: &str, url: &str, origin: &str) -> Snapshot {
        Snapshot {
            id: id.to_string(),
            origin: origin.to_string(),
            recorded_at: "2026-08-01T00:00:00Z".to_string(),
            duration_ms: 1,
            request: RequestRecord {
                method: method.to_string(),
                url: url.to_string(),
                headers: vec![],
                body: String::new(),
                body_encoding: snapshot::ENCODING_UTF8.to_string(),
            },
            response: ResponseRecord {
                status: 200,
                headers: vec![("content-type".to_string(), "application/json".to_string())],
                body: "{}".to_string(),
                body_encoding: snapshot::ENCODING_UTF8.to_string(),
            },
        }
    }

    #[test]
    fn parses_request_path_ignoring_query() {
        let s = snap(
            "000001",
            "GET",
            "http://10.1.2.3:8080/api/user?id=1",
            "http://10.1.2.3:8080",
        );
        assert_eq!(
            request_method_path(&s),
            ("GET".to_string(), "/api/user".to_string())
        );
    }

    #[test]
    fn origin_host_extraction() {
        assert_eq!(origin_host("http://10.1.2.3:8080"), "10.1.2.3:8080");
    }
}
