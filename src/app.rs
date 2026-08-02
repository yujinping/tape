//! `tape app`：接收无法使用 logcat 的盒子应用推送的网络日志（GET / POST），自动落盘
//! `{log-dir}/app-YYYYMMDD-HHMMSS.log`。实现复用通用组件 [`crate::ingest`]。

use anyhow::Result;

use crate::cli::AppArgs;
use crate::ingest::{self, IngestParams};

/// `tape app` 主流程。
pub async fn run(args: AppArgs) -> Result<()> {
    ingest::run(IngestParams {
        port: args.port,
        log_dir: args.log_dir,
        prefix: "app",
        no_color: args.no_color,
    })
    .await
}
