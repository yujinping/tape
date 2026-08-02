use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::config::{CONFIG_FILE_NAME, RecordConfig, RecordFilter};
use crate::download::{
    ResourceStore, download_resources, is_static_asset_type, looks_like_resource,
};
use crate::http_util::{is_hop_by_hop, parse_proxy_target};
use crate::net::{HttpClient, build_client};
use crate::rewrite::{RewriteRule, rewrite_location, rewrite_response_bytes_for};
use crate::snapshot::{self, RequestRecord, ResponseRecord, Snapshot};
use crate::store::Recorder;

pub struct RecordState {
    pub recorder: Recorder,
    pub client: HttpClient,
    pub rewrite_on_record: bool,
    pub resources: Arc<Mutex<ResourceStore>>,
    /// 录制过滤规则（host 数组 + 正则数组），空规则 = 录制全部
    pub filter: RecordFilter,
}

impl RecordState {
    pub fn new(dir: PathBuf, rewrite_on_record: bool, filter: RecordFilter) -> Result<Arc<Self>> {
        let recorder = Recorder::new(dir.clone())?;
        let resources = Arc::new(Mutex::new(ResourceStore::open(&dir.join("resources"))?));
        let client = build_client()?;
        Ok(Arc::new(Self {
            recorder,
            client,
            rewrite_on_record,
            resources,
            filter,
        }))
    }

    fn should_record(&self, authority: &str) -> bool {
        self.filter.matches(authority)
    }
}

pub async fn run(cfg: RecordConfig) -> Result<()> {
    let state = RecordState::new(cfg.dir.clone(), cfg.rewrite_on_record, cfg.filter)?;
    let listener = TcpListener::bind(("0.0.0.0", cfg.port)).await?;
    let abs_dir = std::path::absolute(&cfg.dir).unwrap_or_else(|_| cfg.dir.clone());
    info!(
        "tape record 已启动: 0.0.0.0:{} （数据目录: {}，绝对路径: {}）",
        cfg.port,
        cfg.dir.display(),
        abs_dir.display()
    );
    if state.filter.is_all() {
        info!(
            "录制过滤: 全部上游（可在数据目录放置 {} 或用 --config 指定配置文件限定只录制匹配的上游）",
            CONFIG_FILE_NAME
        );
    } else {
        if let Some(path) = &cfg.config_path {
            info!("录制过滤配置文件: {}", path.display());
        }
        info!("录制过滤: 仅配置文件中匹配的上游（其余请求正常转发但不落快照）");
    }
    info!("请在 APP/设备上配置 HTTP 代理为 本机IP:{}", cfg.port);
    accept_loop(listener, state).await
}

pub async fn accept_loop(listener: TcpListener, state: Arc<RecordState>) -> Result<()> {
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

async fn serve_conn(state: Arc<RecordState>, stream: TcpStream, _addr: SocketAddr) -> Result<()> {
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
    state: Arc<RecordState>,
    req: Request<Incoming>,
) -> Response<Full<Bytes>> {
    let Some(target) = parse_proxy_target(req.uri()) else {
        return simple_response(
            StatusCode::BAD_REQUEST,
            "tape record 需要 absolute-form 请求行（正向代理），或 /http://host/path 前缀式请求路径（无法配置代理时直接加前缀访问）",
        );
    };
    let scheme = target.scheme;
    if scheme != "http" && scheme != "https" {
        return simple_response(
            StatusCode::NOT_IMPLEMENTED,
            &format!("不支持协议 {scheme}，本工具仅支持 http/https"),
        );
    }

    let prefix_style = target.prefix;
    let authority = target.authority;
    let record = state.should_record(&authority);
    let origin = format!("{scheme}://{authority}");
    let path_and_query = target.path_and_query;
    let full_url = format!("{origin}{path_and_query}");

    let (parts, body) = req.into_parts();
    let method = parts.method.clone();
    // 前缀式请求下 Host 是客户端看到的 tape 自身地址（改写 Location/链接回 tape 需要它）
    let req_host = parts
        .headers
        .get(hyper::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let request_headers = parts
        .headers
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect::<Vec<_>>();

    let req_body = match body.collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => return simple_response(StatusCode::BAD_REQUEST, &format!("读取请求体失败: {e}")),
    };

    // 转发必须携带完整绝对 URL，hyper 客户端据此解析连接目标
    let mut builder = Request::builder().method(&method).uri(full_url.clone());
    for (name, value) in parts.headers.iter() {
        // 丢弃客户端原始 Host（absolute-form/前缀式下它是代理自身地址，会导致上游 403），
        // 下面统一覆写为目标 authority。
        // Accept-Encoding 也覆写为 identity：保证上游返回明文，页面/JS/CSS 才能被改写；
        // 否则浏览器带 gzip/br 时上游压缩响应，压缩体无法改写，页面链接会直连公网。
        if is_hop_by_hop(name)
            || name == hyper::header::HOST
            || name == hyper::header::ACCEPT_ENCODING
        {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder = builder.header(hyper::header::HOST, &authority);
    builder = builder.header(hyper::header::ACCEPT_ENCODING, "identity");
    // 防盗链兜底：部分 CDN（如 mintcdn）对无 Referer 的静态资源请求会拖延/拦截；
    // 浏览器可能因 Referrer-Policy 不带 Referer，这里对静态资源 GET/HEAD 补页面 origin 的 Referer
    // （客户端已带 Referer 时原样保留，不改写）。
    if !parts.headers.contains_key(hyper::header::REFERER)
        && (method == hyper::Method::GET || method == hyper::Method::HEAD)
        && looks_like_resource(&path_and_query)
    {
        builder = builder.header(hyper::header::REFERER, format!("{origin}/"));
    }
    let forward = match builder.body(Full::new(req_body.clone())) {
        Ok(f) => f,
        Err(e) => {
            return simple_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("构造转发请求失败: {e}"),
            );
        }
    };

    let start = Instant::now();
    // 静态资源（图片/字体/CSS/JS 等）允许更长的上游响应时间：部分 CDN（如 mintcdn）TTFB 可达 14s+，
    // 统一 30s 总超时会导致浏览器侧图片加载失败；页面/API 请求仍保持 30s，避免慢上游拖垮交互。
    let (timeout, timeout_label) = if looks_like_resource(&path_and_query) {
        (Duration::from_secs(120), "120s")
    } else {
        (Duration::from_secs(30), "30s")
    };
    // GET/HEAD 幂等，连接池可能复用了被上游重置的死连接，失败重试一次（其它方法不重试，避免重复提交）。
    let retryable = method == hyper::Method::GET || method == hyper::Method::HEAD;
    let retry_forward = forward.clone();
    let outcome = tokio::time::timeout(timeout, async {
        let resp = match state.client.request(forward).await {
            Ok(r) => r,
            Err(e) if retryable => {
                warn!("转发失败，重试一次 {}: {}", origin, e);
                state.client.request(retry_forward).await?
            }
            Err(e) => return Err(e.into()),
        };
        let (resp_parts, resp_body) = resp.into_parts();
        let body = resp_body.collect().await?.to_bytes();
        Ok::<_, anyhow::Error>((resp_parts, body))
    })
    .await;

    match outcome {
        Ok(Ok((resp_parts, resp_body))) => {
            let duration_ms = start.elapsed().as_millis() as u64;
            let status = resp_parts.status;
            let resp_headers = resp_parts
                .headers
                .iter()
                .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                .collect::<Vec<_>>();
            let content_type = resp_parts
                .headers
                .get(hyper::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let content_encoding = resp_parts
                .headers
                .get(hyper::header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();

            if record {
                record_response(
                    &state,
                    CallData {
                        method: &method,
                        origin: &origin,
                        full_url: &full_url,
                        path_and_query: &path_and_query,
                        request_headers,
                        req_body: &req_body,
                        status: status.as_u16(),
                        resp_headers: &resp_headers,
                        resp_body: &resp_body,
                        content_type: &content_type,
                        duration_ms,
                    },
                );
            } else {
                debug!("未匹配录制过滤规则，仅转发不录制: {}", origin);
            }

            // 回传：absolute-form 默认原样（保真），开启 --rewrite-on-record 时改写 body；
            // 前缀式请求自动把 Location/链接改写成回到 tape 的前缀式地址（快照仍存原始响应），
            // 否则客户端会按真实地址直连上游，跳转/链接直接绕过 tape 导致断链。
            let prefix_base = if prefix_style && !req_host.is_empty() {
                Some(format!("http://{req_host}"))
            } else {
                None
            };
            let out_body = if let Some(base) = &prefix_base {
                rewrite_response_bytes_for(
                    &resp_body,
                    &content_encoding,
                    &content_type,
                    &RewriteRule::Prefix {
                        base: base.clone(),
                        scheme: scheme.clone(),
                        origin: authority.clone(),
                    },
                )
            } else if state.rewrite_on_record {
                rewrite_response_bytes_for(
                    &resp_body,
                    &content_encoding,
                    &content_type,
                    &RewriteRule::Relative,
                )
            } else {
                resp_body
            };

            let mut builder = Response::builder().status(status);
            for (name, value) in resp_parts.headers.iter() {
                if is_hop_by_hop(name) || name == hyper::header::CONTENT_LENGTH {
                    continue;
                }
                if let Some(base) = &prefix_base
                    && name == hyper::header::LOCATION
                    && let Ok(loc) = value.to_str()
                {
                    builder =
                        builder.header(name, rewrite_location(loc, &scheme, &authority, base));
                    continue;
                }
                builder = builder.header(name, value);
            }
            match builder.body(Full::new(out_body)) {
                Ok(resp) => resp,
                Err(e) => simple_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("构造回传响应失败: {e}"),
                ),
            }
        }
        Ok(Err(e)) => {
            warn!("转发失败 {}: {}", origin, e);
            simple_response(StatusCode::BAD_GATEWAY, &format!("无法连接上游 {origin}"))
        }
        Err(_) => {
            warn!("转发超时（{timeout_label}）: {}", origin);
            simple_response(
                StatusCode::GATEWAY_TIMEOUT,
                &format!("上游响应超时（{timeout_label}）"),
            )
        }
    }
}

/// 一次被录制调用的上下文。
struct CallData<'a> {
    method: &'a hyper::Method,
    origin: &'a str,
    full_url: &'a str,
    path_and_query: &'a str,
    request_headers: Vec<(String, String)>,
    req_body: &'a Bytes,
    status: u16,
    resp_headers: &'a [(String, String)],
    resp_body: &'a Bytes,
    content_type: &'a str,
    duration_ms: u64,
}

/// 快照落盘 + 后台资源处理（文本响应提取下载、静态类型响应直接入库）。
fn record_response(state: &Arc<RecordState>, call: CallData<'_>) {
    let id = state.recorder.next_id();
    let (req_body_text, req_body_encoding) = snapshot::encode_body(call.req_body);
    let (resp_body_text, resp_body_encoding) = snapshot::encode_body(call.resp_body);
    let snapshot = Snapshot {
        id: id.clone(),
        origin: call.origin.to_string(),
        recorded_at: crate::store::now_rfc3339(),
        duration_ms: call.duration_ms,
        request: RequestRecord {
            method: call.method.to_string(),
            url: call.full_url.to_string(),
            headers: call.request_headers,
            body: req_body_text,
            body_encoding: req_body_encoding,
        },
        response: ResponseRecord {
            status: call.status,
            headers: call.resp_headers.to_vec(),
            body: resp_body_text,
            body_encoding: resp_body_encoding,
        },
    };
    if let Err(e) = state.recorder.write_snapshot(&snapshot) {
        warn!("快照写入失败: {e}");
    }
    info!(
        "{} {} {} {} {} ({}ms)",
        id, call.method, call.origin, call.path_and_query, call.status, call.duration_ms
    );

    // 文本类响应：提取并下载其中引用的资源（绝对 URL + 相对路径）
    if let Ok(text) = std::str::from_utf8(call.resp_body) {
        let text = text.to_string();
        let client = state.client.clone();
        let resources = state.resources.clone();
        let origin = call.origin.to_string();
        let ct = call.content_type.to_string();
        tokio::spawn(async move {
            if let Err(e) = download_resources(&client, &resources, &text, &ct, &origin).await {
                warn!("资源下载处理失败: {e}");
            }
        });
    }

    // 静态资源类型响应：本身直接入库 resources/（快照之外的结构化存储）
    if is_static_asset_type(call.content_type) {
        let resources = state.resources.clone();
        let origin = call.origin.to_string();
        let path = crate::rewrite::url_path(call.full_url);
        let body = call.resp_body.clone();
        let ct = call.content_type.to_string();
        tokio::spawn(async move {
            let mut store = resources.lock().await;
            if let Err(e) = store.store(&body, &origin, &path, &ct) {
                warn!("静态资源入库失败: {e}");
            }
            if let Err(e) = store.save_index() {
                warn!("资源索引写入失败: {e}");
            }
        });
    }
}

fn simple_response(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(message.to_string())))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Full::new(Bytes::new()))
                .unwrap()
        })
}
