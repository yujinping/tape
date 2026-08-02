//! `tape export`：把录制目录的快照统一导出为 JSONL 流（每行一条完整快照），
//! 供 AI 矩阵生成、外部对比工具等消费，避免外部工具解析 tape 内部目录结构。
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use tracing::info;

use crate::store;

/// 导出主流程：`-o` 指定输出文件，缺省 stdout。
pub fn run(dir: &Path, output: Option<&Path>) -> Result<()> {
    let text = export_to_string(dir)?;
    match output {
        Some(path) => std::fs::write(path, text)
            .with_context(|| format!("无法写入导出文件 {}", path.display()))?,
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all(text.as_bytes())
                .context("写入 stdout 失败")?;
        }
    }
    Ok(())
}

/// 读取快照目录并序列化为 JSONL 字符串（每行一条快照，按 id 排序）。
pub fn export_to_string(dir: &Path) -> Result<String> {
    let snaps = store::load_snapshots(dir)?;
    let mut out = String::new();
    for snap in &snaps {
        let line = serde_json::to_string(snap)?;
        out.push_str(&line);
        out.push('\n');
    }
    info!("已导出 {} 条快照（{}）", snaps.len(), dir.display());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{ENCODING_UTF8, RequestRecord, ResponseRecord, Snapshot};

    fn sample(id: &str, url: &str) -> Snapshot {
        Snapshot {
            id: id.to_string(),
            origin: "http://10.1.2.3:8080".to_string(),
            recorded_at: "2026-08-02T00:00:00Z".to_string(),
            duration_ms: 7,
            request: RequestRecord {
                method: "GET".to_string(),
                url: url.to_string(),
                headers: vec![],
                body: String::new(),
                body_encoding: ENCODING_UTF8.to_string(),
            },
            response: ResponseRecord {
                status: 200,
                headers: vec![],
                body: "{\"ok\":true}".to_string(),
                body_encoding: ENCODING_UTF8.to_string(),
            },
        }
    }

    #[test]
    fn export_writes_jsonl_with_all_snapshots() {
        let dir = std::env::temp_dir().join(format!(
            "tape-export-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let snap_dir = dir.join("snapshots").join("http_10.1.2.3_8080");
        std::fs::create_dir_all(&snap_dir).unwrap();
        let mut a = sample("000001", "http://10.1.2.3:8080/api/a?x=1");
        let mut b = sample("000002", "http://10.1.2.3:8080/api/b");
        a.request.body = "req-a".to_string();
        b.response.body = "{\"ok\":false}".to_string();
        std::fs::write(
            snap_dir.join("000001-GET-abc.json"),
            serde_json::to_string_pretty(&a).unwrap(),
        )
        .unwrap();
        std::fs::write(
            snap_dir.join("000002-GET-def.json"),
            serde_json::to_string_pretty(&b).unwrap(),
        )
        .unwrap();

        let out = export_to_string(&dir).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "JSONL 每快照一行");
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["request"]["url"], "http://10.1.2.3:8080/api/a?x=1");
        assert_eq!(first["request"]["body"], "req-a");
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["response"]["body"], "{\"ok\":false}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
