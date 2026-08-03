use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::PrivateKeyDer;
use std::io::Write;
use tape::download::ResourceStore;
use tape::record::{RecordState, accept_loop as record_accept_loop};
use tape::replay::{ReplayState, accept_loop as replay_accept_loop};
use tape::rewrite::RewriteRule;
use tape::snapshot::{self, RequestRecord, ResponseRecord, Snapshot};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

type TestClient = Client<HttpConnector, Full<Bytes>>;

fn client() -> TestClient {
    Client::builder(TokioExecutor::new()).build_http()
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tape-it-{tag}-{}-{}",
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

/// 本地 mock HTTPS 上游：自签证书，任何请求返回固定 JSON。
async fn spawn_origin_tls() -> SocketAddr {
    let certified_key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert_der = certified_key.cert.der().clone();
    let key_der = certified_key.signing_key.serialize_der();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], PrivateKeyDer::Pkcs8(key_der.into()))
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = free_listener().await;
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(stream).await else {
                    return;
                };
                let io = TokioIo::new(tls);
                let svc = service_fn(|_req: Request<Incoming>| async {
                    Ok::<_, std::convert::Infallible>(
                        Response::builder()
                            .status(200)
                            .header("content-type", "application/json")
                            .body(Full::new(Bytes::from_static(b"{\"secure\":true}")))
                            .unwrap(),
                    )
                });
                let _ = http1::Builder::new().serve_connection(io, svc).await;
            });
        }
    });
    addr
}

/// 本地 mock 上游：回显收到的 Host 头。
async fn spawn_origin_echo_host() -> SocketAddr {
    let listener = free_listener().await;
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let io = TokioIo::new(stream);
            let svc = service_fn(|req: Request<Incoming>| async move {
                let host = req
                    .headers()
                    .get(hyper::header::HOST)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let body = format!("HOST={host}");
                Ok::<_, std::convert::Infallible>(
                    Response::builder()
                        .status(200)
                        .header("content-type", "text/plain")
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

/// 本地 mock 上游：固定返回 302 跳转到另一主机。
async fn spawn_origin_redirect() -> SocketAddr {
    let listener = free_listener().await;
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let io = TokioIo::new(stream);
            let svc = service_fn(|_req: Request<Incoming>| async {
                Ok::<_, std::convert::Infallible>(
                    Response::builder()
                        .status(302)
                        .header("location", "http://10.9.9.9:9999/landing")
                        .header("content-type", "text/plain")
                        .body(Full::new(Bytes::from_static(b"redirecting")))
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

/// 本地 mock 上游：返回含多种链接形式的 HTML 页面。
async fn spawn_origin_page() -> SocketAddr {
    let listener = free_listener().await;
    let addr = listener.local_addr().unwrap();
    let html = r#"<html><head><link rel="stylesheet" href="/assets/website/index.css"><script src="//img.alicdn.com/a.js"></script></head><body><a href="https://www.test.com/x">x</a><img src="http://cdn.test.com/img/a.png"></body></html>"#;
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let io = TokioIo::new(stream);
            let svc = service_fn(move |_req: Request<Incoming>| async move {
                Ok::<_, std::convert::Infallible>(
                    Response::builder()
                        .status(200)
                        .header("content-type", "text/html")
                        .body(Full::new(Bytes::from(html)))
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

/// 本地 mock 上游：返回 gzip 压缩的 HTML，并回显收到的 Accept-Encoding。
async fn spawn_origin_gzip_page() -> SocketAddr {
    let listener = free_listener().await;
    let addr = listener.local_addr().unwrap();
    let html =
        r#"<script src="//img.alicdn.com/a.js"></script><a href="https://www.test.com/x">x</a>"#;
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let io = TokioIo::new(stream);
            let svc = service_fn(move |req: Request<Incoming>| async move {
                let ae = req
                    .headers()
                    .get(hyper::header::ACCEPT_ENCODING)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let mut enc =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                enc.write_all(html.as_bytes()).unwrap();
                let gz = enc.finish().unwrap();
                Ok::<_, std::convert::Infallible>(
                    Response::builder()
                        .status(200)
                        .header("content-type", "text/html")
                        .header("content-encoding", "gzip")
                        .header("x-seen-accept-encoding", ae)
                        .body(Full::new(Bytes::from(gz)))
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

fn gunzip(data: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data)
        .read_to_end(&mut out)
        .unwrap();
    out
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

/// 通过 TCP 原始发送任意请求行（模拟直连或 URL 前缀式访问）。
async fn raw_request(addr: SocketAddr, request_line: &str, host: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let req = format!("{request_line}\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    String::from_utf8_lossy(&buf).to_string()
}

async fn raw_request_with_header(
    addr: SocketAddr,
    request_line: &str,
    host: &str,
    extra: &str,
) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let req = format!("{request_line}\r\nHost: {host}\r\n{extra}Connection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    String::from_utf8_lossy(&buf).to_string()
}

fn write_snapshot(dir: &Path, snap: &Snapshot) {
    let origin_dir = dir
        .join("snapshots")
        .join(tape::store::origin_dir_name(&snap.origin));
    std::fs::create_dir_all(&origin_dir).unwrap();
    let file = origin_dir.join(format!(
        "{}-{}-{}.json",
        snap.id,
        snap.request.method,
        tape::store::path_hash(&snap.request.url)
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
    let state = RecordState::new(dir.clone(), false, tape::config::RecordFilter::all()).unwrap();
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

    let snaps = tape::store::load_snapshots(&dir).unwrap();
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
async fn record_prefix_style_rewrites_redirect_back_to_tape() {
    let origin = spawn_origin_redirect().await;
    let dir = temp_dir("record-redir");
    let state = RecordState::new(dir.clone(), false, tape::config::RecordFilter::all()).unwrap();
    let listener = free_listener().await;
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(record_accept_loop(listener, state));

    // 前缀式请求：响应中的 Location 应改写成“回到 tape 的前缀式地址”
    let resp = raw_request(
        proxy_addr,
        &format!("GET /http://127.0.0.1:{}/start HTTP/1.1", origin.port()),
        &format!("127.0.0.1:{}", proxy_addr.port()),
    )
    .await;
    let lower = resp.to_lowercase();
    assert!(lower.contains("302"), "应为 302: {resp}");
    assert!(
        lower.contains(&format!(
            "location: http://127.0.0.1:{}/http://10.9.9.9:9999/landing",
            proxy_addr.port()
        )),
        "Location 应改回 tape 前缀式地址: {resp}"
    );

    // 快照仍保存原始响应（录制保真）
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
    let snaps = tape::store::load_snapshots(&dir).unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].response.status, 302);
    assert!(
        snaps[0]
            .response
            .headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("location") && v == "http://10.9.9.9:9999/landing"),
        "快照应保留原始 Location"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn record_absolute_form_keeps_location_untouched() {
    let origin = spawn_origin_redirect().await;
    let dir = temp_dir("record-redir-abs");
    let state = RecordState::new(dir.clone(), false, tape::config::RecordFilter::all()).unwrap();
    let listener = free_listener().await;
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(record_accept_loop(listener, state));

    // absolute-form（标准代理）：Location 原样回传，由代理客户端重新走 tape
    let target = format!("http://127.0.0.1:{}/start", origin.port());
    let resp = raw_proxy_get(proxy_addr, &target).await;
    let lower = resp.to_lowercase();
    assert!(lower.contains("302"));
    assert!(
        lower.contains("location: http://10.9.9.9:9999/landing"),
        "absolute-form 不应改写 Location: {resp}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn record_prefix_style_rewrites_page_links_back_to_tape() {
    let origin = spawn_origin_page().await;
    let dir = temp_dir("record-page");
    let state = RecordState::new(dir.clone(), false, tape::config::RecordFilter::all()).unwrap();
    let listener = free_listener().await;
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(record_accept_loop(listener, state));

    // 前缀式请求：页面里的绝对链接、协议相对链接都改回 tape 前缀式地址
    let resp = raw_request(
        proxy_addr,
        &format!("GET /http://127.0.0.1:{}/page HTTP/1.1", origin.port()),
        &format!("127.0.0.1:{}", proxy_addr.port()),
    )
    .await;
    let port = proxy_addr.port();
    assert!(
        resp.contains(&format!(
            r#"src="http://127.0.0.1:{port}/http://img.alicdn.com/a.js""#
        )),
        "协议相对链接应改回 tape: {resp}"
    );
    assert!(
        resp.contains(&format!(
            r#"href="http://127.0.0.1:{port}/https://www.test.com/x""#
        )),
        "https 绝对链接应改回 tape: {resp}"
    );
    assert!(
        resp.contains(&format!(
            r#"src="http://127.0.0.1:{port}/http://cdn.test.com/img/a.png""#
        )),
        "http 绝对链接应改回 tape: {resp}"
    );
    assert!(
        resp.contains(&format!(
            r#"href="http://127.0.0.1:{port}/http://127.0.0.1:{}/assets/website/index.css""#,
            origin.port()
        )),
        "root-relative 样式表应改回 tape 前缀式地址: {resp}"
    );

    // 快照保存原始页面（录制保真）
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
    let snaps = tape::store::load_snapshots(&dir).unwrap();
    assert!(snaps[0].response.body.contains("//img.alicdn.com/a.js"));
    assert!(snaps[0].response.body.contains("https://www.test.com/x"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn record_rewrites_compressed_page_and_forces_identity() {
    let origin = spawn_origin_gzip_page().await;
    let dir = temp_dir("record-gzip");
    let state = RecordState::new(dir.clone(), false, tape::config::RecordFilter::all()).unwrap();
    let listener = free_listener().await;
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(record_accept_loop(listener, state));

    // 模拟浏览器：带 Accept-Encoding 前缀式访问 gzip 上游
    let c = client();
    let base = format!("http://127.0.0.1:{}", proxy_addr.port());
    let req = hyper::Request::builder()
        .uri(
            format!("{base}/http://127.0.0.1:{}/page", origin.port())
                .parse::<hyper::Uri>()
                .unwrap(),
        )
        .header(hyper::header::ACCEPT_ENCODING, "gzip, br")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let resp = c.request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // 上游应收到 identity（否则压缩体无法改写）
    assert_eq!(
        resp.headers()
            .get("x-seen-accept-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("identity")
    );
    // 回传仍是 gzip，但内容已被改写
    assert_eq!(
        resp.headers()
            .get(hyper::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("gzip")
    );
    let body = resp.collect().await.unwrap().to_bytes().to_vec();
    let plain = String::from_utf8(gunzip(&body)).unwrap();
    let port = proxy_addr.port();
    assert!(
        plain.contains(&format!(
            r#"src="http://127.0.0.1:{port}/http://img.alicdn.com/a.js""#
        )),
        "压缩页面里的协议相对链接应被改写: {plain}"
    );
    assert!(
        plain.contains(&format!(
            r#"href="http://127.0.0.1:{port}/https://www.test.com/x""#
        )),
        "压缩页面里的 https 链接应被改写: {plain}"
    );

    // 快照保留原始 gzip 字节（录制保真）
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
    let snaps = tape::store::load_snapshots(&dir).unwrap();
    assert!(
        snaps[0]
            .response
            .headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-encoding") && v == "gzip"),
        "快照应记录 content-encoding: gzip"
    );
    let raw =
        tape::snapshot::decode_body(&snaps[0].response.body, &snaps[0].response.body_encoding);
    assert_eq!(&raw[..2], &[0x1f, 0x8b], "快照 body 应为原始 gzip 字节");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn replay_rewrites_compressed_snapshot() {
    let dir = temp_dir("replay-gzip");

    let html = r#"<script src="//img.alicdn.com/a.js"></script>"#;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(html.as_bytes()).unwrap();
    let gz = enc.finish().unwrap();
    let (body, body_encoding) = tape::snapshot::encode_body(&gz);

    let snap = Snapshot {
        id: "000001".to_string(),
        origin: "http://10.1.2.3:8080".to_string(),
        recorded_at: "2026-08-01T00:00:00Z".to_string(),
        duration_ms: 1,
        request: RequestRecord {
            method: "GET".to_string(),
            url: "http://10.1.2.3:8080/page".to_string(),
            headers: vec![],
            body: String::new(),
            body_encoding: snapshot::ENCODING_UTF8.to_string(),
        },
        response: ResponseRecord {
            status: 200,
            headers: vec![
                ("content-type".to_string(), "text/html".to_string()),
                ("content-encoding".to_string(), "gzip".to_string()),
            ],
            body,
            body_encoding,
        },
    };
    write_snapshot(&dir, &snap);

    let state = ReplayState::new(dir.clone(), RewriteRule::Relative).unwrap();
    let listener = free_listener().await;
    let replay_addr = listener.local_addr().unwrap();
    tokio::spawn(replay_accept_loop(listener, state));

    // 前缀式请求：压缩快照解压改写后仍按 gzip 回传
    let c = client();
    let base = format!("http://127.0.0.1:{}", replay_addr.port());
    let resp = c
        .get(format!("{base}/http://10.1.2.3:8080/page").parse().unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(hyper::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("gzip")
    );
    let body = resp.collect().await.unwrap().to_bytes().to_vec();
    let plain = String::from_utf8(gunzip(&body)).unwrap();
    assert!(
        plain.contains(&format!(
            r#"src="http://127.0.0.1:{}/http://img.alicdn.com/a.js""#,
            replay_addr.port()
        )),
        "replay 压缩快照应改写链接: {plain}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn replay_prefix_style_rewrites_page_links_back_to_tape() {
    let dir = temp_dir("replay-page");

    let snap = Snapshot {
        id: "000001".to_string(),
        origin: "http://10.1.2.3:8080".to_string(),
        recorded_at: "2026-08-01T00:00:00Z".to_string(),
        duration_ms: 1,
        request: RequestRecord {
            method: "GET".to_string(),
            url: "http://10.1.2.3:8080/page".to_string(),
            headers: vec![],
            body: String::new(),
            body_encoding: snapshot::ENCODING_UTF8.to_string(),
        },
        response: ResponseRecord {
            status: 200,
            headers: vec![("content-type".to_string(), "text/html".to_string())],
            body: r#"<link rel="stylesheet" href="/assets/website/index.css"><script src="//img.alicdn.com/a.js"></script><a href="https://www.test.com/x">x</a>"#.to_string(),
            body_encoding: snapshot::ENCODING_UTF8.to_string(),
        },
    };
    write_snapshot(&dir, &snap);

    let state = ReplayState::new(dir.clone(), RewriteRule::Relative).unwrap();
    let listener = free_listener().await;
    let replay_addr = listener.local_addr().unwrap();
    tokio::spawn(replay_accept_loop(listener, state));

    let resp = raw_request(
        replay_addr,
        "GET /http://10.1.2.3:8080/page HTTP/1.1",
        &format!("127.0.0.1:{}", replay_addr.port()),
    )
    .await;
    let port = replay_addr.port();
    assert!(
        resp.contains(&format!(
            r#"src="http://127.0.0.1:{port}/http://img.alicdn.com/a.js""#
        )),
        "replay 也应改写协议相对链接: {resp}"
    );
    assert!(
        resp.contains(&format!(
            r#"href="http://127.0.0.1:{port}/https://www.test.com/x""#
        )),
        "replay 也应改写 https 绝对链接: {resp}"
    );
    assert!(
        resp.contains(&format!(
            r#"href="http://127.0.0.1:{port}/http://10.1.2.3:8080/assets/website/index.css""#
        )),
        "replay 也应改写 root-relative 样式表: {resp}"
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
async fn replay_prefix_style_rewrites_redirect_back_to_tape() {
    let dir = temp_dir("replay-redir");

    let snap = Snapshot {
        id: "000001".to_string(),
        origin: "http://10.1.2.3:8080".to_string(),
        recorded_at: "2026-08-01T00:00:00Z".to_string(),
        duration_ms: 1,
        request: RequestRecord {
            method: "GET".to_string(),
            url: "http://10.1.2.3:8080/start".to_string(),
            headers: vec![],
            body: String::new(),
            body_encoding: snapshot::ENCODING_UTF8.to_string(),
        },
        response: ResponseRecord {
            status: 302,
            headers: vec![
                (
                    "location".to_string(),
                    "https://www.dingtalk.com/".to_string(),
                ),
                ("content-type".to_string(), "text/plain".to_string()),
            ],
            body: "redirecting".to_string(),
            body_encoding: snapshot::ENCODING_UTF8.to_string(),
        },
    };
    write_snapshot(&dir, &snap);

    let state = ReplayState::new(dir.clone(), RewriteRule::Relative).unwrap();
    let listener = free_listener().await;
    let replay_addr = listener.local_addr().unwrap();
    tokio::spawn(replay_accept_loop(listener, state));

    // 前缀式请求：Location 改回 tape 前缀式地址，跳转链留在 tape 内
    let resp = raw_request(
        replay_addr,
        "GET /http://10.1.2.3:8080/start HTTP/1.1",
        &format!("127.0.0.1:{}", replay_addr.port()),
    )
    .await;
    let lower = resp.to_lowercase();
    assert!(lower.contains("302"));
    assert!(
        lower.contains(&format!(
            "location: http://127.0.0.1:{}/https://www.dingtalk.com/",
            replay_addr.port()
        )),
        "前缀式 Location 应改回 tape: {resp}"
    );

    // absolute-form（标准代理）：Location 原样，客户端重新走代理
    let resp = raw_proxy_get(replay_addr, "http://10.1.2.3:8080/start").await;
    let lower = resp.to_lowercase();
    assert!(lower.contains("302"));
    assert!(
        lower.contains("location: https://www.dingtalk.com/"),
        "absolute-form 不应改写 Location: {resp}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn replay_matches_queries_in_proxy_and_prefix_forms() {
    let dir = temp_dir("replay-query");

    // 快照 URL 与录制侧一致：带 query（录制时索引按 method+path 去 query）
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

    // 静态资源：索引路径去 query（录制侧 url_path），查询侧带 query 也应命中
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

    let state = ReplayState::new(dir.clone(), RewriteRule::None).unwrap();
    let listener = free_listener().await;
    let replay_addr = listener.local_addr().unwrap();
    tokio::spawn(replay_accept_loop(listener, state));

    // absolute-form（标准代理）带 query：应命中快照（按 method+path 匹配，忽略 query）
    let resp = raw_proxy_get(replay_addr, "http://10.1.2.3:8080/api/user?id=1").await;
    assert!(
        resp.starts_with("HTTP/1.1 200"),
        "代理式带 query 应命中快照: {resp}"
    );
    assert!(
        resp.contains(r#"{"url":"http://10.1.2.3:8080/api/user?id=1"}"#),
        "快照 body 应原样返回: {resp}"
    );

    // 前缀式带 query：应命中快照
    let resp = raw_request(
        replay_addr,
        "GET /http://10.1.2.3:8080/api/user?id=1 HTTP/1.1",
        &format!("127.0.0.1:{}", replay_addr.port()),
    )
    .await;
    assert!(
        resp.starts_with("HTTP/1.1 200"),
        "前缀式带 query 应命中快照: {resp}"
    );
    assert!(
        resp.contains(&format!(
            r#"{{"url":"http://127.0.0.1:{}/http://10.1.2.3:8080/api/user?id=1"}}"#,
            replay_addr.port()
        )),
        "前缀式带 query 应命中并把链接改回 tape: {resp}"
    );

    // 前缀式静态资源带 query（缓存破坏参数）：应命中资源
    let resp = raw_request(
        replay_addr,
        "GET /http://10.1.2.3:8080/img/a.png?v=2 HTTP/1.1",
        &format!("127.0.0.1:{}", replay_addr.port()),
    )
    .await;
    assert!(
        resp.starts_with("HTTP/1.1 200"),
        "前缀式资源带 query 应命中: {resp}"
    );
    assert!(resp.contains("PNGDATA"), "应返回资源内容: {resp}");

    // absolute-form 静态资源带 query：同样应命中
    let resp = raw_proxy_get(replay_addr, "http://10.1.2.3:8080/img/a.png?v=2").await;
    assert!(
        resp.starts_with("HTTP/1.1 200"),
        "代理式资源带 query 应命中: {resp}"
    );
    assert!(resp.contains("PNGDATA"), "应返回资源内容: {resp}");

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
async fn replay_resources_do_not_cross_origins() {
    let dir = temp_dir("replay-res-origin");

    // 两个站点存在同路径资源 /img/a.png 但内容不同
    let mut resources = ResourceStore::open(&dir.join("resources")).unwrap();
    resources
        .store(
            b"ORIGIN-A",
            "http://10.1.2.3:8080",
            "/img/a.png",
            "image/png",
        )
        .unwrap();
    resources
        .store(
            b"ORIGIN-B",
            "http://10.2.3.4:8080",
            "/img/a.png",
            "image/png",
        )
        .unwrap();
    resources.save_index().unwrap();

    let state = ReplayState::new(dir.clone(), RewriteRule::None).unwrap();
    let listener = free_listener().await;
    let replay_addr = listener.local_addr().unwrap();
    tokio::spawn(replay_accept_loop(listener, state));

    // 前缀式请求 A 的资源：应命中 A 的内容，不串到 B
    let resp = raw_request(
        replay_addr,
        "GET /http://10.1.2.3:8080/img/a.png HTTP/1.1",
        &format!("127.0.0.1:{}", replay_addr.port()),
    )
    .await;
    assert!(
        resp.starts_with("HTTP/1.1 200"),
        "站点 A 资源应命中: {resp}"
    );
    assert!(resp.contains("ORIGIN-A"), "站点 A 应返回自身资源: {resp}");
    assert!(!resp.contains("ORIGIN-B"), "不得串到站点 B 的资源: {resp}");

    // 前缀式请求 B 的资源：应命中 B 的内容
    let resp = raw_request(
        replay_addr,
        "GET /http://10.2.3.4:8080/img/a.png HTTP/1.1",
        &format!("127.0.0.1:{}", replay_addr.port()),
    )
    .await;
    assert!(
        resp.starts_with("HTTP/1.1 200"),
        "站点 B 资源应命中: {resp}"
    );
    assert!(resp.contains("ORIGIN-B"), "站点 B 应返回自身资源: {resp}");
    assert!(!resp.contains("ORIGIN-A"), "不得串到站点 A 的资源: {resp}");

    // 直接访问（Host 是 tape 自身地址）：回退按路径匹配，取最早录制的一条（A）
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
    assert_eq!(bytes, b"ORIGIN-A", "直接访问回退应命中最早录制的资源");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn replay_resources_reject_non_get_methods() {
    let dir = temp_dir("replay-res-method");
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

    let state = ReplayState::new(dir.clone(), RewriteRule::None).unwrap();
    let listener = free_listener().await;
    let replay_addr = listener.local_addr().unwrap();
    tokio::spawn(replay_accept_loop(listener, state));

    // POST 到资源路径：应 405 而非把资源当 200 返回
    let resp = raw_request(
        replay_addr,
        "POST /img/a.png HTTP/1.1",
        &format!("127.0.0.1:{}", replay_addr.port()),
    )
    .await;
    assert!(resp.starts_with("HTTP/1.1 405"), "POST 资源应 405: {resp}");
    assert!(
        resp.to_lowercase().contains("allow: get, head"),
        "405 响应应带 Allow: GET, HEAD: {resp}"
    );

    // GET 仍正常返回资源
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
    assert_eq!(bytes, b"PNGDATA");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn record_returns_502_for_unreachable_upstream() {
    let dir = temp_dir("record-502");
    let state = RecordState::new(dir.clone(), false, tape::config::RecordFilter::all()).unwrap();
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
    let filter = tape::config::RecordFilter::with_rules(
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
            || !tape::store::load_snapshots(&dir).unwrap().is_empty(),
            Duration::from_secs(3)
        )
        .await,
        "应有快照生成"
    );
    let snaps = tape::store::load_snapshots(&dir).unwrap();
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
async fn record_captures_prefix_style_requests() {
    let origin = spawn_origin().await;
    let dir = temp_dir("record-prefix");
    let state = RecordState::new(dir.clone(), false, tape::config::RecordFilter::all()).unwrap();
    let listener = free_listener().await;
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(record_accept_loop(listener, state));

    // 前缀式：GET /http://<origin>/api/user?x=1，Host 是 tape 自身
    let target = format!("http://127.0.0.1:{}/api/user?x=1", origin.port());
    let resp = raw_request(
        proxy_addr,
        &format!("GET /{target} HTTP/1.1"),
        &format!("127.0.0.1:{}", proxy_addr.port()),
    )
    .await;
    assert!(resp.contains("\"name\":\"x\""), "前缀式应正常转发: {resp}");

    assert!(
        wait_until(
            || !tape::store::load_snapshots(&dir).unwrap().is_empty(),
            Duration::from_secs(3)
        )
        .await,
        "应有快照生成"
    );
    let snaps = tape::store::load_snapshots(&dir).unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].request.url, target, "快照应记录解析出的目标 URL");
    assert_eq!(snaps[0].request.method, "GET");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn record_captures_https_upstream() {
    // 自签证书 + 跳过校验，模拟专网 https 上游
    unsafe { std::env::set_var("TAPE_INSECURE_TLS", "1") };

    let origin = spawn_origin_tls().await;
    let dir = temp_dir("record-https");
    let state = RecordState::new(dir.clone(), false, tape::config::RecordFilter::all()).unwrap();
    let listener = free_listener().await;
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(record_accept_loop(listener, state));

    // 前缀式 https：GET /https://<origin>/api/user
    let target = format!("https://127.0.0.1:{}/api/user", origin.port());
    let resp = raw_request(
        proxy_addr,
        &format!("GET /{target} HTTP/1.1"),
        &format!("127.0.0.1:{}", proxy_addr.port()),
    )
    .await;
    assert!(
        resp.contains("\"secure\":true"),
        "https 前缀式应正常转发: {resp}"
    );

    // 浏览器会把前缀中的 : 百分号编码（%3A / %2F），也应识别
    let encoded = target.replace("://", "%3A%2F%2F").replace(':', "%3A");
    let resp = raw_request(
        proxy_addr,
        &format!("GET /{encoded} HTTP/1.1"),
        &format!("127.0.0.1:{}", proxy_addr.port()),
    )
    .await;
    assert!(
        resp.contains("\"secure\":true"),
        "编码后的 https 前缀应正常转发: {resp}"
    );

    // absolute-form https
    let resp = raw_proxy_get(proxy_addr, &target).await;
    assert!(
        resp.contains("\"secure\":true"),
        "absolute-form https 应正常转发: {resp}"
    );

    assert!(
        wait_until(
            || tape::store::load_snapshots(&dir).unwrap().len() >= 3,
            Duration::from_secs(3)
        )
        .await,
        "应生成 3 条 https 快照"
    );
    let snaps = tape::store::load_snapshots(&dir).unwrap();
    assert_eq!(snaps.len(), 3);
    assert!(
        snaps.iter().all(|s| s.origin.starts_with("https://")),
        "快照 origin 应为 https"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn record_forwards_single_correct_host_header() {
    let origin = spawn_origin_echo_host().await;
    let dir = temp_dir("record-host");
    let state = RecordState::new(dir.clone(), false, tape::config::RecordFilter::all()).unwrap();
    let listener = free_listener().await;
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(record_accept_loop(listener, state));

    // 前缀式请求：客户端 Host 是代理自身地址，上游必须只收到目标 Host
    let target = format!("http://127.0.0.1:{}/api/user", origin.port());
    let resp = raw_request(
        proxy_addr,
        &format!("GET /{target} HTTP/1.1"),
        &format!("127.0.0.1:{}", proxy_addr.port()),
    )
    .await;
    assert!(
        resp.contains(&format!("HOST=127.0.0.1:{}", origin.port())),
        "上游应只收到目标 Host: {resp}"
    );
    assert!(
        !resp.contains(&format!("HOST=127.0.0.1:{}", proxy_addr.port())),
        "不应出现代理自身地址的 Host: {resp}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn record_injects_referer_for_resource_requests() {
    // 本地 mock 上游：回显收到的 Referer 头
    let listener = free_listener().await;
    let origin = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let io = TokioIo::new(stream);
            let svc = service_fn(|req: Request<Incoming>| async move {
                // 回显时去掉 scheme：否则回显内容里的绝对 URL 会被 tape 的前缀式响应改写二次加工，
                // 无法直接断言原始值（127.0.0.1 本地地址除外，改写会跳过本地主机）。
                let referer = req
                    .headers()
                    .get(hyper::header::REFERER)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .replace("http://", "");
                let body = format!("REFERER=[{referer}]");
                Ok::<_, std::convert::Infallible>(
                    Response::builder()
                        .status(200)
                        .header("content-type", "text/plain")
                        .body(Full::new(Bytes::from(body)))
                        .unwrap(),
                )
            });
            tokio::spawn(async move {
                let _ = http1::Builder::new().serve_connection(io, svc).await;
            });
        }
    });

    let dir = temp_dir("record-referer");
    let state = RecordState::new(dir.clone(), false, tape::config::RecordFilter::all()).unwrap();
    let listener = free_listener().await;
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(record_accept_loop(listener, state));

    // 静态资源请求且客户端未带 Referer：应兜底注入页面 origin 的 Referer（防盗链）
    let resp = raw_request(
        proxy_addr,
        &format!("GET /http://127.0.0.1:{}/img/a.png HTTP/1.1", origin.port()),
        &format!("127.0.0.1:{}", proxy_addr.port()),
    )
    .await;
    assert!(
        resp.contains(&format!("REFERER=[127.0.0.1:{}/]", origin.port())),
        "资源请求应兜底注入 origin Referer: {resp}"
    );

    // 非资源请求：不注入 Referer
    let resp = raw_request(
        proxy_addr,
        &format!("GET /http://127.0.0.1:{}/api/user HTTP/1.1", origin.port()),
        &format!("127.0.0.1:{}", proxy_addr.port()),
    )
    .await;
    assert!(
        resp.contains("REFERER=[]"),
        "非资源请求不应注入 Referer: {resp}"
    );

    // 客户端已带 Referer：原样保留，不改写
    let client_referer = "http://client.example/from-browser".to_string();
    let resp = raw_request_with_header(
        proxy_addr,
        &format!("GET /http://127.0.0.1:{}/img/a.png HTTP/1.1", origin.port()),
        &format!("127.0.0.1:{}", proxy_addr.port()),
        &format!("Referer: {client_referer}\r\n"),
    )
    .await;
    assert!(
        resp.contains(&format!(
            "REFERER=[{}]",
            client_referer.replace("http://", "")
        )),
        "客户端自带 Referer 应原样保留: {resp}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn record_rejects_plain_origin_form() {
    let dir = temp_dir("record-origin-form");
    let state = RecordState::new(dir.clone(), false, tape::config::RecordFilter::all()).unwrap();
    let listener = free_listener().await;
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(record_accept_loop(listener, state));

    // 无法确定目标主机：既不是 absolute-form 也不是前缀式 → 400
    let resp = raw_request(
        proxy_addr,
        "GET /api/user HTTP/1.1",
        &format!("127.0.0.1:{}", proxy_addr.port()),
    )
    .await;
    assert!(resp.starts_with("HTTP/1.1 400"), "应返回 400: {resp}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn replay_serves_prefix_style_requests() {
    let dir = temp_dir("replay-prefix");

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

    let state = ReplayState::new(dir.clone(), RewriteRule::Relative).unwrap();
    let listener = free_listener().await;
    let replay_addr = listener.local_addr().unwrap();
    tokio::spawn(replay_accept_loop(listener, state));

    // 前缀式：/http://10.1.2.3:8080/api/user，Host 是 tape 自身
    let resp = raw_request(
        replay_addr,
        "GET /http://10.1.2.3:8080/api/user HTTP/1.1",
        &format!("127.0.0.1:{}", replay_addr.port()),
    )
    .await;
    assert!(
        resp.starts_with("HTTP/1.1 200"),
        "前缀式应命中快照返回 200: {resp}"
    );
    assert!(
        resp.contains(&format!(
            r#"{{"url":"http://127.0.0.1:{}/http://10.1.2.3:8080/api/user?id=1"}}"#,
            replay_addr.port()
        )),
        "前缀式应命中快照并把链接改回 tape 前缀式地址: {resp}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn record_stores_requested_assets_to_resources() {
    let origin = spawn_origin_asset().await;
    let dir = temp_dir("record-asset");
    let state = RecordState::new(dir.clone(), false, tape::config::RecordFilter::all()).unwrap();
    let listener = free_listener().await;
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(record_accept_loop(listener, state));

    let target = format!("http://127.0.0.1:{}/img/a.png", origin.port());
    let resp = raw_proxy_get(proxy_addr, &target).await;
    assert!(resp.contains("FAKE-PNG-BYTES"), "图片应原样回传");

    let resource_file = dir
        .join("resources")
        .join(format!("http_127.0.0.1_{}", origin.port()))
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

#[tokio::test]
async fn compare_end_to_end_reports_missing_and_changed() {
    use tape::compare::{FeatureMatrix, IgnoreRules, compare_dirs, render_report};

    fn snap(id: &str, url: &str, body: &str, status: u16) -> Snapshot {
        Snapshot {
            id: id.to_string(),
            origin: "http://10.1.2.3:8080".to_string(),
            recorded_at: "2026-08-02T00:00:00Z".to_string(),
            duration_ms: 1,
            request: RequestRecord {
                method: "POST".to_string(),
                url: url.to_string(),
                headers: vec![],
                body: String::new(),
                body_encoding: tape::snapshot::ENCODING_UTF8.to_string(),
            },
            response: ResponseRecord {
                status,
                headers: vec![],
                body: body.to_string(),
                body_encoding: tape::snapshot::ENCODING_UTF8.to_string(),
            },
        }
    }

    let base = temp_dir("compare-base");
    let curr = temp_dir("compare-curr");
    // 基线：搜索（kw=电影）+ 首页
    write_snapshot(
        &base,
        &snap(
            "000001",
            "http://10.1.2.3:8080/api/search/query?kw=电影",
            r#"{"list":["a"]}"#,
            200,
        ),
    );
    write_snapshot(
        &base,
        &snap(
            "000002",
            "http://10.1.2.3:8080/api/home",
            r#"{"banners":2}"#,
            200,
        ),
    );
    // 新版：搜索（kw=电影，响应字段变了）+ 首页缺失
    write_snapshot(
        &curr,
        &snap(
            "000001",
            "http://10.1.2.3:8080/api/search/query?kw=电影",
            r#"{"list":["a","b"]}"#,
            200,
        ),
    );

    let rules = IgnoreRules::default();
    let result = compare_dirs(&base, &curr, &rules).unwrap();
    let matrix = serde_json::json!({
        "module": "首页",
        "entries": [
            {"id": "s", "name": "搜索流程", "steps": [
                {"action": "搜索", "apis": [{"method": "POST", "path": "/api/search/query"}]}
            ]},
            {"id": "h", "name": "首页加载", "steps": [
                {"action": "进首页", "apis": [{"method": "POST", "path": "/api/home"}]}
            ]}
        ]
    });
    let matrix: FeatureMatrix = serde_json::from_value(matrix).unwrap();
    let md = render_report("基线", "新版", &result, Some(&matrix), None, &[]);
    assert!(md.contains("缺失"), "报告应有汇总: {md}");
    assert!(md.contains("❌ 搜索流程"), "搜索响应变更应标问题: {md}");
    assert!(md.contains("$.list"), "差异应定位到字段: {md}");
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&curr);
}

#[tokio::test]
async fn compare_reports_business_flow_assertions() {
    use tape::compare::{
        FeatureMatrix, IgnoreRules, build_sequence_diff, compare_dirs, render_report,
        run_feature_assertions,
    };

    fn snap(id: &str, url: &str, body: &str) -> Snapshot {
        Snapshot {
            id: id.to_string(),
            origin: "http://10.1.2.3:8080".to_string(),
            recorded_at: "2026-08-02T00:00:00Z".to_string(),
            duration_ms: 1,
            request: RequestRecord {
                method: "POST".to_string(),
                url: url.to_string(),
                headers: vec![],
                body: String::new(),
                body_encoding: tape::snapshot::ENCODING_UTF8.to_string(),
            },
            response: ResponseRecord {
                status: 200,
                headers: vec![],
                body: body.to_string(),
                body_encoding: tape::snapshot::ENCODING_UTF8.to_string(),
            },
        }
    }

    let base = temp_dir("m2-base");
    let curr = temp_dir("m2-curr");
    // 基线：搜索返回空列表 → 断言失败在基线侧；新版返回列表 → 断言通过
    write_snapshot(
        &base,
        &snap(
            "000001",
            "http://10.1.2.3:8080/api/search",
            r#"{"data":{"list":[]}}"#,
        ),
    );
    write_snapshot(
        &curr,
        &snap(
            "000001",
            "http://10.1.2.3:8080/api/search",
            r#"{"data":{"list":["a"]}}"#,
        ),
    );

    let rules = IgnoreRules::default();
    let comparisons = compare_dirs(&base, &curr, &rules).unwrap();
    let matrix: FeatureMatrix = serde_json::from_value(serde_json::json!({
        "module": "首页",
        "entries": [{
            "id": "s",
            "name": "搜索流程",
            "steps": [{"action": "搜索", "apis": [{"method": "POST", "path": "/api/search"}]}],
            "expected": [{"path": "$.data.list", "op": "nonEmpty", "desc": "搜索结果非空"}]
        }]
    }))
    .unwrap();
    let sequence = build_sequence_diff(&comparisons);
    let assertions = run_feature_assertions(Some(&matrix), &comparisons);
    let md = render_report(
        "基线",
        "新版",
        &comparisons,
        Some(&matrix),
        sequence.as_ref(),
        &assertions,
    );
    assert!(md.contains("业务结果断言"), "报告应有断言小节: {md}");
    assert!(md.contains("基线 0/1 通过"), "基线断言应失败: {md}");
    assert!(md.contains("新版 1/1 通过"), "新版断言应通过: {md}");
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&curr);
}
