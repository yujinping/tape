//! `tape console`：接收盒子 WebView / 网页推送的调试日志（GET / POST），自动落盘
//! `{log-dir}/console-YYYYMMDD-HHMMSSmmm.log`。实现复用通用组件 [`crate::ingest`]。

use anyhow::Result;

use crate::cli::ConsoleArgs;
use crate::ingest::{self, IngestParams};

/// `tape console` 主流程。
pub async fn run(args: ConsoleArgs) -> Result<()> {
    ingest::run(IngestParams {
        port: args.port,
        log_dir: args.log_dir,
        prefix: "console",
        no_color: args.no_color,
    })
    .await
}
