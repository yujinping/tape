//! `tape logcat`：Android logcat 查看子命令（CLI）。
//!
//! 实时读取设备日志，按级别/关键词过滤后彩色输出到终端，并自动落盘
//! `{log-dir}/logcat-YYYYMMDD-HHMMSS.log`（纯文本、无颜色）。
//!
//! adb 读取、logcat 解析与过滤逻辑移植自 [rcat](https://github.com/soenkehahn/rcat)（MIT License）。
use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::cli::LogcatArgs;

/// 单条 logcat 日志（threadtime 格式）。
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub time: String,
    pub pid: String,
    pub tid: String,
    pub level: String, // V/D/I/W/E/F
    pub tag: String,
    pub message: String,
}

/// 解析标准 logcat threadtime 格式单行：
/// `02-03 15:44:41.704  2359  3654 I TagName: Message`
pub fn parse_log_line(line: &str) -> Option<LogEntry> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 6 {
        return None;
    }
    // parts[0] = date, parts[1] = time, parts[2] = pid, parts[3] = tid, parts[4] = level
    let level = parts[4].to_string();
    if !["V", "D", "I", "W", "E", "F"].contains(&level.as_str()) {
        return None;
    }
    let tag_message = parts[5..].join(" ");
    let (tag, message) = match tag_message.split_once(": ") {
        Some((t, m)) => (t.to_string(), m.to_string()),
        None => (tag_message, String::new()),
    };
    Some(LogEntry {
        time: parts[1].to_string(),
        pid: parts[2].to_string(),
        tid: parts[3].to_string(),
        level,
        tag,
        message,
    })
}

/// 级别权重，用于 `--level` 最小级别过滤。
fn level_rank(level: &str) -> u8 {
    match level {
        "V" => 0,
        "D" => 1,
        "I" => 2,
        "W" => 3,
        "E" => 4,
        "F" => 5,
        _ => 0,
    }
}

/// 级别对应的 ANSI 颜色（终端显示用）。
fn level_color(level: &str) -> &'static str {
    match level {
        "V" => "\x1b[90m", // 灰
        "D" => "\x1b[32m", // 绿
        "I" => "\x1b[34m", // 蓝
        "W" => "\x1b[33m", // 黄
        "E" => "\x1b[31m", // 红
        "F" => "\x1b[35m", // 品红
        _ => "\x1b[0m",
    }
}

/// 扫描在线 adb 设备（`adb devices`）。
fn list_devices() -> Vec<String> {
    let Ok(output) = Command::new("adb").args(["devices"]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?;
            let status = parts.next()?;
            (status == "device").then(|| serial.to_string())
        })
        .collect()
}

/// logcat 子进程流读取器：后台线程读取 stdout，经 mpsc 通道投递。
struct LogcatReader {
    process: Option<Child>,
    cancel: Arc<AtomicBool>,
}

impl LogcatReader {
    fn start(serial: &str) -> Result<(Self, mpsc::UnboundedReceiver<String>)> {
        // 先清空设备日志缓冲区，保证只看到"本次会话"的日志
        let _ = Command::new("adb")
            .args(["-s", serial, "logcat", "-c"])
            .output();

        let mut process = Command::new("adb")
            .args(["-s", serial, "logcat", "-v", "threadtime"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("无法启动 adb logcat（请确认 adb 已安装并在 PATH 中）")?;
        let stdout = process
            .stdout
            .take()
            .context("无法获取 adb logcat stdout")?;

        let (tx, rx) = mpsc::unbounded_channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(line) = line
                    && tx.send(line).is_err()
                {
                    break;
                }
            }
        });

        Ok((
            Self {
                process: Some(process),
                cancel,
            },
            rx,
        ))
    }

    fn stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(mut proc) = self.process.take() {
            let _ = proc.kill();
            let _ = proc.wait();
        }
    }
}

impl Drop for LogcatReader {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 落盘文件名时间戳：`YYYYMMDD-HHMMSS`（本地时间，失败时回退 UTC）。
fn log_file_stamp() -> String {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    now.format(&time::macros::format_description!(
        "[year][month][day]-[hour][minute][second]"
    ))
    .unwrap_or_else(|_| "unknown".to_string())
}

/// `tape logcat` 主流程。
pub async fn run(args: LogcatArgs) -> Result<()> {
    let color = !args.no_color && std::io::stdout().is_terminal();

    // 1. 选择设备
    let devices = list_devices();
    let serial = match &args.serial {
        Some(s) => {
            if !devices.iter().any(|d| d == s) {
                anyhow::bail!("未找到在线设备 {}", s);
            }
            s.clone()
        }
        None => devices
            .first()
            .cloned()
            .context("未检测到 adb 在线设备（请连接设备并授权）")?,
    };

    // 2. 启动 logcat
    let (mut reader, mut rx) = LogcatReader::start(&serial)?;

    // 3. 创建落盘文件：{log-dir}/logcat-YYYYMMDD-HHMMSS.log
    std::fs::create_dir_all(&args.log_dir)
        .with_context(|| format!("无法创建日志目录 {}", args.log_dir.display()))?;
    let log_path = args
        .log_dir
        .join(format!("logcat-{}.log", log_file_stamp()));
    let mut file = match std::fs::File::create(&log_path) {
        Ok(f) => Some(f),
        Err(e) => {
            warn!(
                "日志落盘失败 {}: {}（继续打印，不写文件）",
                log_path.display(),
                e
            );
            None
        }
    };

    // 4. 过滤条件
    let min_rank = args.level.map(|l| level_rank(l.as_str())).unwrap_or(0);
    let search = args.search.as_deref().unwrap_or("").to_lowercase();

    info!(
        "tape logcat: 设备 {}，输出 {}，落盘 {}",
        serial,
        log_path.display(),
        if file.is_some() { "开启" } else { "失败" }
    );

    // 5. 读取循环：过滤 → 终端打印（彩色）→ 落盘（纯文本）
    let mut stdout = std::io::stdout().lock();
    loop {
        tokio::select! {
            line = rx.recv() => {
                let Some(line) = line else { break };
                let Some(entry) = parse_log_line(&line) else { continue };
                if level_rank(&entry.level) < min_rank {
                    continue;
                }
                if !search.is_empty()
                    && !format!("{} {} {} {}", entry.tag, entry.message, entry.pid, entry.tid)
                        .to_lowercase()
                        .contains(&search)
                {
                    continue;
                }
                let plain = format!(
                    "{} {:>5} {:>5} {} {}: {}",
                    entry.time, entry.pid, entry.tid, entry.level, entry.tag, entry.message
                );
                if color {
                    let colored = format!(
                        "{} {:>5} {:>5} {}{} {}: {}\x1b[0m",
                        entry.time, entry.pid, entry.tid, level_color(&entry.level), entry.level, entry.tag, entry.message
                    );
                    let _ = writeln!(stdout, "{}", colored);
                } else {
                    let _ = writeln!(stdout, "{}", plain);
                }
                if let Some(f) = file.as_mut()
                    && let Err(e) = writeln!(f, "{}", plain)
                {
                    warn!("写入日志文件失败: {}（停止落盘）", e);
                    file = None;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\n[tape logcat] 已停止，日志已保存到 {}", log_path.display());
                break;
            }
        }
    }

    reader.stop();
    if let Some(f) = file.as_mut() {
        let _ = f.flush();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_standard_threadtime_line() {
        let entry =
            parse_log_line("02-03 15:44:41.704  2359  3654 I TagName: Hello world").unwrap();
        assert_eq!(entry.time, "15:44:41.704");
        assert_eq!(entry.pid, "2359");
        assert_eq!(entry.tid, "3654");
        assert_eq!(entry.level, "I");
        assert_eq!(entry.tag, "TagName");
        assert_eq!(entry.message, "Hello world");
    }

    #[test]
    fn parse_line_without_message() {
        let entry = parse_log_line("02-03 15:44:41.704  2359  3654 W TagOnly").unwrap();
        assert_eq!(entry.tag, "TagOnly");
        assert_eq!(entry.message, "");
    }

    #[test]
    fn parse_rejects_invalid_or_short_lines() {
        assert!(parse_log_line("").is_none());
        assert!(parse_log_line("not enough tokens").is_none());
        assert!(parse_log_line("02-03 15:44:41.704  2359  3654 X Tag: nope").is_none());
    }

    #[test]
    fn level_rank_ordering() {
        assert!(level_rank("V") < level_rank("D"));
        assert!(level_rank("D") < level_rank("I"));
        assert!(level_rank("I") < level_rank("W"));
        assert!(level_rank("W") < level_rank("E"));
        assert!(level_rank("E") < level_rank("F"));
    }

    #[test]
    fn log_file_stamp_shape() {
        let stamp = log_file_stamp();
        assert_eq!(stamp.len(), 15, "expected YYYYMMDD-HHMMSS, got {stamp}");
        assert_eq!(&stamp[8..9], "-");
        assert!(stamp.chars().all(|c| c.is_ascii_digit() || c == '-'));
    }
}
