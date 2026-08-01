use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::snapshot::Snapshot;

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// `http://10.1.2.3:8080` -> `10.1.2.3_8080`，用于磁盘目录名。
pub fn origin_dir_name(origin: &str) -> String {
    let trimmed = origin
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let name = trimmed.replace([':', '/'], "_");
    if name.is_empty() {
        "unknown".to_string()
    } else {
        name
    }
}

/// URL path+query 的短哈希，用于快照文件名。
pub fn path_hash(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    hex::encode(&hasher.finalize()[..6])
}

pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub tool_version: String,
    pub recorded_at: String,
    pub origins: Vec<String>,
    pub snapshot_count: usize,
}

/// 录制器：负责快照序号分配与落盘、session.json 维护。
pub struct Recorder {
    root: PathBuf,
    seq: AtomicU64,
    origins: Mutex<Vec<String>>,
    recorded_at: String,
}

impl Recorder {
    pub fn new(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(root.join("snapshots"))
            .with_context(|| format!("无法创建快照目录: {}", root.display()))?;
        let recorder = Self {
            root,
            seq: AtomicU64::new(0),
            origins: Mutex::new(Vec::new()),
            recorded_at: now_rfc3339(),
        };
        // 启动即落盘 session.json（0 条快照），避免数据目录看起来"未初始化"
        recorder.save_session()?;
        Ok(recorder)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn next_id(&self) -> String {
        format!("{:06}", self.seq.fetch_add(1, Ordering::SeqCst) + 1)
    }

    pub fn write_snapshot(&self, snapshot: &Snapshot) -> Result<PathBuf> {
        let origin_dir = self
            .root
            .join("snapshots")
            .join(origin_dir_name(&snapshot.origin));
        std::fs::create_dir_all(&origin_dir)?;
        let file_name = format!(
            "{}-{}-{}.json",
            snapshot.id,
            sanitize_method(&snapshot.request.method),
            path_hash(&snapshot.request.url)
        );
        let path = origin_dir.join(file_name);
        let json = serde_json::to_string_pretty(snapshot)?;
        std::fs::write(&path, json)?;
        self.origins.lock().unwrap().push(snapshot.origin.clone());
        let _ = self.save_session();
        Ok(path)
    }

    fn save_session(&self) -> Result<()> {
        let origins = {
            let mut v = self.origins.lock().unwrap().clone();
            v.sort();
            v.dedup();
            v
        };
        let snapshot_count = load_snapshots(&self.root)?.len();
        let meta = SessionMeta {
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            recorded_at: self.recorded_at.clone(),
            origins,
            snapshot_count,
        };
        let json = serde_json::to_string_pretty(&meta)?;
        std::fs::write(self.root.join("session.json"), json)?;
        Ok(())
    }
}

fn sanitize_method(method: &str) -> String {
    let cleaned: String = method
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(10)
        .collect();
    if cleaned.is_empty() {
        "REQ".to_string()
    } else {
        cleaned
    }
}

/// 递归读取 `snapshots/<origin>/*.json`，按 id 排序。
pub fn load_snapshots(root: &Path) -> Result<Vec<Snapshot>> {
    let snap_dir = root.join("snapshots");
    let mut out = Vec::new();
    if !snap_dir.is_dir() {
        return Ok(out);
    }
    for origin_entry in std::fs::read_dir(&snap_dir)? {
        let origin_entry = origin_entry?;
        let origin_path = origin_entry.path();
        if !origin_path.is_dir() {
            continue;
        }
        for file_entry in std::fs::read_dir(&origin_path)? {
            let file_entry = file_entry?;
            let path = file_entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                let text = std::fs::read_to_string(&path)?;
                if let Ok(snap) = serde_json::from_str::<Snapshot>(&text) {
                    out.push(snap);
                }
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{ENCODING_BASE64, ENCODING_UTF8, RequestRecord, ResponseRecord};

    fn sample_snapshot(id: &str, body: &str, encoding: &str) -> Snapshot {
        Snapshot {
            id: id.to_string(),
            origin: "http://10.1.2.3:8080".to_string(),
            recorded_at: "2026-08-01T00:00:00Z".to_string(),
            duration_ms: 7,
            request: RequestRecord {
                method: "GET".to_string(),
                url: format!("http://10.1.2.3:8080/api/user?id={id}"),
                headers: vec![("accept".to_string(), "*/*".to_string())],
                body: String::new(),
                body_encoding: ENCODING_UTF8.to_string(),
            },
            response: ResponseRecord {
                status: 200,
                headers: vec![("content-type".to_string(), "application/json".to_string())],
                body: body.to_string(),
                body_encoding: encoding.to_string(),
            },
        }
    }

    #[test]
    fn origin_dir_name_normalizes() {
        assert_eq!(origin_dir_name("http://10.1.2.3:8080"), "10.1.2.3_8080");
        assert_eq!(origin_dir_name("https://a.b.com:8443"), "a.b.com_8443");
        assert_eq!(origin_dir_name("http://10.1.2.3"), "10.1.2.3");
    }

    #[test]
    fn path_hash_is_stable_and_distinct() {
        assert_eq!(path_hash("/api/user?id=1"), path_hash("/api/user?id=1"));
        assert_ne!(path_hash("/api/user?id=1"), path_hash("/api/user?id=2"));
    }

    #[test]
    fn recorder_writes_and_loads_roundtrip() {
        let dir = std::env::temp_dir().join(format!("tape-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let recorder = Recorder::new(dir.clone()).unwrap();

        let mut snap1 = sample_snapshot("", r#"{"name":"中文"}"#, ENCODING_UTF8);
        snap1.id = recorder.next_id();
        let mut snap2 = sample_snapshot("", "binary", ENCODING_BASE64);
        snap2.id = recorder.next_id();
        recorder.write_snapshot(&snap1).unwrap();
        recorder.write_snapshot(&snap2).unwrap();

        assert_eq!(recorder.next_id(), "000003");
        assert!(dir.join("session.json").is_file());

        let loaded = load_snapshots(&dir).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id, "000001");
        assert_eq!(loaded[1].id, "000002");
        assert_eq!(loaded[0].response.body, r#"{"name":"中文"}"#);
        assert_eq!(loaded[1].response.body_encoding, ENCODING_BASE64);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recorder_initializes_empty_session_at_startup() {
        let dir = std::env::temp_dir().join(format!(
            "tape-test-session-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Recorder::new(dir.clone()).unwrap();
        assert!(dir.join("session.json").is_file(), "启动应写入 session.json");
        let text = std::fs::read_to_string(dir.join("session.json")).unwrap();
        assert!(text.contains("\"snapshot_count\": 0"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
