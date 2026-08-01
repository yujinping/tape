use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use crate::config::DEFAULT_PORT;

#[derive(Parser)]
#[command(name = "tape", version, about = "专网HTTP接口录制与离线重放代理工具")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// 录制模式：本地正向 HTTP 代理，录制全部流量到快照目录
    Record(RecordArgs),
    /// 重放模式：本地离线服务，按 method+path 匹配快照并返回改写后的响应
    Replay(ReplayArgs),
    /// 列出数据目录下缓存的站点与接口/资源数量
    List(ListArgs),
}

#[derive(Args)]
pub struct ListArgs {
    /// 快照数据目录（默认当前目录下 tape-api，支持软链接切换已录制目录）
    #[arg(short, long, default_value = "./tape-api")]
    pub dir: PathBuf,
}

#[derive(Args)]
pub struct RecordArgs {
    /// 代理监听端口
    #[arg(short, long, default_value_t = DEFAULT_PORT)]
    pub port: u16,
    /// 快照与资源输出目录（默认当前目录下 tape-api，支持软链接切换已录制目录）
    #[arg(short, long, default_value = "./tape-api")]
    pub dir: PathBuf,
    /// 录制时同步改写回传给 APP 的响应（默认原样回传，保证录制保真）
    #[arg(long)]
    pub rewrite_on_record: bool,
    /// 录制过滤配置文件（TOML，见 README；[record] 表支持 include_hosts 数组
    /// 与 include_hosts_regex 正则数组）；不传则默认读取数据目录下的
    /// tape-config.toml（不存在则录制全部）。未匹配的请求仍会正常转发，只是不落快照
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// 日志详细程度（可重复 -v）
    #[arg(short, long, action = ArgAction::Count)]
    pub verbose: u8,
}

#[derive(Args)]
pub struct ReplayArgs {
    /// 重放服务监听端口（默认 8888，与 record 一致，录制/重放两阶段 APP 地址不变；可在配置文件 [replay] 表中设置，命令行优先）
    #[arg(short, long)]
    pub port: Option<u16>,
    /// 快照数据目录（默认当前目录下 tape-api，支持软链接切换已录制目录）
    #[arg(short, long, default_value = "./tape-api")]
    pub dir: PathBuf,
    /// 响应改写模式（默认 relative；可在配置文件 [replay] 表中设置，命令行优先）
    #[arg(long, value_enum)]
    pub rewrite: Option<RewriteMode>,
    /// absolute 改写模式下使用的本地基地址（默认 http://127.0.0.1:8888/；可在配置文件 [replay] 表中设置，命令行优先）
    #[arg(long)]
    pub absolute_base: Option<String>,
    /// 共用配置文件（TOML，[replay] 表支持 port / rewrite / absolute_base，见 README）；
    /// 不传则默认读取数据目录下的 tape-config.toml
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// 日志详细程度（可重复 -v）
    #[arg(short, long, action = ArgAction::Count)]
    pub verbose: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RewriteMode {
    /// 改写为相对路径（推荐：跨机器/端口可移植）
    Relative,
    /// 改写为绝对地址（配合 --absolute-base）
    Absolute,
    /// 不改写
    None,
}
