use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;

use crate::cli::RewriteMode;
use crate::rewrite::RewriteRule;

pub struct RecordConfig {
    pub port: u16,
    pub dir: PathBuf,
    pub rewrite_on_record: bool,
    pub filter: RecordFilter,
}

pub struct ReplayConfig {
    pub port: u16,
    pub dir: PathBuf,
    pub rewrite: RewriteRule,
}

/// 配置文件顶层结构，`[record]` 表可平级扩展 replay 等其他设置。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    record: Option<RecordFilterSection>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordFilterSection {
    #[serde(default)]
    include_hosts: Vec<String>,
    #[serde(default)]
    include_hosts_regex: Vec<String>,
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

    /// 读取并解析 TOML 配置文件；`None` 表示不限制（录制全部）。
    pub fn from_config_path(path: Option<&Path>) -> Result<Self> {
        let Some(path) = path else {
            return Ok(Self::all());
        };
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("无法读取配置文件 {}", path.display()))?;
        let raw: RawConfig = toml::from_str(&text)
            .with_context(|| format!("配置文件解析失败 {}", path.display()))?;
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
    std::fs::create_dir_all(&dir).with_context(|| format!("无法创建数据目录 {}", dir.display()))?;
    let filter = RecordFilter::from_config_path(config_path.as_deref())?;
    Ok(RecordConfig {
        port,
        dir,
        rewrite_on_record,
        filter,
    })
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
    port: u16,
    dir: PathBuf,
    mode: RewriteMode,
    absolute_base: String,
) -> Result<ReplayConfig> {
    if !dir.is_dir() {
        anyhow::bail!(
            "数据目录不存在: {}（请先运行 box-proxy record 录制，或用 --dir 指定已录制目录）",
            dir.display()
        );
    }
    let rewrite = match mode {
        RewriteMode::Relative => RewriteRule::Relative,
        RewriteMode::Absolute => RewriteRule::Absolute {
            base: absolute_base,
        },
        RewriteMode::None => RewriteRule::None,
    };
    Ok(ReplayConfig { port, dir, rewrite })
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
        let dir = std::env::temp_dir().join(format!("box-proxy-cfg-empty-{}", std::process::id()));
        let path = dir.join("box-proxy.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "").unwrap();
        let f = RecordFilter::from_config_path(Some(&path)).unwrap();
        assert!(f.is_all());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loads_config_file_with_hosts_and_regex() {
        let dir = std::env::temp_dir().join(format!("box-proxy-cfg-{}", std::process::id()));
        let path = dir.join("box-proxy.toml");
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
        let dir = std::env::temp_dir().join(format!("box-proxy-cfg-bad-{}", std::process::id()));
        let path = dir.join("box-proxy.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "not = = valid toml [").unwrap();
        assert!(RecordFilter::from_config_path(Some(&path)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_regex_errors() {
        let dir = std::env::temp_dir().join(format!("box-proxy-cfg-re-{}", std::process::id()));
        let path = dir.join("box-proxy.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "[record]\ninclude_hosts_regex = ['(unclosed']\n").unwrap();
        assert!(RecordFilter::from_config_path(Some(&path)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_config_file_errors() {
        let path = Path::new("/nonexistent/box-proxy.toml");
        assert!(RecordFilter::from_config_path(Some(path)).is_err());
    }
}
