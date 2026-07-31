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
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::config::{RecordConfig, RecordFilter};
use crate::download::{ResourceStore, download_resources, is_static_asset_type};
use crate::http_util::is_hop_by_hop;
use crate::rewrite::{RewriteRule, rewrite_text};
use crate::snapshot::{self, RequestRecord, ResponseRecord, Snapshot};
use crate::store::Recorder;

pub type HttpClient = Client<HttpConnector, Full<Bytes>>;

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
        let client = Client::builder(TokioExecutor::new()).build_http();
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
    info!(
        "box-proxy record 已启动: 0.0.0.0:{} （数据目录: {}）",
        cfg.port,
        cfg.dir.display()
    );
    if state.filter.is_all() {
        info!("录制过滤: 全部上游（可用 --config 指定配置文件限定只录制匹配的上游）");
    } else {
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
    let uri = req.uri();
    let (scheme, authority) = match (uri.scheme_str(), uri.authority()) {
        (Some(s), Some(a)) => (s.to_string(), a.as_str().to_string()),
        _ => {
            return simple_response(
                StatusCode::BAD_REQUEST,
                "box-proxy record 需要 absolute-form 请求行（正向代理模式下请为 APP 配置 HTTP 代理）",
            );
        }
    };
    if scheme != "http" {
        return simple_response(
            StatusCode::NOT_IMPLEMENTED,
            &format!("不支持协议 {scheme}，本工具仅支持纯 HTTP"),
        );
    }

    let record = state.should_record(&authority);
    let origin = format!("{scheme}://{authority}");
    let path_and_query = uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let full_url = format!("{origin}{path_and_query}");

    let (parts, body) = req.into_parts();
    let method = parts.method.clone();
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
        if is_hop_by_hop(name) {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder = builder.header(hyper::header::HOST, &authority);
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
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        let resp = state.client.request(forward).await?;
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

            // 回传：默认原样（保真）；开启 --rewrite-on-record 时改写
            let mut out_body = resp_body;
            if state.rewrite_on_record
                && let Ok(text) = std::str::from_utf8(&out_body)
            {
                out_body = Bytes::from(rewrite_text(text, &RewriteRule::Relative));
            }

            let mut builder = Response::builder().status(status);
            for (name, value) in resp_parts.headers.iter() {
                if is_hop_by_hop(name) || name == hyper::header::CONTENT_LENGTH {
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
            warn!("转发超时（30s）: {}", origin);
            simple_response(StatusCode::GATEWAY_TIMEOUT, "上游响应超时（30s）")
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
