//! `tape compare`：对比两个录制目录，三层对齐 + JSON diff + Markdown 报告。
//! 完整实现由 M1 计划任务 3-7 填充。
use std::path::Path;

use anyhow::Result;

pub fn run(
    _baseline: &Path,
    _current: &Path,
    _matrix: Option<&Path>,
    _ignore: Option<&Path>,
    _output: Option<&Path>,
) -> Result<()> {
    unimplemented!("tape compare 由 M1 任务 3-7 实现")
}
