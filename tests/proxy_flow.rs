use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use box_proxy::download::ResourceStore;
use box_proxy::record::{RecordState, accept_loop as record_accept_loop};
use box_proxy::replay::{ReplayState, accept_loop as replay_accept_loop};
use box_proxy::rewrite::RewriteRule;
use box_proxy::snapshot::{self, RequestRecord, ResponseRecord, Snapshot};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

type TestClient = Client<HttpConnector, Full<Bytes>>;

fn client() -> TestClient {
    Client::builder(TokioExecutor::new()).build_http()
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "box-proxy-it-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

async fn free_listener() -> TcpListener {
    TcpListener::bind(("127.0.0.1", 0)).await.unwrap()
}

/// 本地 mock 上游：任何请求都返回固定 JSON。
async fn spawn_origin() -> SocketAddr {
    let listener = free_listener().await;
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let io = TokioIo::new(stream);
            let svc = service_fn(|_req: Request<Incoming>| async {
                let body = r#"{"name":"x","avatar":"http://10.1.2.3:9090/img/a.png"}"#;
                Ok::<_, std::convert::Infallible>(
                    Response::builder()
                        .status(200)
                        .header("content-type", "application/json")
                        .body(Full::new(Bytes::from(body)))
                        .unwrap(),
                )
            });
            tokio::spawn(async move {
                let _ = http1::Builder::new().serve_connection(io, svc).await;
            });
        }
    });
    addr
}

/// 本地 mock 上游：/img/* 返回图片字节，其余返回固定 JSON。
async fn spawn_origin_asset() -> SocketAddr {
    let listener = free_listener().await;
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let io = TokioIo::new(stream);
            let svc = service_fn(|req: Request<Incoming>| async move {
                let is_img = req.uri().path().starts_with("/img/");
                if is_img {
                    Ok::<_, std::convert::Infallible>(
                        Response::builder()
                            .status(200)
                            .header("content-type", "image/png")
                            .body(Full::new(Bytes::from_static(b"FAKE-PNG-BYTES")))
                            .unwrap(),
                    )
                } else {
                    Ok::<_, std::convert::Infallible>(
                        Response::builder()
                            .status(200)
                            .header("content-type", "application/json")
                            .body(Full::new(Bytes::from_static(b"{\"ok\":true}")))
                            .unwrap(),
                    )
                }
            });
            tokio::spawn(async move {
                let _ = http1::Builder::new().serve_connection(io, svc).await;
            });
        }
    });
    addr
}

/// 通过 TCP 原始发送 absolute-form 请求到代理（模拟真实正向代理客户端）。
async fn raw_proxy_get(proxy: SocketAddr, target: &str) -> String {
    let mut stream = TcpStream::connect(proxy).await.unwrap();
    let req = format!("GET {target} HTTP/1.1\r\nHost: {target}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    String::from_utf8_lossy(&buf).to_string()
}

fn write_snapshot(dir: &Path, snap: &Snapshot) {
    let origin_dir = dir
        .join("snapshots")
        .join(box_proxy::store::origin_dir_name(&snap.origin));
    std::fs::create_dir_all(&origin_dir).unwrap();
    let file = origin_dir.join(format!(
        "{}-{}-{}.json",
        snap.id,
        snap.request.method,
        box_proxy::store::path_hash(&snap.request.url)
    ));
    std::fs::write(file, serde_json::to_string_pretty(snap).unwrap()).unwrap();
}

async fn wait_until(cond: impl Fn() -> bool, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    cond()
}

#[tokio::test]
async fn record_proxy_captures_and_forwards() {
    let origin = spawn_origin().await;
    let dir = temp_dir("record");
    let state =
        RecordState::new(dir.clone(), false, box_proxy::config::RecordFilter::all()).unwrap();
    let listener = free_listener().await;
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(record_accept_loop(listener, state));

    let target = format!("http://127.0.0.1:{}/api/user?x=1", origin.port());
    let resp = raw_proxy_get(proxy_addr, &target).await;

    assert!(resp.contains("\"name\":\"x\""), "回传应与上游一致: {resp}");

    let snap_dir = dir.join("snapshots");
    assert!(
        wait_until(
            || std::fs::read_dir(&snap_dir)
                .map(|mut it| it.any(|e| e.unwrap().path().is_dir()))
                .unwrap_or(false),
            Duration::from_secs(3)
        )
        .await,
        "快照目录应生成"
    );

    let snaps = box_proxy::store::load_snapshots(&dir).unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].request.url, target);
    assert_eq!(snaps[0].request.method, "GET");
    assert_eq!(snaps[0].response.status, 200);
    assert!(
        snaps[0]
            .response
            .body
            .contains("http://10.1.2.3:9090/img/a.png")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn replay_matches_by_method_path_and_rewrites() {
    let dir = temp_dir("replay");

    let snap = Snapshot {
        id: "000001".to_string(),
        origin: "http://10.1.2.3:8080".to_string(),
        recorded_at: "2026-08-01T00:00:00Z".to_string(),
        duration_ms: 1,
        request: RequestRecord {
            method: "GET".to_string(),
            url: "http://10.1.2.3:8080/api/user?id=1".to_string(),
            headers: vec![],
            body: String::new(),
            body_encoding: snapshot::ENCODING_UTF8.to_string(),
        },
        response: ResponseRecord {
            status: 200,
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: r#"{"url":"http://10.1.2.3:8080/api/user?id=1"}"#.to_string(),
            body_encoding: snapshot::ENCODING_UTF8.to_string(),
        },
    };
    write_snapshot(&dir, &snap);

    let mut resources = ResourceStore::open(&dir.join("resources")).unwrap();
    resources
        .store(
            b"PNGDATA",
            "http://10.1.2.3:8080",
            "/img/a.png",
            "image/png",
        )
        .unwrap();
    resources.save_index().unwrap();

    let state = ReplayState::new(dir.clone(), RewriteRule::Relative).unwrap();
    let listener = free_listener().await;
    let replay_addr = listener.local_addr().unwrap();
    tokio::spawn(replay_accept_loop(listener, state));

    let c = client();
    let base = format!("http://127.0.0.1:{}", replay_addr.port());

    // 1. method+path 命中（Host 为本地，走兜底匹配），body 被改写为相对地址
    let resp = c
        .get(format!("{base}/api/user").parse().unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let text = String::from_utf8(resp.collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert_eq!(text, r#"{"url":"/api/user?id=1"}"#);

    // 2. 静态资源
    let resp = c
        .get(format!("{base}/img/a.png").parse().unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.collect().await.unwrap().to_bytes().to_vec();
    assert_eq!(bytes, b"PNGDATA");

    // 3. 未记录 → 404
    let resp = c
        .get(format!("{base}/nope").parse().unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn replay_picks_latest_when_ambiguous() {
    let dir = temp_dir("replay-latest");

    let mut older = Snapshot {
        id: "000001".to_string(),
        origin: "http://10.1.2.3:8080".to_string(),
        recorded_at: "2026-08-01T00:00:00Z".to_string(),
        duration_ms: 1,
        request: RequestRecord {
            method: "GET".to_string(),
            url: "http://10.1.2.3:8080/api/user?id=1".to_string(),
            headers: vec![],
            body: String::new(),
            body_encoding: snapshot::ENCODING_UTF8.to_string(),
        },
        response: ResponseRecord {
            status: 200,
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: "\"OLD\"".to_string(),
            body_encoding: snapshot::ENCODING_UTF8.to_string(),
        },
    };
    write_snapshot(&dir, &older);
    older.id = "000002".to_string();
    older.request.url = "http://10.1.2.3:8080/api/user?id=2".to_string();
    older.response.body = "\"NEW\"".to_string();
    write_snapshot(&dir, &older);

    let state = ReplayState::new(dir.clone(), RewriteRule::None).unwrap();
    let listener = free_listener().await;
    let replay_addr = listener.local_addr().unwrap();
    tokio::spawn(replay_accept_loop(listener, state));

    let c = client();
    let url = format!("http://127.0.0.1:{}/api/user", replay_addr.port());
    let resp = c.get(url.parse().unwrap()).await.unwrap();
    let text = String::from_utf8(resp.collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert_eq!(text, "\"NEW\"", "歧义时应取最新录制快照");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn record_returns_502_for_unreachable_upstream() {
    let dir = temp_dir("record-502");
    let state =
        RecordState::new(dir.clone(), false, box_proxy::config::RecordFilter::all()).unwrap();
    let listener = free_listener().await;
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(record_accept_loop(listener, state));

    // 找一个几乎肯定无服务的端口
    let dead_listener = free_listener().await;
    let dead_port = dead_listener.local_addr().unwrap().port();
    drop(dead_listener);

    let target = format!("http://127.0.0.1:{dead_port}/api/x");
    let resp = raw_proxy_get(proxy_addr, &target).await;
    assert!(resp.starts_with("HTTP/1.1 502"), "应返回 502: {resp}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn record_filters_by_config_rules() {
    let business = spawn_origin().await;
    let noise = spawn_origin().await;
    let dir = temp_dir("record-filter");

    // host 精确项 + 正则项取并集；noise 用独立端口模拟系统噪音
    let filter = box_proxy::config::RecordFilter::with_rules(
        vec![format!("127.0.0.1:{}", business.port())],
        vec![r"^10\.1\.2\.\d+:\d+$".to_string()],
    )
    .unwrap();
    let state = RecordState::new(dir.clone(), false, filter).unwrap();
    let listener = free_listener().await;
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(record_accept_loop(listener, state));

    let target_business = format!("http://127.0.0.1:{}/api/user", business.port());
    let target_noise = format!("http://127.0.0.1:{}/api/user", noise.port());
    let resp = raw_proxy_get(proxy_addr, &target_business).await;
    assert!(
        resp.contains("\"name\":\"x\""),
        "白名单 host 应正常转发: {resp}"
    );
    let resp = raw_proxy_get(proxy_addr, &target_noise).await;
    assert!(
        resp.contains("\"name\":\"x\""),
        "非白名单 host 仍应正常转发: {resp}"
    );

    assert!(
        wait_until(
            || !box_proxy::store::load_snapshots(&dir).unwrap().is_empty(),
            Duration::from_secs(3)
        )
        .await,
        "应有快照生成"
    );
    let snaps = box_proxy::store::load_snapshots(&dir).unwrap();
    assert_eq!(snaps.len(), 1, "只应录制匹配过滤规则的请求");
    assert!(
        snaps[0]
            .request
            .url
            .contains(&format!(":{}", business.port()))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn record_stores_requested_assets_to_resources() {
    let origin = spawn_origin_asset().await;
    let dir = temp_dir("record-asset");
    let state =
        RecordState::new(dir.clone(), false, box_proxy::config::RecordFilter::all()).unwrap();
    let listener = free_listener().await;
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(record_accept_loop(listener, state));

    let target = format!("http://127.0.0.1:{}/img/a.png", origin.port());
    let resp = raw_proxy_get(proxy_addr, &target).await;
    assert!(resp.contains("FAKE-PNG-BYTES"), "图片应原样回传");

    let resource_file = dir
        .join("resources")
        .join(format!("127.0.0.1_{}", origin.port()))
        .join("img")
        .join("a.png");
    assert!(
        wait_until(|| resource_file.is_file(), Duration::from_secs(3)).await,
        "直接请求的图片应落入 resources/ 目录"
    );
    assert_eq!(std::fs::read(&resource_file).unwrap(), b"FAKE-PNG-BYTES");

    // 重放：静态资源路径可直接访问
    let state = ReplayState::new(dir.clone(), RewriteRule::None).unwrap();
    let listener = free_listener().await;
    let replay_addr = listener.local_addr().unwrap();
    tokio::spawn(replay_accept_loop(listener, state));
    let c = client();
    let resp = c
        .get(
            format!("http://127.0.0.1:{}/img/a.png", replay_addr.port())
                .parse()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.collect().await.unwrap().to_bytes().to_vec();
    assert_eq!(bytes, b"FAKE-PNG-BYTES");

    let _ = std::fs::remove_dir_all(&dir);
}
