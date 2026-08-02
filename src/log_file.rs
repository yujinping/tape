//! 日志落盘公共辅助：带前缀的时间戳文件名 `{prefix}-YYYYMMDD-HHMMSS.log`。
//!
//! 命名规范（各日志源共用同一 `--log-dir`，按前缀区分）：
//! - `logcat-`：Android 设备日志（`tape logcat`）；
//! - `console-`：WebView / 网页 console.log 推送（规划中）；
//! - `app-`：无法使用 logcat 的盒子应用网络日志推送（规划中）。

use std::path::{Path, PathBuf};

/// 本地时间戳 `YYYYMMDD-HHMMSS`（获取本地时间失败时回退 UTC）。
pub fn stamp() -> String {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    now.format(&time::macros::format_description!(
        "[year][month][day]-[hour][minute][second]"
    ))
    .unwrap_or_else(|_| "unknown".to_string())
}

/// 构造落盘路径：`{dir}/{prefix}-YYYYMMDD-HHMMSS.log`。
pub fn path(dir: &Path, prefix: &str) -> PathBuf {
    dir.join(format!("{prefix}-{}.log", stamp()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_shape() {
        let stamp = stamp();
        assert_eq!(stamp.len(), 15, "expected YYYYMMDD-HHMMSS, got {stamp}");
        assert_eq!(&stamp[8..9], "-");
        assert!(stamp.chars().all(|c| c.is_ascii_digit() || c == '-'));
    }

    #[test]
    fn path_uses_prefix_and_stamp() {
        let p = path(Path::new("logs"), "console");
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("console-"), "got {name}");
        assert!(name.ends_with(".log"), "got {name}");
        assert_eq!(name.len(), "console-".len() + 15 + ".log".len());
    }
}
