use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::cli::RewriteMode;
use crate::rewrite::RewriteRule;

/// 数据目录内的默认配置文件名称。
pub const CONFIG_FILE_NAME: &str = "tape-config.toml";

/// record 与 replay 共用同一默认端口，录制/重放两阶段 APP 地址保持一致。
pub const DEFAULT_PORT: u16 = 8888;

/// absolute 改写模式默认本地基地址（与默认端口一致）。
pub const DEFAULT_ABSOLUTE_BASE: &str = "http://127.0.0.1:8888/";

/// 默认监听地址：0.0.0.0（全网卡，供局域网设备访问）。
pub const DEFAULT_BIND: IpAddr = IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);

pub struct RecordConfig {
    pub bind: IpAddr,
    pub port: u16,
    pub dir: PathBuf,
    pub rewrite_on_record: bool,
    pub filter: RecordFilter,
    /// 透明模式配置；`None` 表示只接受正向代理与前缀式请求。
    pub transparent: Option<TransparentConfig>,
    /// 实际使用的配置文件（默认位置不存在时为 None，表示录制全部）。
    pub config_path: Option<PathBuf>,
}

pub struct ReplayConfig {
    pub bind: IpAddr,
    pub port: u16,
    pub dir: PathBuf,
    pub rewrite: RewriteRule,
    /// 实际使用的配置文件（默认位置不存在时为 None）。
    pub config_path: Option<PathBuf>,
}

/// 配置文件顶层结构：`[record]` 与 `[replay]` 两表共用同一文件。
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<RecordFilterSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<ReplaySection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transparent: Option<TransparentSection>,
}

/// 透明模式配置段（`[transparent]`），命令行参数优先于本段。
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransparentSection {
    #[serde(default)]
    pub enabled: bool,
    /// 目标端口白名单，元素形如 `"80"` 或 `"8080-8090"`，也接受逗号分隔的单个元素。
    #[serde(default)]
    pub allow_ports: Vec<String>,
    /// 目标主机白名单；空表示不限制主机（端口白名单仍然生效）。
    #[serde(default)]
    pub allow_hosts: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordFilterSection {
    #[serde(default)]
    pub include_hosts: Vec<String>,
    #[serde(default)]
    pub include_hosts_regex: Vec<String>,
}

/// 重放模式配置段；字段均为可选，缺省时使用内置默认值。
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaySection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absolute_base: Option<String>,
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

/// 读取数据目录下的配置文件（不存在时返回全默认值）。
pub fn read_raw_config(path: &Path) -> Result<RawConfig> {
    load_config(Some(path))
}

/// 校验并写回配置文件。
///
/// 先校验透明模式端口与过滤正则，再原子写入，避免管理界面把坏配置落盘。
pub fn save_raw_config(path: &Path, raw: &RawConfig) -> Result<()> {
    if let Some(section) = &raw.transparent
        && section.enabled
    {
        TransparentConfig::parse(
            &section.allow_ports,
            &section.allow_hosts,
            DEFAULT_BIND,
            DEFAULT_PORT,
        )?;
    }
    if let Some(section) = &raw.record {
        RecordFilter::with_rules(
            section.include_hosts.clone(),
            section.include_hosts_regex.clone(),
        )?;
    }
    let text = toml::to_string_pretty(raw).context("配置序列化失败")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("无法创建配置目录 {}", parent.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("无法写入 {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("无法更新 {}", path.display()))?;
    Ok(())
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

/// 透明模式配置：目标端口/主机白名单 + 本机监听信息（用于回环防护）。
///
/// 透明模式意味着「任何能连到 tape 端口的请求都会被转发」，如果不加白名单，
/// 它就变成了一个开放代理/内网跳板。因此端口白名单是必填项，主机白名单可选。
#[derive(Debug, Clone)]
pub struct TransparentConfig {
    allow_ports: Vec<u16>,
    allow_hosts: Vec<String>,
    bind: IpAddr,
    port: u16,
}

impl TransparentConfig {
    /// 解析并校验透明模式配置；端口白名单为空时报错。
    pub fn parse(
        allow_ports: &[String],
        allow_hosts: &[String],
        bind: IpAddr,
        port: u16,
    ) -> Result<Self> {
        let ports = parse_port_specs(allow_ports)?;
        if ports.is_empty() {
            anyhow::bail!(
                "透明模式需要目标端口白名单：用 --transparent-allow-port 80,8080 或配置文件 [transparent].allow_ports 指定"
            );
        }
        let hosts = allow_hosts
            .iter()
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        Ok(Self {
            allow_ports: ports,
            allow_hosts: hosts,
            bind,
            port,
        })
    }

    /// 目标是否在端口/主机白名单内。authority 形如 `10.1.2.3:8080` 或 `api.foo.com`。
    pub fn allows(&self, authority: &str) -> bool {
        let Ok(authority) = authority.parse::<hyper::http::uri::Authority>() else {
            return false;
        };
        let port = authority.port_u16().unwrap_or(80);
        if !self.allow_ports.contains(&port) {
            return false;
        }
        if self.allow_hosts.is_empty() {
            return true;
        }
        let host = authority.host().to_ascii_lowercase();
        self.allow_hosts
            .iter()
            .any(|pattern| host_matches(pattern, &host))
    }

    /// 目标是否指向 tape 自身（转发会形成回环）。
    ///
    /// 只对「本机名字 + 监听端口」判回环：局域网里另一台机器恰好也用 8888 是合法的。
    pub fn is_self_target(&self, authority: &str) -> bool {
        let Ok(authority) = authority.parse::<hyper::http::uri::Authority>() else {
            return false;
        };
        if authority.port_u16().unwrap_or(80) != self.port {
            return false;
        }
        let host = authority.host().to_ascii_lowercase();
        let bind = self.bind.to_string();
        host == "localhost"
            || host == "127.0.0.1"
            || host == "::1"
            || host == "0.0.0.0"
            || host == bind
    }

    pub fn allowed_ports(&self) -> &[u16] {
        &self.allow_ports
    }
}

/// 主机白名单匹配：`*.foo.com` 只匹配子域，其余按完整主机名精确匹配。
fn host_matches(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix("*.") {
        Some(suffix) => host.len() > suffix.len() && host.ends_with(&format!(".{suffix}")),
        None => host == pattern,
    }
}

/// 解析端口列表：元素可以是 `80`、`8080-8090`，也可以是逗号分隔的 `80,8080`。
fn parse_port_specs(specs: &[String]) -> Result<Vec<u16>> {
    let mut ports = std::collections::BTreeSet::new();
    for spec in specs {
        for item in spec.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let (start, end) = match item.split_once('-') {
                Some((start, end)) => (start.trim(), end.trim()),
                None => (item, item),
            };
            let start = start
                .parse::<u16>()
                .with_context(|| format!("透明模式端口非法: {item}"))?;
            let end = end
                .parse::<u16>()
                .with_context(|| format!("透明模式端口非法: {item}"))?;
            if start == 0 || start > end {
                anyhow::bail!("透明模式端口区间非法: {item}");
            }
            if u32::from(end) - u32::from(start) > 4096 {
                anyhow::bail!("透明模式端口区间过大（最多 4097 个端口）: {item}");
            }
            ports.extend(start..=end);
        }
    }
    Ok(ports.into_iter().collect())
}

fn compile_regex(pattern: &str) -> Result<Regex> {
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .with_context(|| format!("配置中的正则非法: {pattern}"))
}

// 录制参数同时受命令行与配置文件影响，逐项传入比引入中间结构更直白。
#[allow(clippy::too_many_arguments)]
pub fn record_config(
    bind: IpAddr,
    port: u16,
    dir: PathBuf,
    rewrite_on_record: bool,
    config_path: Option<PathBuf>,
    transparent: bool,
    transparent_allow_ports: Vec<String>,
    transparent_allow_hosts: Vec<String>,
) -> Result<RecordConfig> {
    let config_path = resolve_config_path(config_path, &dir);
    let raw = load_config(config_path.as_deref())?;
    let filter = RecordFilter::from_config_path(config_path.as_deref())?;
    // 透明模式：命令行开关/白名单优先，缺省取配置文件 [transparent] 段
    let section = raw.transparent.unwrap_or_default();
    let enabled = transparent || section.enabled;
    let ports = if transparent_allow_ports.is_empty() {
        section.allow_ports
    } else {
        transparent_allow_ports
    };
    let hosts = if transparent_allow_hosts.is_empty() {
        section.allow_hosts
    } else {
        transparent_allow_hosts
    };
    let transparent = if enabled {
        Some(TransparentConfig::parse(&ports, &hosts, bind, port)?)
    } else {
        None
    };
    // 先校验配置再建目录：显式 --config 缺失时报错，不应留下空的半成品数据目录
    std::fs::create_dir_all(&dir).with_context(|| format!("无法创建数据目录 {}", dir.display()))?;
    Ok(RecordConfig {
        bind,
        port,
        dir,
        rewrite_on_record,
        filter,
        transparent,
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
    bind: Option<IpAddr>,
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
    let bind = match bind {
        Some(b) => b,
        None => match replay.bind.as_deref() {
            Some(value) => value
                .trim()
                .parse::<IpAddr>()
                .with_context(|| format!("配置文件 [replay] 的 bind 取值非法: {value}"))?,
            None => DEFAULT_BIND,
        },
    };
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
        bind,
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
        let cfg = record_config(
            DEFAULT_BIND,
            8888,
            dir.clone(),
            false,
            None,
            false,
            vec![],
            vec![],
        )
        .unwrap();
        assert!(cfg.filter.matches("10.1.2.3:80"));
        assert!(!cfg.filter.matches("other.com:80"));
        assert_eq!(cfg.bind, DEFAULT_BIND);
        assert_eq!(cfg.config_path, Some(path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_config_without_file_records_all() {
        let dir = std::env::temp_dir().join(format!("tape-record-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = record_config(
            DEFAULT_BIND,
            8888,
            dir.clone(),
            false,
            None,
            false,
            vec![],
            vec![],
        )
        .unwrap();
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
bind = "127.0.0.1"
port = 9999
rewrite = "absolute"
absolute_base = "http://10.0.0.1:9000/"
"#,
        )
        .unwrap();
        let cfg = replay_config(None, None, dir.clone(), None, None, None).unwrap();
        assert_eq!(cfg.bind, "127.0.0.1".parse::<std::net::IpAddr>().unwrap());
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
            Some("10.0.0.2".parse().unwrap()),
            Some(1234),
            dir.clone(),
            Some(RewriteMode::Relative),
            Some("http://cli-base/".to_string()),
            None,
        )
        .unwrap();
        assert_eq!(cfg.bind, "10.0.0.2".parse::<std::net::IpAddr>().unwrap());
        assert_eq!(cfg.port, 1234);
        assert!(matches!(cfg.rewrite, RewriteRule::Relative));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replay_config_defaults_without_file() {
        let dir = std::env::temp_dir().join(format!("tape-replay-default-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = replay_config(None, None, dir.clone(), None, None, None).unwrap();
        assert_eq!(cfg.bind, DEFAULT_BIND);
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
        assert!(replay_config(None, None, dir.clone(), None, None, None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replay_config_invalid_bind_errors() {
        let dir = std::env::temp_dir().join(format!("tape-replay-bind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(CONFIG_FILE_NAME),
            "[replay]\nbind = \"not-an-ip\"\n",
        )
        .unwrap();
        assert!(replay_config(None, None, dir.clone(), None, None, None).is_err());
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

    fn transparent() -> TransparentConfig {
        TransparentConfig::parse(
            &["80,8080-8081".to_string()],
            &["*.foo.com".to_string(), "10.1.2.3".to_string()],
            "0.0.0.0".parse().unwrap(),
            8888,
        )
        .unwrap()
    }

    #[test]
    fn transparent_allows_ports_and_hosts() {
        let cfg = transparent();
        assert_eq!(cfg.allowed_ports(), &[80, 8080, 8081]);
        assert!(cfg.allows("10.1.2.3:80"));
        assert!(cfg.allows("10.1.2.3:8081"));
        // 端口不在白名单
        assert!(!cfg.allows("10.1.2.3:8082"));
        // 主机不在白名单
        assert!(!cfg.allows("10.9.9.9:80"));
        // 通配子域匹配，但不匹配裸域
        assert!(cfg.allows("api.foo.com:8080"));
        assert!(!cfg.allows("foo.com:8080"));
        // 默认端口按 80 计算
        assert!(cfg.allows("10.1.2.3"));
    }

    #[test]
    fn transparent_empty_ports_is_error() {
        let err = TransparentConfig::parse(&[], &[], "0.0.0.0".parse().unwrap(), 8888).unwrap_err();
        assert!(err.to_string().contains("端口白名单"));
    }

    #[test]
    fn transparent_rejects_invalid_port_spec() {
        assert!(
            TransparentConfig::parse(
                &["8080-".to_string()],
                &[],
                "0.0.0.0".parse().unwrap(),
                8888
            )
            .is_err()
        );
        assert!(
            TransparentConfig::parse(
                &["9000-8000".to_string()],
                &[],
                "0.0.0.0".parse().unwrap(),
                8888
            )
            .is_err()
        );
    }

    #[test]
    fn transparent_detects_self_target_only_on_listen_port() {
        let cfg = transparent();
        assert!(cfg.is_self_target("127.0.0.1:8888"));
        assert!(cfg.is_self_target("localhost:8888"));
        // 同一台机器的其它端口不会回环到 tape
        assert!(!cfg.is_self_target("127.0.0.1:8080"));
        // 局域网另一台机器用 8888 是正常场景
        assert!(!cfg.is_self_target("10.9.9.9:8888"));
    }

    #[test]
    fn transparent_section_in_config_file_enables_mode() {
        let dir = std::env::temp_dir().join(format!("tape-cfg-transparent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(CONFIG_FILE_NAME),
            "[transparent]\nenabled = true\nallow_ports = [\"80\"]\nallow_hosts = [\"api.foo.com\"]\n",
        )
        .unwrap();
        let cfg = record_config(
            "127.0.0.1".parse().unwrap(),
            8888,
            dir.clone(),
            false,
            None,
            false,
            vec![],
            vec![],
        )
        .unwrap();
        let transparent = cfg.transparent.unwrap();
        assert!(transparent.allows("api.foo.com:80"));
        assert!(!transparent.allows("api.foo.com:8080"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod roundtrip_tests {
    use super::*;

    fn temp_config_path() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tape-config-roundtrip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        dir.join("tape-config.toml")
    }

    #[test]
    fn save_and_read_config_roundtrip() {
        let path = temp_config_path();
        let raw = RawConfig {
            record: Some(RecordFilterSection {
                include_hosts: vec!["10.1.2.3:8080".to_string()],
                include_hosts_regex: vec![r"^10\.1\.2\.(3|4):\d+$".to_string()],
            }),
            replay: Some(ReplaySection {
                bind: None,
                port: Some(8899),
                rewrite: Some("relative".to_string()),
                absolute_base: None,
            }),
            transparent: Some(TransparentSection {
                enabled: true,
                allow_ports: vec!["80".to_string(), "8080-8090".to_string()],
                allow_hosts: vec!["*.company.com".to_string()],
            }),
        };
        save_raw_config(&path, &raw).expect("保存配置");
        let loaded = read_raw_config(&path).expect("读取配置");
        let record = loaded.record.expect("record 段");
        assert_eq!(record.include_hosts, vec!["10.1.2.3:8080"]);
        let transparent = loaded.transparent.expect("transparent 段");
        assert!(transparent.enabled);
        assert_eq!(transparent.allow_ports, vec!["80", "8080-8090"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn invalid_transparent_ports_are_rejected() {
        let path = temp_config_path();
        let raw = RawConfig {
            record: None,
            replay: None,
            transparent: Some(TransparentSection {
                enabled: true,
                allow_ports: vec!["70000".to_string()],
                allow_hosts: vec![],
            }),
        };
        assert!(save_raw_config(&path, &raw).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
