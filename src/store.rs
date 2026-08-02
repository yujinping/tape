use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::snapshot::Snapshot;

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// `http://10.1.2.3:8080` -> `http_10.1.2.3_8080`，用于磁盘目录名。
/// 前缀保留 scheme，避免同一 host:port 的 http / https 快照混入同一目录。
pub fn origin_dir_name(origin: &str) -> String {
    let (scheme, rest) = origin.split_once("://").unwrap_or(("", origin));
    let name = rest.replace([':', '/'], "_");
    if name.is_empty() {
        return "unknown".to_string();
    }
    let scheme = scheme.to_ascii_lowercase();
    if scheme == "http" || scheme == "https" {
        format!("{scheme}_{name}")
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
    /// 快照总数（启动时统计一次，之后增量维护，避免每次落盘全量重读的 O(n²)）。
    snapshot_count: AtomicUsize,
    origins: Mutex<Vec<String>>,
    recorded_at: String,
}

impl Recorder {
    pub fn new(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(root.join("snapshots"))
            .with_context(|| format!("无法创建快照目录: {}", root.display()))?;
        // 启动时统计一次已有快照：序号从最大 id 续起（避免重复录制同一目录时
        // 000001 重新开始而静默覆盖旧快照），snapshot_count 作为增量基数。
        let existing = load_snapshots(&root)?;
        let max_id = existing
            .iter()
            .filter_map(|s| s.id.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        let snapshot_count = existing.len();
        let recorder = Self {
            root,
            seq: AtomicU64::new(max_id),
            snapshot_count: AtomicUsize::new(snapshot_count),
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
        self.snapshot_count.fetch_add(1, Ordering::SeqCst);
        if let Err(e) = self.save_session() {
            // 快照本身已落盘成功，session.json 维护失败只告警，不中断录制
            warn!("session.json 更新失败: {e}");
        }
        Ok(path)
    }

    fn save_session(&self) -> Result<()> {
        let origins = {
            let mut v = self.origins.lock().unwrap().clone();
            v.sort();
            v.dedup();
            v
        };
        let snapshot_count = self.snapshot_count.load(Ordering::SeqCst);
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
    let mut files = Vec::new();
    if snap_dir.is_dir() {
        collect_json_files(&snap_dir, &mut files)?;
    }
    let mut out = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path)?;
        match serde_json::from_str::<Snapshot>(&text) {
            Ok(snap) => out.push(snap),
            Err(e) => warn!("快照解析失败（已跳过）: {}: {e}", path.display()),
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// 递归收集目录树下的全部 `.json` 文件（兼容历史无 scheme 前缀的旧目录名）。
fn collect_json_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
    Ok(())
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
        assert_eq!(
            origin_dir_name("http://10.1.2.3:8080"),
            "http_10.1.2.3_8080"
        );
        assert_eq!(
            origin_dir_name("https://a.b.com:8443"),
            "https_a.b.com_8443"
        );
        assert_eq!(origin_dir_name("http://10.1.2.3"), "http_10.1.2.3");
        // 同 host:port 的 http / https 目录必须区分
        assert_ne!(
            origin_dir_name("http://a.b.com:8080"),
            origin_dir_name("https://a.b.com:8080")
        );
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
        assert!(
            dir.join("session.json").is_file(),
            "启动应写入 session.json"
        );
        let text = std::fs::read_to_string(dir.join("session.json")).unwrap();
        assert!(text.contains("\"snapshot_count\": 0"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_snapshots_reads_legacy_dir_names() {
        // 历史版本目录名无 scheme 前缀（如 10.1.2.3_8080），递归加载应仍能读取
        let dir = std::env::temp_dir().join(format!(
            "tape-test-legacy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let legacy = dir.join("snapshots").join("10.1.2.3_8080");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(
            legacy.join("000001-GET-abc.json"),
            r#"{"id":"000001","origin":"http://10.1.2.3:8080","recorded_at":"2026-08-01T00:00:00Z","duration_ms":1,"request":{"method":"GET","url":"http://10.1.2.3:8080/api","headers":[],"body":"","body_encoding":"utf8"},"response":{"status":200,"headers":[],"body":"ok","body_encoding":"utf8"}}"#,
        )
        .unwrap();
        let loaded = load_snapshots(&dir).unwrap();
        assert_eq!(loaded.len(), 1, "旧目录名的快照应能递归读取");
        assert_eq!(loaded[0].origin, "http://10.1.2.3:8080");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recorder_resumes_seq_and_count_after_existing_snapshots() {
        let dir = std::env::temp_dir().join(format!(
            "tape-test-resume-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        // 第一会话：写两条快照（000001、000002）
        let recorder = Recorder::new(dir.clone()).unwrap();
        let mut snap1 = sample_snapshot("", "a", ENCODING_UTF8);
        snap1.id = recorder.next_id();
        let mut snap2 = sample_snapshot("", "b", ENCODING_UTF8);
        snap2.id = recorder.next_id();
        recorder.write_snapshot(&snap1).unwrap();
        recorder.write_snapshot(&snap2).unwrap();

        // 第二会话（同一目录）：序号应从最大 id 续起，不得回到 000001 覆盖旧文件
        let recorder2 = Recorder::new(dir.clone()).unwrap();
        let id3 = recorder2.next_id();
        assert_eq!(id3, "000003", "重复录制应从已有最大 id 续起");
        let mut snap3 = sample_snapshot("", "c", ENCODING_UTF8);
        snap3.id = id3;
        recorder2.write_snapshot(&snap3).unwrap();

        let loaded = load_snapshots(&dir).unwrap();
        assert_eq!(loaded.len(), 3, "新会话快照应与旧会话共存");
        assert_eq!(loaded[0].id, "000001");
        assert_eq!(loaded[2].id, "000003");
        let text = std::fs::read_to_string(dir.join("session.json")).unwrap();
        assert!(
            text.contains("\"snapshot_count\": 3"),
            "session.json 应记录增量后的快照数: {text}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
