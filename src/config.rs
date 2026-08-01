use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;

use crate::cli::RewriteMode;
use crate::rewrite::RewriteRule;

/// 数据目录内的默认配置文件名称。
pub const CONFIG_FILE_NAME: &str = "tape-config.toml";

/// record 与 replay 共用同一默认端口，录制/重放两阶段 APP 地址保持一致。
pub const DEFAULT_PORT: u16 = 8888;

/// absolute 改写模式默认本地基地址（与默认端口一致）。
pub const DEFAULT_ABSOLUTE_BASE: &str = "http://127.0.0.1:8888/";

pub struct RecordConfig {
    pub port: u16,
    pub dir: PathBuf,
    pub rewrite_on_record: bool,
    pub filter: RecordFilter,
    /// 实际使用的配置文件（默认位置不存在时为 None，表示录制全部）。
    pub config_path: Option<PathBuf>,
}

pub struct ReplayConfig {
    pub port: u16,
    pub dir: PathBuf,
    pub rewrite: RewriteRule,
    /// 实际使用的配置文件（默认位置不存在时为 None）。
    pub config_path: Option<PathBuf>,
}

/// 配置文件顶层结构：`[record]` 与 `[replay]` 两表共用同一文件。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    record: Option<RecordFilterSection>,
    #[serde(default)]
    replay: Option<ReplaySection>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordFilterSection {
    #[serde(default)]
    include_hosts: Vec<String>,
    #[serde(default)]
    include_hosts_regex: Vec<String>,
}

/// 重放模式配置段；字段均为可选，缺省时使用内置默认值。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaySection {
    port: Option<u16>,
    rewrite: Option<String>,
    absolute_base: Option<String>,
}

/// 读取并解析 TOML 配置文件；`None` 表示没有配置文件（用内置默认值）。
fn load_config(path: Option<&Path>) -> Result<RawConfig> {
    let Some(path) = path else {
        return Ok(RawConfig::default());
    };
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("无法读取配置文件 {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("配置文件解析失败 {}", path.display()))
}

/// 录制过滤规则：host 数组 + 预编译正则数组，任一命中即录制。
#[derive(Debug, Clone)]
pub struct RecordFilter {
    hosts: Vec<String>,
    regexes: Vec<Regex>,
}

impl RecordFilter {
    /// 录制全部（未配置过滤规则）。
    pub fn all() -> Self {
        Self {
            hosts: Vec::new(),
            regexes: Vec::new(),
        }
    }

    /// 从配置文件的 host 数组与正则数组构建规则（用于测试与配置加载）。
    pub fn with_rules(hosts: Vec<String>, regexes: Vec<String>) -> Result<Self> {
        let hosts = normalize_hosts(hosts);
        let mut compiled = Vec::with_capacity(regexes.len());
        for pattern in regexes {
            compiled.push(compile_regex(&pattern)?);
        }
        Ok(Self {
            hosts,
            regexes: compiled,
        })
    }

    /// 从共用配置文件读取 `[record]` 段；`None` 表示不限制（录制全部）。
    pub fn from_config_path(path: Option<&Path>) -> Result<Self> {
        let raw = load_config(path)?;
        let section = raw.record.unwrap_or_default();
        Self::with_rules(section.include_hosts, section.include_hosts_regex)
    }

    pub fn is_all(&self) -> bool {
        self.hosts.is_empty() && self.regexes.is_empty()
    }

    /// authority 形如 `host:port`（如 `10.1.2.3:8080`）。
    /// 规则语义：host 项匹配该主机任意端口；host:port 项精确匹配；
    /// 正则对完整 authority 做大小写不敏感匹配；两类规则取并集。
    pub fn matches(&self, authority: &str) -> bool {
        if self.is_all() {
            return true;
        }
        let authority = authority.to_ascii_lowercase();
        let host = authority.split(':').next().unwrap_or(&authority);
        if self.hosts.iter().any(|h| authority == *h || host == *h) {
            return true;
        }
        self.regexes.iter().any(|re| re.is_match(&authority))
    }
}

fn compile_regex(pattern: &str) -> Result<Regex> {
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .with_context(|| format!("配置中的正则非法: {pattern}"))
}

pub fn record_config(
    port: u16,
    dir: PathBuf,
    rewrite_on_record: bool,
    config_path: Option<PathBuf>,
) -> Result<RecordConfig> {
    let config_path = resolve_config_path(config_path, &dir);
    let filter = RecordFilter::from_config_path(config_path.as_deref())?;
    // 先校验配置再建目录：显式 --config 缺失时报错，不应留下空的半成品数据目录
    std::fs::create_dir_all(&dir).with_context(|| format!("无法创建数据目录 {}", dir.display()))?;
    Ok(RecordConfig {
        port,
        dir,
        rewrite_on_record,
        filter,
        config_path,
    })
}

/// 解析配置文件路径：
/// - 显式传了 `--config`：直接用该文件（缺失会报错）；
/// - 未传 `--config`：默认取数据目录下的 `tape-config.toml`，
///   该默认文件不存在时返回 `None`（录制全部）。
pub fn resolve_config_path(config_path: Option<PathBuf>, dir: &Path) -> Option<PathBuf> {
    match config_path {
        Some(path) => Some(path),
        None => {
            let default = dir.join(CONFIG_FILE_NAME);
            default.is_file().then_some(default)
        }
    }
}

/// 规范化 host 项：去除 scheme、转小写、去掉空项。
fn normalize_hosts(hosts: Vec<String>) -> Vec<String> {
    hosts
        .into_iter()
        .map(|h| {
            let h = h.trim().to_ascii_lowercase();
            h.trim_start_matches("http://")
                .trim_start_matches("https://")
                .to_string()
        })
        .filter(|h| !h.is_empty())
        .collect()
}

pub fn replay_config(
    port: Option<u16>,
    dir: PathBuf,
    mode: Option<RewriteMode>,
    absolute_base: Option<String>,
    config_path: Option<PathBuf>,
) -> Result<ReplayConfig> {
    if !dir.is_dir() {
        anyhow::bail!(
            "数据目录不存在: {}（请先运行 tape record 录制，或用 --dir 指定已录制目录）",
            dir.display()
        );
    }
    let config_path = resolve_config_path(config_path, &dir);
    let raw = load_config(config_path.as_deref())?;
    let replay = raw.replay.unwrap_or_default();

    // 优先级：命令行显式参数 > 配置文件 > 内置默认值。
    let port = port.or(replay.port).unwrap_or(DEFAULT_PORT);
    let mode = match mode {
        Some(m) => m,
        None => match replay.rewrite.as_deref() {
            Some(value) => parse_rewrite_mode(value)?,
            None => RewriteMode::Relative,
        },
    };
    let absolute_base = absolute_base
        .or(replay.absolute_base)
        .unwrap_or_else(|| DEFAULT_ABSOLUTE_BASE.to_string());

    let rewrite = match mode {
        RewriteMode::Relative => RewriteRule::Relative,
        RewriteMode::Absolute => RewriteRule::Absolute {
            base: absolute_base,
        },
        RewriteMode::None => RewriteRule::None,
    };
    Ok(ReplayConfig {
        port,
        dir,
        rewrite,
        config_path,
    })
}

fn parse_rewrite_mode(value: &str) -> Result<RewriteMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "relative" => Ok(RewriteMode::Relative),
        "absolute" => Ok(RewriteMode::Absolute),
        "none" => Ok(RewriteMode::None),
        other => anyhow::bail!(
            "配置文件 [replay] 的 rewrite 取值非法: {other}（可选 relative / absolute / none）"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_hosts_strips_scheme_and_case() {
        assert_eq!(
            normalize_hosts(vec![
                " HTTP://10.1.2.3:8080 ".to_string(),
                "api.Example.com".to_string(),
                "".to_string(),
            ]),
            vec!["10.1.2.3:8080".to_string(), "api.example.com".to_string()]
        );
    }

    #[test]
    fn filter_matches_host_any_port_and_exact_port() {
        let f = RecordFilter::with_rules(
            vec!["10.1.2.3".to_string(), "api.company.com:8080".to_string()],
            vec![],
        )
        .unwrap();
        assert!(f.matches("10.1.2.3:8080"));
        assert!(f.matches("10.1.2.3:9090"));
        assert!(f.matches("api.company.com:8080"));
        assert!(!f.matches("api.company.com:9090"));
        assert!(!f.matches("10.1.2.4:8080"));
    }

    #[test]
    fn filter_matches_regex_on_authority() {
        let f = RecordFilter::with_rules(
            vec![],
            vec![
                r"^10\.1\.2\.(3|4):\d+$".to_string(),
                r"\.company\.com(:\d+)?$".to_string(),
            ],
        )
        .unwrap();
        assert!(f.matches("10.1.2.3:8080"));
        assert!(f.matches("10.1.2.4:9090"));
        assert!(!f.matches("10.1.2.5:8080"));
        assert!(f.matches("api.company.com:80"));
        assert!(f.matches("sub.Company.COM:443"));
        assert!(!f.matches("company.com.evil.net:80"));
    }

    #[test]
    fn filter_all_matches_everything() {
        let f = RecordFilter::all();
        assert!(f.is_all());
        assert!(f.matches("anything:1"));
    }

    #[test]
    fn empty_config_means_all() {
        let dir = std::env::temp_dir().join(format!("tape-cfg-empty-{}", std::process::id()));
        let path = dir.join("tape.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "").unwrap();
        let f = RecordFilter::from_config_path(Some(&path)).unwrap();
        assert!(f.is_all());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loads_config_file_with_hosts_and_regex() {
        let dir = std::env::temp_dir().join(format!("tape-cfg-{}", std::process::id()));
        let path = dir.join("tape.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            r#"
# 录制过滤配置
[record]
include_hosts = ["10.1.2.3:8080", "api.company.com"]
include_hosts_regex = ['^10\.1\.2\.(3|4):\d+$']
"#,
        )
        .unwrap();
        let f = RecordFilter::from_config_path(Some(&path)).unwrap();
        assert!(f.matches("10.1.2.3:8080"));
        assert!(f.matches("api.company.com:80"));
        assert!(f.matches("10.1.2.4:1234"));
        assert!(!f.matches("10.1.2.9:8080"));
        assert!(!f.matches("other.com:80"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_toml_errors() {
        let dir = std::env::temp_dir().join(format!("tape-cfg-bad-{}", std::process::id()));
        let path = dir.join("tape.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "not = = valid toml [").unwrap();
        assert!(RecordFilter::from_config_path(Some(&path)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_regex_errors() {
        let dir = std::env::temp_dir().join(format!("tape-cfg-re-{}", std::process::id()));
        let path = dir.join("tape.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "[record]\ninclude_hosts_regex = ['(unclosed']\n").unwrap();
        assert!(RecordFilter::from_config_path(Some(&path)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_config_file_errors() {
        let path = Path::new("/nonexistent/tape.toml");
        assert!(RecordFilter::from_config_path(Some(path)).is_err());
    }

    #[test]
    fn resolve_config_path_prefers_explicit() {
        let dir = Path::new("/some/data-dir");
        let explicit = PathBuf::from("/custom/tape.toml");
        assert_eq!(
            resolve_config_path(Some(explicit.clone()), dir),
            Some(explicit)
        );
    }

    #[test]
    fn resolve_config_path_defaults_inside_dir() {
        let dir = std::env::temp_dir().join(format!("tape-cfg-resolve-{}", std::process::id()));
        let path = dir.join(CONFIG_FILE_NAME);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "").unwrap();
        assert_eq!(resolve_config_path(None, &dir), Some(path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_config_path_missing_default_is_none() {
        let dir =
            std::env::temp_dir().join(format!("tape-cfg-resolve-miss-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(resolve_config_path(None, &dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_config_loads_default_file_in_dir() {
        let dir = std::env::temp_dir().join(format!("tape-record-default-{}", std::process::id()));
        let path = dir.join(CONFIG_FILE_NAME);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "[record]\ninclude_hosts = [\"10.1.2.3\"]\n").unwrap();
        let cfg = record_config(8888, dir.clone(), false, None).unwrap();
        assert!(cfg.filter.matches("10.1.2.3:80"));
        assert!(!cfg.filter.matches("other.com:80"));
        assert_eq!(cfg.config_path, Some(path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_config_without_file_records_all() {
        let dir = std::env::temp_dir().join(format!("tape-record-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = record_config(8888, dir.clone(), false, None).unwrap();
        assert!(cfg.filter.is_all());
        assert_eq!(cfg.config_path, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundled_example_config_is_valid() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tape-config.example.toml");
        let f = RecordFilter::from_config_path(Some(&path)).unwrap();
        assert!(!f.is_all());
        assert!(f.matches("10.1.2.3:8080"));
        assert!(f.matches("api.company.com:443"));
        assert!(f.matches("10.1.2.4:1234"));
        assert!(!f.matches("10.1.2.9:8080"));
        assert!(!f.matches("other.com:80"));
    }

    #[test]
    fn replay_config_reads_shared_config_file() {
        let dir = std::env::temp_dir().join(format!("tape-replay-cfg-{}", std::process::id()));
        let path = dir.join(CONFIG_FILE_NAME);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            r#"
[record]
include_hosts = ["10.1.2.3"]

[replay]
port = 9999
rewrite = "absolute"
absolute_base = "http://10.0.0.1:9000/"
"#,
        )
        .unwrap();
        let cfg = replay_config(None, dir.clone(), None, None, None).unwrap();
        assert_eq!(cfg.port, 9999);
        let RewriteRule::Absolute { base } = cfg.rewrite else {
            panic!("expected absolute rewrite");
        };
        assert_eq!(base, "http://10.0.0.1:9000/");
        assert_eq!(cfg.config_path, Some(path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replay_config_cli_overrides_config_file() {
        let dir = std::env::temp_dir().join(format!("tape-replay-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(CONFIG_FILE_NAME),
            "[replay]\nport = 9999\nrewrite = \"absolute\"\n",
        )
        .unwrap();
        let cfg = replay_config(
            Some(1234),
            dir.clone(),
            Some(RewriteMode::Relative),
            Some("http://cli-base/".to_string()),
            None,
        )
        .unwrap();
        assert_eq!(cfg.port, 1234);
        assert!(matches!(cfg.rewrite, RewriteRule::Relative));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replay_config_defaults_without_file() {
        let dir = std::env::temp_dir().join(format!("tape-replay-default-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = replay_config(None, dir.clone(), None, None, None).unwrap();
        assert_eq!(cfg.port, DEFAULT_PORT);
        assert!(matches!(cfg.rewrite, RewriteRule::Relative));
        assert_eq!(cfg.config_path, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replay_config_invalid_rewrite_errors() {
        let dir = std::env::temp_dir().join(format!("tape-replay-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(CONFIG_FILE_NAME), "[replay]\nrewrite = \"foo\"\n").unwrap();
        assert!(replay_config(None, dir.clone(), None, None, None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_config_table_errors() {
        let dir = std::env::temp_dir().join(format!("tape-cfg-unknown-{}", std::process::id()));
        let path = dir.join("tape.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "[foo]\nbar = 1\n").unwrap();
        assert!(RecordFilter::from_config_path(Some(&path)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
