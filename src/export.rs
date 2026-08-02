//! `tape export`：把录制目录的快照统一导出为 JSONL 流（AI 矩阵生成等外部工具消费）。
//! 完整实现由 M1 计划任务 2 填充。
use std::path::Path;

use anyhow::Result;

pub fn run(_dir: &Path, _output: Option<&Path>) -> Result<()> {
    unimplemented!("tape export 由 M1 任务 2 实现")
}
