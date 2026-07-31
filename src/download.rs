use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Semaphore};
use tracing::warn;

use crate::record::HttpClient;
use crate::rewrite;

const RESOURCE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg", "css", "js", "mjs", "woff", "woff2",
    "ttf", "eot", "otf", "mp4", "webm", "mp3", "wav", "ogg", "aac", "flac", "pdf", "zip", "gz",
];

const ASSET_DIR_PREFIXES: &[&str] = &[
    "/static/", "/assets/", "/img/", "/images/", "/upload/", "/fonts/", "/media/",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceIndexEntry {
    pub hash: String,
    pub origin: String,
    /// 相对路径（不含前导 /），如 img/a.png
    pub path: String,
    pub content_type: String,
    pub size: usize,
}

/// 静态资源磁盘存储：blob 按内容哈希去重，路径处建硬链接副本。
pub struct ResourceStore {
    root: PathBuf,
    index: Vec<ResourceIndexEntry>,
}

impl ResourceStore {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        let index_path = root.join("index.json");
        let index = if index_path.exists() {
            let text = std::fs::read_to_string(&index_path)?;
            serde_json::from_str(&text).unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(Self {
            root: root.to_path_buf(),
            index,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn index(&self) -> &[ResourceIndexEntry] {
        &self.index
    }

    pub fn store(
        &mut self,
        data: &[u8],
        origin: &str,
        url_path: &str,
        content_type: &str,
    ) -> Result<()> {
        let hash = crate::store::sha256_hex(data);
        let blob_dir = self.root.join("blobs");
        std::fs::create_dir_all(&blob_dir)?;
        let blob_path = blob_dir.join(&hash);
        if !blob_path.exists() {
            std::fs::write(&blob_path, data)?;
        }
        // 索引保留原始相对路径，保证重放按原路径匹配；磁盘副本用跨平台安全文件名
        let rel = url_path.trim_start_matches('/').to_string();
        if self
            .index
            .iter()
            .any(|e| e.origin == origin && e.path == rel)
        {
            return Ok(());
        }
        let disk_rel = disk_rel_path(url_path);
        let target = self
            .root
            .join(crate::store::origin_dir_name(origin))
            .join(&disk_rel);
        if !target.exists() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if std::fs::hard_link(&blob_path, &target).is_err() {
                std::fs::copy(&blob_path, &target)?;
            }
        }
        self.index.push(ResourceIndexEntry {
            hash,
            origin: origin.to_string(),
            path: rel,
            content_type: content_type.to_string(),
            size: data.len(),
        });
        Ok(())
    }

    pub fn save_index(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.index)?;
        std::fs::write(self.root.join("index.json"), json)?;
        Ok(())
    }
}

/// 对文本响应体提取资源链接并下载到本地（失败仅告警，不中断主流程）。
pub async fn download_resources(
    client: &HttpClient,
    store: &Arc<Mutex<ResourceStore>>,
    body_text: &str,
    content_type: &str,
    origin: &str,
) -> Result<()> {
    if !is_text_body(content_type) {
        return Ok(());
    }
    let mut seen = std::collections::HashSet::new();
    let mut candidates: Vec<String> = Vec::new();
    for url in rewrite::extract_http_urls(body_text) {
        if seen.insert(url.clone()) {
            candidates.push(url);
        }
    }
    // 相对路径资源按当前 origin 拼接为绝对地址后同样纳入下载
    for rel in rewrite::extract_relative_asset_paths(body_text) {
        let url = format!("{origin}{rel}");
        if seen.insert(url.clone()) {
            candidates.push(url);
        }
    }
    let candidates: Vec<String> = candidates
        .into_iter()
        .filter(|u| looks_like_resource(u))
        .collect();
    if candidates.is_empty() {
        return Ok(());
    }

    let sem = Arc::new(Semaphore::new(8));
    let mut handles = Vec::new();
    for url in candidates {
        let client = client.clone();
        let store = store.clone();
        let sem = sem.clone();
        let origin = origin.to_string();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.ok();
            if let Err(e) = fetch_and_store(&client, &store, &url, &origin).await {
                warn!("资源下载失败 {}: {}", url, e);
            }
        }));
    }
    for handle in handles {
        handle.await.ok();
    }
    Ok(())
}

async fn fetch_and_store(
    client: &HttpClient,
    store: &Arc<Mutex<ResourceStore>>,
    url: &str,
    origin: &str,
) -> Result<()> {
    let req = Request::builder()
        .method("GET")
        .uri(url)
        .body(Full::new(Bytes::new()))?;
    let resp = tokio::time::timeout(Duration::from_secs(15), client.request(req)).await??;
    if resp.status() != StatusCode::OK {
        return Ok(());
    }
    let content_type = resp
        .headers()
        .get(hyper::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !acceptable_content_type(&content_type) {
        return Ok(());
    }
    let body = resp.collect().await?.to_bytes();
    if body.is_empty() {
        return Ok(());
    }
    let path = rewrite::url_path(url);
    let mut store = store.lock().await;
    store.store(&body, origin, &path, &content_type)?;
    store.save_index()?;
    Ok(())
}

/// 该 Content-Type 是否属于应落盘到 resources/ 的静态资源（直接请求的响应）。
pub fn is_static_asset_type(content_type: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    ct.starts_with("image/")
        || ct.starts_with("font/")
        || ct.starts_with("audio/")
        || ct.starts_with("video/")
        || ct.contains("text/css")
        || ct.contains("javascript")
        || ct.contains("ecmascript")
        || ct.contains("font-woff")
        || ct.contains("x-icon")
}

fn looks_like_resource(url: &str) -> bool {
    let path = rewrite::url_path(url);
    if let Some(ext) = path.rsplit('.').next()
        && !ext.contains('/')
        && RESOURCE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
    {
        return true;
    }
    ASSET_DIR_PREFIXES.iter().any(|p| path.starts_with(p))
}

fn is_text_body(content_type: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    [
        "json",
        "text",
        "html",
        "xml",
        "javascript",
        "css",
        "x-www-form-urlencoded",
        "svg",
    ]
    .iter()
    .any(|k| ct.contains(k))
}

fn acceptable_content_type(content_type: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    !(ct.contains("text/html") || ct.contains("application/json"))
}

/// Windows 保留设备名（含扩展名场景，如 CON.txt），大小写不敏感。
const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// 生成跨平台安全的磁盘相对路径（仅用于 resources/ 下的人工可读副本）：
/// 非法字符替换、尾随点/空格裁剪、路径穿越段剔除、Windows 保留设备名前缀下划线。
fn disk_rel_path(raw_path: &str) -> String {
    let mut segments = Vec::new();
    for seg in raw_path.trim_start_matches('/').split('/') {
        let seg = seg.trim_end_matches([' ', '.']);
        let mut clean: String = seg
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if clean.is_empty() || clean == "." || clean == ".." {
            continue;
        }
        let stem = clean.split('.').next().unwrap_or("").to_ascii_lowercase();
        if WINDOWS_RESERVED.contains(&stem.as_str()) {
            clean = format!("_{clean}");
        }
        segments.push(clean);
    }
    if segments.is_empty() {
        "index".to_string()
    } else {
        segments.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_path_is_cross_platform_safe() {
        assert_eq!(disk_rel_path("/img/a.png"), "img/a.png");
        assert_eq!(disk_rel_path("/a//b/c"), "a/b/c");
        assert_eq!(disk_rel_path("/"), "index");
        assert_eq!(disk_rel_path("/a b?c=d"), "a_b_c_d");
        assert_eq!(disk_rel_path("/../etc/passwd"), "etc/passwd");
        assert_eq!(disk_rel_path("/CON"), "_CON");
        assert_eq!(disk_rel_path("/con.txt"), "_con.txt");
        assert_eq!(disk_rel_path("/NUL/x.png"), "_NUL/x.png");
        assert_eq!(disk_rel_path("/a.png."), "a.png");
        assert_eq!(disk_rel_path("/a b "), "a_b");
        assert_eq!(disk_rel_path("/lpt9.log"), "_lpt9.log");
    }

    #[test]
    fn resource_filter() {
        assert!(looks_like_resource("http://10.1.2.3:8080/img/a.png"));
        assert!(looks_like_resource("http://10.1.2.3:8080/static/js/app.js"));
        assert!(!looks_like_resource("http://10.1.2.3:8080/api/user"));
        assert!(!looks_like_resource("http://10.1.2.3:8080/page.html"));
    }

    #[test]
    fn static_asset_type_detection() {
        assert!(is_static_asset_type("image/png"));
        assert!(is_static_asset_type("text/css; charset=utf-8"));
        assert!(is_static_asset_type("application/javascript"));
        assert!(is_static_asset_type("font/woff2"));
        assert!(!is_static_asset_type("application/json"));
        assert!(!is_static_asset_type("text/html"));
    }

    #[tokio::test]
    async fn store_keeps_original_path_in_index() {
        let dir = std::env::temp_dir().join(format!("box-proxy-res-idx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = ResourceStore::open(&dir).unwrap();
        store
            .store(b"x", "http://10.1.2.3:8080", "/my img/a.png", "image/png")
            .unwrap();
        store.save_index().unwrap();
        assert_eq!(store.index()[0].path, "my img/a.png", "索引应保留原始路径");
        assert!(
            dir.join("10.1.2.3_8080/my_img/a.png").is_file(),
            "磁盘副本用安全文件名"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn store_dedups_by_hash() {
        let dir = std::env::temp_dir().join(format!("box-proxy-res-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mut store = ResourceStore::open(&dir).unwrap();
        store
            .store(
                b"same-content",
                "http://10.1.2.3:8080",
                "/img/a.png",
                "image/png",
            )
            .unwrap();
        store
            .store(
                b"same-content",
                "http://10.1.2.3:8080",
                "/img/b.png",
                "image/png",
            )
            .unwrap();
        store.save_index().unwrap();

        let blobs = std::fs::read_dir(dir.join("blobs")).unwrap().count();
        assert_eq!(blobs, 1, "相同内容只应有一个 blob");
        assert!(dir.join("10.1.2.3_8080/img/a.png").is_file());
        assert!(dir.join("10.1.2.3_8080/img/b.png").is_file());

        let reopened = ResourceStore::open(&dir).unwrap();
        assert_eq!(reopened.index().len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
