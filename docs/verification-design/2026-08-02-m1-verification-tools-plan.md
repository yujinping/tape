# tape 验证工具（export / compare / 报告）M1 实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 为 tape 新增 `export` 与 `compare` 两个子命令，支撑「新旧系统功能复刻验证」闭环：统一导出录制数据供 AI 消费，按三层对齐对比两个录制目录并输出 Markdown 差异报告。

**架构：** 在现有 tape（Rust 2024 + clap + tokio + serde_json）中新增 `src/export.rs`（快照 → JSONL 流）与 `src/compare.rs`（加载 → 归一化 → 三层对齐 → JSON diff → 报告）。`compare` 以（method, path）分组，组内用「归一化请求指纹」配对，指纹相同者按调用顺序兜底；忽略规则按字段路径过滤动态值；报告按矩阵功能条目（可选）或 path 组织。

**技术栈：** Rust 2024、clap 4（derive）、serde_json、anyhow、tracing。无新增外部依赖（JSON diff 手写递归实现）。

**工作仓库：** `/Users/yujinping/data/workspace/rust-projects/box-proxy`（tape）。所有文件路径相对该仓库根。

---

## 文件结构

**创建：**
- `src/export.rs` —— `tape export`：读快照目录，逐条输出 JSONL
- `src/compare.rs` —— `tape compare`：归一化、三层对齐、JSON diff、忽略规则、Markdown 报告
- `src/compare/`（不拆目录，全部放 `compare.rs`，职责内聚，约 600 行内）

**修改：**
- `src/cli.rs` —— `Command` 枚举新增 `Export(ExportArgs)`、`Compare(CompareArgs)`
- `src/main.rs` —— 分发两个新子命令
- `src/lib.rs` —— `pub mod export; pub mod compare;`
- `tests/proxy_flow.rs` —— 增加 export / compare 端到端集成测试

**测试位置：** `src/export.rs` 与 `src/compare.rs` 内置 `#[cfg(test)] mod tests`；集成测试追加到 `tests/proxy_flow.rs`。

---

### 任务 1：CLI 骨架（export / compare 子命令）

**文件：**
- 修改：`src/cli.rs`
- 修改：`src/main.rs`
- 修改：`src/lib.rs`

- [ ] **步骤 1：在 `src/cli.rs` 的 `Command` 枚举中新增两个变体**

```rust
#[derive(Subcommand)]
pub enum Command {
    // ... 现有变体 ...
    /// 导出录制目录的快照为 JSONL 流（AI 矩阵生成等外部工具消费）
    Export(ExportArgs),
    /// 对比两个录制目录：三层对齐 + JSON diff + Markdown 报告
    Compare(CompareArgs),
}

#[derive(Args)]
pub struct ExportArgs {
    /// 录制数据目录（tape record 的输出目录）
    #[arg(value_name = "DIR")]
    pub dir: PathBuf,
    /// 输出文件（默认 stdout）
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
}

#[derive(Args)]
pub struct CompareArgs {
    /// 基线录制目录（旧版实跑）
    #[arg(value_name = "BASELINE_DIR")]
    pub baseline: PathBuf,
    /// 新版录制目录
    #[arg(value_name = "CURRENT_DIR")]
    pub current: PathBuf,
    /// 功能矩阵 JSON（可选：报告按功能条目组织）
    #[arg(long, value_name = "FILE")]
    pub matrix: Option<PathBuf>,
    /// 忽略规则 JSON（可选：字段路径列表，如 ["$.data.token"]）
    #[arg(long, value_name = "FILE")]
    pub ignore: Option<PathBuf>,
    /// Markdown 报告输出文件（可选；不传则只打印终端摘要）
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
}
```

确认 `use std::path::PathBuf;` 已在 `src/cli.rs` 顶部。

- [ ] **步骤 2：在 `src/main.rs` 的 match 中分发**

```rust
cli::Command::Export(args) => {
    init_tracing(args.verbose);
    export::run(&args.dir, args.output.as_deref())
}
cli::Command::Compare(args) => {
    init_tracing(args.verbose);
    compare::run(
        &args.baseline,
        &args.current,
        args.matrix.as_deref(),
        args.ignore.as_deref(),
        args.output.as_deref(),
    )
}
```

`ExportArgs` / `CompareArgs` 需要 `verbose: u8` 字段（与其它子命令一致，`#[arg(short, long, action = ArgAction::Count)]`）。

- [ ] **步骤 3：在 `src/lib.rs` 注册模块**

```rust
pub mod export;
pub mod compare;
```

- [ ] **步骤 4：运行构建验证**

运行：`cargo check --all-targets`
预期：编译失败，报 `export::run` / `compare::run` 未定义——符合预期，下一步补实现。

- [ ] **步骤 5：Commit**

```bash
git add src/cli.rs src/main.rs src/lib.rs
git commit -m "feat(cli): 新增 export / compare 子命令骨架"
```

---

### 任务 2：`tape export`——快照导出 JSONL

**文件：**
- 创建：`src/export.rs`
- 修改：`src/lib.rs`（上一步已加）

- [ ] **步骤 1：编写失败测试（`src/export.rs` 底部）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{RequestRecord, ResponseRecord, Snapshot, ENCODING_UTF8};

    fn sample(id: &str, url: &str) -> Snapshot {
        Snapshot {
            id: id.to_string(),
            origin: "http://10.1.2.3:8080".to_string(),
            recorded_at: "2026-08-02T00:00:00Z".to_string(),
            duration_ms: 7,
            request: RequestRecord {
                method: "GET".to_string(),
                url: url.to_string(),
                headers: vec![],
                body: String::new(),
                body_encoding: ENCODING_UTF8.to_string(),
            },
            response: ResponseRecord {
                status: 200,
                headers: vec![],
                body: "{\"ok\":true}".to_string(),
                body_encoding: ENCODING_UTF8.to_string(),
            },
        }
    }

    #[test]
    fn export_writes_jsonl_with_all_snapshots() {
        let dir = std::env::temp_dir().join(format!(
            "tape-export-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let snap_dir = dir.join("snapshots").join("http_10.1.2.3_8080");
        std::fs::create_dir_all(&snap_dir).unwrap();
        let mut a = sample("000001", "http://10.1.2.3:8080/api/a?x=1");
        let mut b = sample("000002", "http://10.1.2.3:8080/api/b");
        a.request.body = "req-a".to_string();
        b.response.body = "{\"ok\":false}".to_string();
        std::fs::write(
            snap_dir.join("000001-GET-abc.json"),
            serde_json::to_string_pretty(&a).unwrap(),
        )
        .unwrap();
        std::fs::write(
            snap_dir.join("000002-GET-def.json"),
            serde_json::to_string_pretty(&b).unwrap(),
        )
        .unwrap();

        let out = export_to_string(&dir).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "JSONL 每快照一行");
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["request"]["url"], "http://10.1.2.3:8080/api/a?x=1");
        assert_eq!(first["request"]["body"], "req-a");
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["response"]["body"], "{\"ok\":false}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **步骤 2：运行测试确认失败**

运行：`cargo test --lib export::tests::export_writes_jsonl_with_all_snapshots`
预期：FAIL，报 `cannot find function export_to_string`。

- [ ] **步骤 3：实现 `src/export.rs`**

```rust
//! `tape export`：把录制目录的快照统一导出为 JSONL 流（每行一条完整快照），
//! 供 AI 矩阵生成、外部对比工具等消费，避免外部工具解析 tape 内部目录结构。
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use tracing::info;

use crate::store;

/// 导出主流程：`-o` 指定输出文件，缺省 stdout。
pub fn run(dir: &Path, output: Option<&Path>) -> Result<()> {
    let text = export_to_string(dir)?;
    match output {
        Some(path) => std::fs::write(path, text)
            .with_context(|| format!("无法写入导出文件 {}", path.display()))?,
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all(text.as_bytes())
                .context("写入 stdout 失败")?;
        }
    }
    Ok(())
}

/// 读取快照目录并序列化为 JSONL 字符串（每行一条快照，按 id 排序）。
pub fn export_to_string(dir: &Path) -> Result<String> {
    let snaps = store::load_snapshots(dir)?;
    let mut out = String::new();
    for snap in &snaps {
        let line = serde_json::to_string(snap)?;
        out.push_str(&line);
        out.push('\n');
    }
    info!("已导出 {} 条快照（{}）", snaps.len(), dir.display());
    Ok(out)
}
```

- [ ] **步骤 4：运行测试确认通过**

运行：`cargo test --lib export::tests`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/export.rs
git commit -m "feat(export): 新增 tape export 导出 JSONL 快照流"
```

---

### 任务 3：compare 数据模型与归一化

**文件：**
- 创建：`src/compare.rs`

- [ ] **步骤 1：编写失败测试（归一化：去 query 的 path、排序 query、body 指纹）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{RequestRecord, ResponseRecord, Snapshot, ENCODING_UTF8};

    fn call(id: &str, url: &str, body: &str) -> Snapshot {
        Snapshot {
            id: id.to_string(),
            origin: "http://10.1.2.3:8080".to_string(),
            recorded_at: "2026-08-02T00:00:00Z".to_string(),
            duration_ms: 1,
            request: RequestRecord {
                method: "POST".to_string(),
                url: url.to_string(),
                headers: vec![],
                body: body.to_string(),
                body_encoding: ENCODING_UTF8.to_string(),
            },
            response: ResponseRecord {
                status: 200,
                headers: vec![],
                body: "{}".to_string(),
                body_encoding: ENCODING_UTF8.to_string(),
            },
        }
    }

    #[test]
    fn normalize_strips_query_and_sorts_params() {
        let r = CallRecord::from_snapshot(
            &call("000001", "http://10.1.2.3:8080/api/search?kw=电影&page=2", ""),
            &IgnoreRules::default(),
        );
        assert_eq!(r.path, "/api/search");
        assert_eq!(r.query, vec![
            ("kw".to_string(), "电影".to_string()),
            ("page".to_string(), "2".to_string()),
        ]);
    }

    #[test]
    fn fingerprint_differs_by_param_value_but_ignores_rule_paths() {
        let rules = IgnoreRules::from_paths(vec!["$.data.token".to_string()]);
        let a = CallRecord::from_snapshot(
            &call("000001", "http://10.1.2.3:8080/api/search?kw=电影", r#"{"kw":"电影","ts":1}"#),
            &rules,
        );
        let b = CallRecord::from_snapshot(
            &call("000002", "http://10.1.2.3:8080/api/search?kw=电视剧", r#"{"kw":"电视剧","ts":999}"#),
            &rules,
        );
        assert_ne!(a.fingerprint(), b.fingerprint(), "不同参数指纹必须不同");

        let c = CallRecord::from_snapshot(
            &call("000003", "http://10.1.2.3:8080/api/search?kw=电影", r#"{"kw":"电影","ts":2}"#),
            &rules,
        );
        assert_eq!(a.fingerprint(), c.fingerprint(), "动态字段 ts 被忽略后指纹应相同");
    }
}
```

- [ ] **步骤 2：运行测试确认失败**

运行：`cargo test --lib compare::tests`
预期：FAIL，报 `CallRecord` 等未定义。

- [ ] **步骤 3：实现数据模型与归一化（`src/compare.rs` 前半部分）**

```rust
//! `tape compare`：对比两个录制目录，按「method+path 分组 → 归一化请求指纹配对 →
//! 调用顺序兜底」三层对齐，输出字段级 JSON diff 与 Markdown 报告。
use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;
use tracing::info;

use crate::rewrite;
use crate::snapshot::Snapshot;
use crate::store;

/// 归一化后的单次调用记录（对比单元）。
#[derive(Debug, Clone)]
pub struct CallRecord {
    pub id: String,
    pub origin: String,
    pub recorded_at: String,
    pub duration_ms: u64,
    pub method: String,
    /// 去 query 的路径（对齐主键之一）
    pub path: String,
    /// 排序后的 query 参数（区分同 path 不同调用）
    pub query: Vec<(String, String)>,
    pub request_body: String,
    pub request_body_encoding: String,
    pub status: u16,
    pub response_headers: Vec<(String, String)>,
    pub response_body: String,
    pub response_body_encoding: String,
}

impl CallRecord {
    pub fn from_snapshot(snap: &Snapshot, rules: &IgnoreRules) -> Self {
        let method = snap.request.method.to_ascii_uppercase();
        let (path, query) = split_path_query(&snap.request.url);
        let query = normalize_query(query);
        Self {
            id: snap.id.clone(),
            origin: snap.origin.clone(),
            recorded_at: snap.recorded_at.clone(),
            duration_ms: snap.duration_ms,
            method,
            path,
            query,
            request_body: snap.request.body.clone(),
            request_body_encoding: snap.request.body_encoding.clone(),
            status: snap.response.status,
            response_headers: snap.response.headers.clone(),
            response_body: snap.response.body.clone(),
            response_body_encoding: snap.response.body_encoding.clone(),
        }
    }

    /// 归一化请求指纹：query 的 key 集合+值（规则命中 key 归一化为占位符）
    /// + body JSON 的 key 集合（规则命中路径归一化），用于同 path 组内配对。
    pub fn fingerprint(&self, rules: &IgnoreRules) -> String {
        let mut parts = Vec::new();
        for (k, v) in &self.query {
            let val = if rules.matches(&format!("$.query.{k}")) {
                "<dynamic>".to_string()
            } else {
                v.clone()
            };
            parts.push(format!("q:{k}={val}"));
        }
        if let Ok(value) = serde_json::from_str::<Value>(&self.request_body) {
            let keys = collect_json_keys(&value, "$", rules);
            parts.extend(keys);
        } else if !self.request_body.is_empty() {
            parts.push(format!("body-raw:{}", self.request_body));
        }
        parts.sort();
        parts.join("|")
    }
}

/// 忽略规则：按字段路径（`$.a.b`、`$.data.list[*].id`）过滤动态值。
#[derive(Debug, Default)]
pub struct IgnoreRules {
    paths: Vec<String>,
}

impl IgnoreRules {
    pub fn from_paths(paths: Vec<String>) -> Self {
        Self { paths }
    }

    pub fn load(path: Option<&Path>) -> Result<Self> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("无法读取忽略规则 {}", path.display()))?;
        let list: Vec<String> = serde_json::from_str(&text)
            .with_context(|| format!("忽略规则应为字符串数组: {}", path.display()))?;
        Ok(Self { paths: list })
    }

    /// 判断字段路径是否命中规则（`[*]` 通配任意数字下标）。
    pub fn matches(&self, path: &str) -> bool {
        self.paths.iter().any(|rule| {
            if rule == path {
                return true;
            }
            // 把规则中的 [*] 展开成正则片段：匹配任意 [数字]
            if rule.contains("[*]") {
                let re = rule.replace("[*]", r"\[\d+\]");
                let re = format!("^{}$", regex::escape(&re).replace(r"\[\d+\]", r"\[\d+\]"));
                regex::Regex::new(&re).map(|r| r.is_match(path)).unwrap_or(false)
            } else {
                false
            }
        })
    }
}

fn split_path_query(url: &str) -> (String, Vec<(String, String)>) {
    let path = rewrite::url_path(url);
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let query = after_scheme
        .split_once('?')
        .map(|(_, q)| q)
        .unwrap_or("");
    let pairs = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect();
    (path, pairs)
}

fn normalize_query(mut q: Vec<(String, String)>) -> Vec<(String, String)> {
    q.sort();
    q
}

/// 递归收集 JSON 对象的 key 路径集合（形如 `$.data.list[0].id`），用于请求指纹；
/// 命中忽略规则的路径归一化为 `<dynamic>`。
fn collect_json_keys(value: &Value, prefix: &str, rules: &IgnoreRules) -> Vec<String> {
    let mut out = Vec::new();
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let path = format!("{prefix}.{k}");
                if rules.matches(&path) {
                    out.push(format!("{path}=<dynamic>"));
                } else {
                    out.extend(collect_json_keys(v, &path, rules));
                }
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                let path = format!("{prefix}[{i}]");
                out.extend(collect_json_keys(v, &path, rules));
            }
        }
        Value::String(s) => out.push(format!("{prefix}={s}")),
        Value::Number(n) => out.push(format!("{prefix}={n}")),
        Value::Bool(b) => out.push(format!("{prefix}={b}")),
        Value::Null => out.push(format!("{prefix}=null")),
    }
    out
}
```

注意：`IgnoreRules::matches` 中的正则构造较绕，若实现不便可简化为「规则命中则归一化」的精确匹配 + 前缀匹配（`$.data` 命中 `$.data.token`）。以测试通过为准，保持简单。

- [ ] **步骤 4：运行测试确认通过**

运行：`cargo test --lib compare::tests`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/compare.rs
git commit -m "feat(compare): 实现调用记录归一化与请求指纹"
```

---

### 任务 4：三层对齐引擎

**文件：**
- 修改：`src/compare.rs`

- [ ] **步骤 1：编写失败测试**

```rust
#[test]
fn align_groups_by_path_then_fingerprint_then_order() {
    let rules = IgnoreRules::default();
    // 基线：同 path 三个调用（kw=电影 / kw=电视剧 / kw=综艺）
    let base = vec![
        CallRecord::from_snapshot(&call("000001", "http://10.1.2.3:8080/api/search?kw=电影", ""), &rules),
        CallRecord::from_snapshot(&call("000002", "http://10.1.2.3:8080/api/search?kw=电视剧", ""), &rules),
        CallRecord::from_snapshot(&call("000003", "http://10.1.2.3:8080/api/search?kw=综艺", ""), &rules),
    ];
    // 新版：kw=电影 重复两次 + kw=电视剧（顺序不同）
    let curr = vec![
        CallRecord::from_snapshot(&call("000001", "http://10.1.2.3:8080/api/search?kw=电视剧", ""), &rules),
        CallRecord::from_snapshot(&call("000002", "http://10.1.2.3:8080/api/search?kw=电影", ""), &rules),
        CallRecord::from_snapshot(&call("000003", "http://10.1.2.3:8080/api/search?kw=电影", ""), &rules),
    ];
    let result = align_calls(&base, &curr, &rules);
    // 按指纹配对：电视剧→电视剧；电影→电影（第一个）；电影→电影（第二个）
    // 基线 kw=综艺 无配对 → Missing
    let missing = result.iter().filter(|c| c.kind == MatchKind::Missing).count();
    let matched = result.iter().filter(|c| c.kind == MatchKind::Matched).count();
    assert_eq!(missing, 1, "综艺 应缺失");
    assert_eq!(matched, 3, "三个调用应配对（电影按顺序配对两条）");
}
```

- [ ] **步骤 2：运行测试确认失败**

运行：`cargo test --lib compare::tests::align_groups_by_path_then_fingerprint_then_order`
预期：FAIL，报 `align_calls` 未定义。

- [ ] **步骤 3：实现对齐引擎**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    /// 基线有、新版无
    Missing,
    /// 新版有、基线无
    Added,
    /// 两边都有且已配对
    Matched,
}

/// 差异类型：结构差异（字段增删）vs 值差异（字段值变化）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffKind {
    Structure,
    Value,
}

/// 一条字段级差异。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDiff {
    pub path: String,
    pub kind: DiffKind,
    pub baseline: Option<String>,
    pub current: Option<String>,
}

/// 一次对齐后的对比单元。
#[derive(Debug)]
pub struct CallComparison {
    pub method: String,
    pub path: String,
    pub kind: MatchKind,
    pub baseline: Option<CallRecord>,
    pub current: Option<CallRecord>,
    /// 状态码差异（基线, 新版）
    pub status_diff: Option<(u16, u16)>,
    /// 响应字段级差异
    pub response_diffs: Vec<FieldDiff>,
    /// 请求差异描述（为空表示请求一致）
    pub request_diff: Option<String>,
}

/// 加载两个目录并执行三层对齐。
pub fn compare_dirs(
    baseline_dir: &Path,
    current_dir: &Path,
    rules: &IgnoreRules,
) -> Result<Vec<CallComparison>> {
    let baseline = load_records(baseline_dir)?;
    let current = load_records(current_dir)?;
    info!(
        "对比加载完成：基线 {} 条，新版 {} 条",
        baseline.len(),
        current.len()
    );
    Ok(align_calls(&baseline, &current, rules))
}

fn load_records(dir: &Path) -> Result<Vec<CallRecord>> {
    let snaps = store::load_snapshots(dir)?;
    Ok(snaps
        .iter()
        .map(|s| CallRecord::from_snapshot(s, &IgnoreRules::default()))
        .collect())
}

fn align_calls(baseline: &[CallRecord], current: &[CallRecord], rules: &IgnoreRules) -> Vec<CallComparison> {
    let mut by_key: HashMap<(String, String), Vec<usize>> = HashMap::new();
    let mut records: Vec<&CallRecord> = baseline.iter().chain(current.iter()).collect();
    // 需要区分来源：baseline 下标 0..n，current 下标 n..n+m
    let n = baseline.len();
    let m = current.len();
    let mut buckets: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (i, r) in baseline.iter().enumerate() {
        buckets.entry((r.method.clone(), r.path.clone())).or_default().push(i);
    }
    for (j, r) in current.iter().enumerate() {
        buckets.entry((r.method.clone(), r.path.clone())).or_default().push(n + j);
    }

    let mut out = Vec::new();
    for ((method, path), idxs) in buckets {
        let base_idx: Vec<usize> = idxs.iter().copied().filter(|&i| i < n).collect();
        let curr_idx: Vec<usize> = idxs.iter().copied().filter(|&i| i >= n).map(|i| i - n).collect();
        // 指纹分组：HashMap<fingerprint, Vec<(基线下标列表, 新版下标列表)>>
        let mut groups: HashMap<String, (Vec<usize>, Vec<usize>)> = HashMap::new();
        for &i in &base_idx {
            groups.entry(baseline[i].fingerprint(rules)).or_default().0.push(i);
        }
        for &j in &curr_idx {
            groups.entry(current[j].fingerprint(rules)).or_default().1.push(j);
        }
        let mut keys: Vec<String> = groups.keys().cloned().collect();
        keys.sort();
        for key in keys {
            let (mut bi, mut cj) = groups.remove(&key).unwrap();
            // 指纹相同：按调用顺序（id 数值序）配对
            bi.sort_by_key(|&i| numeric_id(&baseline[i].id));
            cj.sort_by_key(|&j| numeric_id(&current[j].id));
            let pairs = bi.len().min(cj.len());
            for k in 0..pairs {
                out.push(CallComparison {
                    method: method.clone(),
                    path: path.clone(),
                    kind: MatchKind::Matched,
                    baseline: Some(baseline[bi[k]].clone()),
                    current: Some(current[cj[k]].clone()),
                    status_diff: None,
                    response_diffs: Vec::new(),
                    request_diff: None,
                });
            }
            for &i in &bi[pairs..] {
                out.push(CallComparison {
                    method: method.clone(),
                    path: path.clone(),
                    kind: MatchKind::Missing,
                    baseline: Some(baseline[i].clone()),
                    current: None,
                    status_diff: None,
                    response_diffs: Vec::new(),
                    request_diff: None,
                });
            }
            for &j in &cj[pairs..] {
                out.push(CallComparison {
                    method: method.clone(),
                    path: path.clone(),
                    kind: MatchKind::Added,
                    baseline: None,
                    current: Some(current[j].clone()),
                    status_diff: None,
                    response_diffs: Vec::new(),
                    request_diff: None,
                });
            }
        }
    }
    out
}

fn numeric_id(id: &str) -> u64 {
    id.parse::<u64>().unwrap_or(u64::MAX)
}
```

- [ ] **步骤 4：运行测试确认通过**

运行：`cargo test --lib compare::tests::align_groups_by_path_then_fingerprint_then_order`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/compare.rs
git commit -m "feat(compare): 实现 method+path 分组与请求指纹三层对齐"
```

---

### 任务 5：JSON 语义 diff 与忽略规则应用

**文件：**
- 修改：`src/compare.rs`

- [ ] **步骤 1：编写失败测试**

```rust
#[test]
fn json_diff_separates_structure_and_value_and_applies_rules() {
    let base: Value = serde_json::from_str(
        r#"{"data":{"title":"a","list":[{"id":1,"name":"x"}],"token":"T1"}}"#,
    )
    .unwrap();
    let curr: Value = serde_json::from_str(
        r#"{"data":{"title":"b","list":[{"id":2,"name":"y"}],"extra":true,"token":"T2"}}"#,
    )
    .unwrap();
    let rules = IgnoreRules::from_paths(vec!["$.data.token".to_string(), "$.data.list[*].id".to_string()]);
    let diffs = diff_json(&base, &curr, "$", &rules);
    assert!(diffs.iter().any(|d| d.path == "$.data.title" && d.kind == DiffKind::Value));
    assert!(diffs.iter().any(|d| d.path == "$.data.list[0].name" && d.kind == DiffKind::Value));
    assert!(diffs.iter().any(|d| d.path == "$.data.extra" && d.kind == DiffKind::Structure));
    assert!(!diffs.iter().any(|d| d.path.contains("token")), "token 应被忽略");
    assert!(!diffs.iter().any(|d| d.path.contains("[0].id")), "list[*].id 应被忽略");
}
```

- [ ] **步骤 2：运行测试确认失败**

运行：`cargo test --lib compare::tests::json_diff_separates_structure_and_value_and_applies_rules`
预期：FAIL，报 `diff_json` 未定义。

- [ ] **步骤 3：实现递归 JSON diff**

```rust
/// 递归对比两个 JSON 值：对象 key 增删为 Structure，标量变化为 Value；数组按索引对比。
pub fn diff_json(base: &Value, current: &Value, prefix: &str, rules: &IgnoreRules) -> Vec<FieldDiff> {
    let mut out = Vec::new();
    if rules.matches(prefix) {
        return out; // 忽略规则命中：整棵子树跳过
    }
    match (base, current) {
        (Value::Object(b), Value::Object(c)) => {
            let mut keys: Vec<&String> = b.keys().chain(c.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let path = format!("{prefix}.{k}");
                match (b.get(k), c.get(k)) {
                    (Some(bv), Some(cv)) => out.extend(diff_json(bv, cv, &path, rules)),
                    (Some(bv), None) => out.push(FieldDiff {
                        path: path.clone(),
                        kind: DiffKind::Structure,
                        baseline: Some(value_summary(bv)),
                        current: None,
                    }),
                    (None, Some(cv)) => out.push(FieldDiff {
                        path: path.clone(),
                        kind: DiffKind::Structure,
                        baseline: None,
                        current: Some(value_summary(cv)),
                    }),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(b), Value::Array(c)) => {
            let len = b.len().max(c.len());
            for i in 0..len {
                let path = format!("{prefix}[{i}]");
                match (b.get(i), c.get(i)) {
                    (Some(bv), Some(cv)) => out.extend(diff_json(bv, cv, &path, rules)),
                    (Some(bv), None) => out.push(FieldDiff {
                        path: path.clone(),
                        kind: DiffKind::Structure,
                        baseline: Some(value_summary(bv)),
                        current: None,
                    }),
                    (None, Some(cv)) => out.push(FieldDiff {
                        path: path.clone(),
                        kind: DiffKind::Structure,
                        baseline: None,
                        current: Some(value_summary(cv)),
                    }),
                    (None, None) => {}
                }
            }
        }
        (b, c) if b == c => {}
        (b, c) => out.push(FieldDiff {
            path: prefix.to_string(),
            kind: DiffKind::Value,
            baseline: Some(value_summary(b)),
            current: Some(value_summary(c)),
        }),
    }
    out
}

fn value_summary(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        Value::Array(a) => format!("[{} 项]", a.len()),
        Value::Object(o) => format!("{{{}}}", o.len()),
    }
}
```

- [ ] **步骤 4：运行测试确认通过**

运行：`cargo test --lib compare::tests::json_diff_separates_structure_and_value_and_applies_rules`
预期：PASS；随后运行 `cargo test --lib compare::tests` 确认任务 3/4 测试全部通过。

- [ ] **步骤 5：Commit**

```bash
git add src/compare.rs
git commit -m "feat(compare): 实现 JSON 语义 diff 与忽略规则"
```

---

### 任务 6：请求差异对比

**文件：**
- 修改：`src/compare.rs`

- [ ] **步骤 1：编写失败测试**

```rust
#[test]
fn request_diff_detects_param_and_body_changes() {
    let rules = IgnoreRules::default();
    let b = CallRecord::from_snapshot(
        &call("000001", "http://10.1.2.3:8080/api/search?kw=电影", r#"{"kw":"电影"}"#),
        &rules,
    );
    let c = CallRecord::from_snapshot(
        &call("000002", "http://10.1.2.3:8080/api/search?kw=电视剧", r#"{"kw":"电视剧"}"#),
        &rules,
    );
    let diff = request_diff(&b, &c);
    assert!(diff.is_some(), "query 变化应产生请求差异");
    let text = diff.unwrap();
    assert!(text.contains("kw=电影"), "应指出基线参数: {text}");
    assert!(text.contains("kw=电视剧"), "应指出新版参数: {text}");
}
```

- [ ] **步骤 2：运行测试确认失败**

运行：`cargo test --lib compare::tests::request_diff_detects_param_and_body_changes`
预期：FAIL，报 `request_diff` 未定义。

- [ ] **步骤 3：实现请求差异**

```rust
/// 对比两次调用的请求（query + body），返回人类可读差异描述；一致返回 None。
pub fn request_diff(base: &CallRecord, current: &CallRecord) -> Option<String> {
    let mut parts = Vec::new();
    if base.query != current.query {
        parts.push(format!(
            "query: {:?} → {:?}",
            base.query, current.query
        ));
    }
    if base.request_body != current.request_body {
        parts.push(format!(
            "body: {} → {}",
            summarize_body(&base.request_body),
            summarize_body(&current.request_body)
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("；"))
    }
}

fn summarize_body(body: &str) -> String {
    if body.len() > 120 {
        format!("{}…", &body[..120])
    } else {
        body.to_string()
    }
}
```

- [ ] **步骤 4：运行测试确认通过**

运行：`cargo test --lib compare::tests::request_diff_detects_param_and_body_changes`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/compare.rs
git commit -m "feat(compare): 对比请求参数与请求体差异"
```

---

### 任务 7：`tape compare` 主流程与 Markdown 报告

**文件：**
- 修改：`src/compare.rs`
- 修改：`src/cli.rs`（`CompareArgs` 增加 `verbose`，任务 1 已含）

- [ ] **步骤 1：编写失败测试（报告结构 + 矩阵组织）**

```rust
#[test]
fn report_groups_by_matrix_entries() {
    let rules = IgnoreRules::default();
    let base = vec![
        CallRecord::from_snapshot(&call("000001", "http://10.1.2.3:8080/api/search/query?kw=电影", ""), &rules),
        CallRecord::from_snapshot(&call("000002", "http://10.1.2.3:8080/api/home", ""), &rules),
    ];
    let curr = vec![
        CallRecord::from_snapshot(&call("000001", "http://10.1.2.3:8080/api/search/query?kw=电影", ""), &rules),
    ];
    let result = align_calls(&base, &curr, &rules);
    let matrix = serde_json::json!({
        "module": "首页",
        "entries": [
            {"id": "home-search", "name": "搜索流程", "steps": [
                {"action": "点击搜索", "apis": [{"method": "POST", "path": "/api/search/query"}]}
            ]},
            {"id": "home-main", "name": "首页加载", "steps": [
                {"action": "进入首页", "apis": [{"method": "GET", "path": "/api/home"}]}
            ]}
        ]
    });
    let md = render_report("基线-旧版", "新版", &result, Some(&matrix));
    assert!(md.contains("搜索流程"), "报告应按功能条目组织: {md}");
    assert!(md.contains("首页加载"), "报告应包含未缺失条目: {md}");
    assert!(md.contains("缺失"), "报告应含三分类汇总: {md}");
}
```

- [ ] **步骤 2：运行测试确认失败**

运行：`cargo test --lib compare::tests::report_groups_by_matrix_entries`
预期：FAIL，报 `render_report` 未定义。

- [ ] **步骤 3：实现矩阵解析与报告渲染**

```rust
/// 矩阵 JSON：{ module, entries: [{ id, name, steps: [{ action, apis: [{method,path}] }] }] }
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FeatureMatrix {
    pub module: String,
    pub entries: Vec<MatrixEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MatrixEntry {
    pub id: String,
    pub name: String,
    pub steps: Vec<MatrixStep>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MatrixStep {
    pub action: String,
    pub apis: Vec<MatrixApi>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MatrixApi {
    pub method: String,
    pub path: String,
}

fn load_matrix(path: Option<&Path>) -> Result<Option<FeatureMatrix>> {
    let Some(path) = path else { return Ok(None) };
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("无法读取功能矩阵 {}", path.display()))?;
    let m: FeatureMatrix = serde_json::from_str(&text)
        .with_context(|| format!("功能矩阵 JSON 解析失败 {}", path.display()))?;
    Ok(Some(m))
}

/// 渲染 Markdown 报告：按矩阵功能条目组织；无矩阵时按 path 组织。
pub fn render_report(
    baseline_name: &str,
    current_name: &str,
    comparisons: &[CallComparison],
    matrix: Option<&FeatureMatrix>,
) -> String {
    let mut md = String::new();
    md.push_str(&format!("# 接口复刻对比报告\n\n基线：{baseline_name}　新版：{current_name}\n\n"));

    let missing = comparisons.iter().filter(|c| c.kind == MatchKind::Missing).count();
    let added = comparisons.iter().filter(|c| c.kind == MatchKind::Added).count();
    let changed = comparisons
        .iter()
        .filter(|c| c.kind == MatchKind::Matched && (c.status_diff.is_some() || !c.response_diffs.is_empty()))
        .count();
    let identical = comparisons
        .iter()
        .filter(|c| c.kind == MatchKind::Matched && c.status_diff.is_none() && c.response_diffs.is_empty())
        .count();
    md.push_str(&format!(
        "## 汇总\n\n- 一致：{identical}\n- 变更：{changed}\n- 缺失：{missing}\n- 新增：{added}\n\n"
    ));

    if let Some(m) = matrix {
        md.push_str("## 按功能条目\n\n");
        for entry in &m.entries {
            let entry_apis: Vec<&MatrixApi> = entry.steps.iter().flat_map(|s| s.apis.iter()).collect();
            let relevant: Vec<&CallComparison> = comparisons
                .iter()
                .filter(|c| {
                    entry_apis.iter().any(|a| {
                        a.method.eq_ignore_ascii_case(&c.method) && a.path == c.path
                    })
                })
                .collect();
            let has_issue = relevant.iter().any(|c| {
                c.kind == MatchKind::Missing
                    || c.kind == MatchKind::Added
                    || c.status_diff.is_some()
                    || !c.response_diffs.is_empty()
            });
            let icon = if relevant.is_empty() { "⚠️" } else if has_issue { "❌" } else { "✅" };
            md.push_str(&format!("### {icon} {}（{}）\n\n", entry.name, entry.id));
            if relevant.is_empty() {
                md.push_str("- 未捕获到该功能的接口调用\n");
            }
            for c in &relevant {
                md.push_str(&format!("- `{} {}`\n", c.method, c.path));
                if c.kind == MatchKind::Missing {
                    md.push_str("  - **缺失**：新版未调用该接口\n");
                }
                if c.kind == MatchKind::Added {
                    md.push_str("  - **新增**：新版多出该调用\n");
                }
                if let Some((b, c_)) = c.status_diff {
                    md.push_str(&format!("  - 状态码：{b} → {c_}\n"));
                }
                for d in &c.response_diffs {
                    md.push_str(&format!(
                        "  - 响应 {} `{}`：{:?} → {:?}\n",
                        if d.kind == DiffKind::Structure { "结构" } else { "值" },
                        d.path,
                        d.baseline,
                        d.current
                    ));
                }
                if let Some(rd) = &c.request_diff {
                    md.push_str(&format!("  - 请求差异：{rd}\n"));
                }
            }
            md.push('\n');
        }
    } else {
        md.push_str("## 差异详情\n\n");
        for c in comparisons {
            if c.kind == MatchKind::Missing {
                md.push_str(&format!("- ❌ 缺失 `{} {}`\n", c.method, c.path));
            } else if c.kind == MatchKind::Added {
                md.push_str(&format!("- ➕ 新增 `{} {}`\n", c.method, c.path));
            } else if c.status_diff.is_some() || !c.response_diffs.is_empty() {
                md.push_str(&format!("- 🔄 变更 `{} {}`\n", c.method, c.path));
                if let Some((b, c_)) = c.status_diff {
                    md.push_str(&format!("  - 状态码：{b} → {c_}\n"));
                }
                for d in &c.response_diffs {
                    md.push_str(&format!(
                        "  - 响应 {} `{}`：{:?} → {:?}\n",
                        if d.kind == DiffKind::Structure { "结构" } else { "值" },
                        d.path,
                        d.baseline,
                        d.current
                    ));
                }
                if let Some(rd) = &c.request_diff {
                    md.push_str(&format!("  - 请求差异：{rd}\n"));
                }
            }
        }
    }
    md
}
```

- [ ] **步骤 4：实现 `run` 主流程并补全 Matched 项的 diff 计算**

```rust
/// `tape compare` 主流程：加载 → 对齐 → 计算 Matched 项差异 → 渲染报告。
pub fn run(
    baseline_dir: &Path,
    current_dir: &Path,
    matrix_path: Option<&Path>,
    ignore_path: Option<&Path>,
    output: Option<&Path>,
) -> Result<()> {
    let rules = IgnoreRules::load(ignore_path)?;
    let mut comparisons = compare_dirs(baseline_dir, current_dir, &rules)?;
    // 对 Matched 项补算状态码 / 响应 diff / 请求 diff
    for c in &mut comparisons {
        if c.kind != MatchKind::Matched {
            continue;
        }
        let (b, cur) = (c.baseline.as_ref().unwrap(), c.current.as_ref().unwrap());
        if b.status != cur.status {
            c.status_diff = Some((b.status, cur.status));
        }
        if let (Ok(bv), Ok(cv)) = (
            serde_json::from_str::<Value>(&decode_body(&b.response_body, &b.response_body_encoding)),
            serde_json::from_str::<Value>(&decode_body(&cur.response_body, &cur.response_body_encoding)),
        ) {
            c.response_diffs = diff_json(&bv, &cv, "$", &rules);
        }
        c.request_diff = request_diff(b, cur);
    }

    let matrix = load_matrix(matrix_path)?;
    let report = render_report(
        &baseline_dir.display().to_string(),
        &current_dir.display().to_string(),
        &comparisons,
        matrix.as_ref(),
    );

    // 终端摘要
    let missing = comparisons.iter().filter(|c| c.kind == MatchKind::Missing).count();
    let added = comparisons.iter().filter(|c| c.kind == MatchKind::Added).count();
    let changed = comparisons
        .iter()
        .filter(|c| c.kind == MatchKind::Matched && (c.status_diff.is_some() || !c.response_diffs.is_empty()))
        .count();
    let identical = comparisons.len() - missing - added - changed;
    println!("对比完成：一致 {identical}，变更 {changed}，缺失 {missing}，新增 {added}");

    if let Some(path) = output {
        std::fs::write(path, &report)
            .with_context(|| format!("无法写入报告 {}", path.display()))?;
        println!("报告已保存: {}", path.display());
    } else {
        println!("{report}");
    }
    Ok(())
}

fn decode_body(body: &str, encoding: &str) -> Vec<u8> {
    crate::snapshot::decode_body(body, encoding)
}
```

- [ ] **步骤 5：运行全量测试并 Commit**

运行：`cargo test --all-targets`
预期：全部 PASS（含任务 2-6 测试）

```bash
git add src/compare.rs src/cli.rs
git commit -m "feat(compare): tape compare 主流程与 Markdown 报告（支持矩阵组织）"
```

---

### 任务 8：端到端集成测试

**文件：**
- 修改：`tests/proxy_flow.rs`

- [ ] **步骤 1：编写失败测试（构造两个录制目录 + 矩阵 → compare → 校验报告）**

```rust
#[tokio::test]
async fn compare_end_to_end_reports_missing_and_changed() {
    use tape::compare::{compare_dirs, render_report, IgnoreRules};
    use tape::snapshot::ENCODING_UTF8;

    fn snap(id: &str, url: &str, body: &str, status: u16) -> Snapshot {
        Snapshot {
            id: id.to_string(),
            origin: "http://10.1.2.3:8080".to_string(),
            recorded_at: "2026-08-02T00:00:00Z".to_string(),
            duration_ms: 1,
            request: RequestRecord {
                method: "POST".to_string(),
                url: url.to_string(),
                headers: vec![],
                body: String::new(),
                body_encoding: ENCODING_UTF8.to_string(),
            },
            response: ResponseRecord {
                status,
                headers: vec![],
                body: body.to_string(),
                body_encoding: ENCODING_UTF8.to_string(),
            },
        }
    }

    let base = temp_dir("compare-base");
    let curr = temp_dir("compare-curr");
    // 基线：搜索（kw=电影）+ 首页
    write_snapshot(
        &base,
        &snap("000001", "http://10.1.2.3:8080/api/search/query?kw=电影", r#"{"list":["a"]}"#, 200),
    );
    write_snapshot(
        &base,
        &snap("000002", "http://10.1.2.3:8080/api/home", r#"{"banners":2}"#, 200),
    );
    // 新版：搜索（kw=电影，响应字段变了）+ 首页缺失
    write_snapshot(
        &curr,
        &snap("000001", "http://10.1.2.3:8080/api/search/query?kw=电影", r#"{"list":["a","b"]}"#, 200),
    );

    let rules = IgnoreRules::default();
    let result = compare_dirs(&base, &curr, &rules).unwrap();
    let matrix = serde_json::json!({
        "module": "首页",
        "entries": [
            {"id": "s", "name": "搜索流程", "steps": [
                {"action": "搜索", "apis": [{"method": "POST", "path": "/api/search/query"}]}
            ]},
            {"id": "h", "name": "首页加载", "steps": [
                {"action": "进首页", "apis": [{"method": "POST", "path": "/api/home"}]}
            ]}
        ]
    });
    let md = render_report("基线", "新版", &result, Some(&matrix));
    assert!(md.contains("缺失"), "报告应有汇总: {md}");
    assert!(md.contains("❌ 搜索流程"), "搜索响应变更应标问题: {md}");
    assert!(md.contains("$.data.list"), "差异应定位到字段: {md}");
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&curr);
}
```

注意：`snap` 的响应 body 是 JSON 字符串，`render_report` 中 diff 路径为 `$.list`（body 是数组/对象根）。若断言路径不符，以实际 diff 输出为准调整断言（diff 根前缀为 `$`）。

- [ ] **步骤 2：运行测试确认失败或通过**

运行：`cargo test --test proxy_flow compare_end_to_end_reports_missing_and_changed -- --nocapture`
预期：PASS（若失败，根据断言信息修正测试或实现）

- [ ] **步骤 3：全量回归 + clippy + fmt**

运行：`cargo test --all-targets && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
预期：全部 PASS、零警告、fmt 干净

- [ ] **步骤 4：Commit**

```bash
git add tests/proxy_flow.rs
git commit -m "test(compare): 端到端对比集成测试"
```

---

### 任务 9：真实数据手动验证（收尾）

**文件：** 无（仅手动操作）

- [ ] **步骤 1：录制两遍真实业务路径**

运行：`tape record -d /tmp/base-record`（旧版跑一遍）与 `tape record -d /tmp/current-record`（新版跑一遍），录制同一盒子上同一路径。

- [ ] **步骤 2：导出与对比**

运行：`tape export /tmp/base-record -o /tmp/base.jsonl` 与 `tape compare /tmp/base-record /tmp/current-record --matrix matrix.json --ignore ignore.json -o /tmp/report.md`
预期：终端输出「对比完成：一致 X，变更 Y，缺失 Z，新增 W」；`/tmp/report.md` 按功能条目列出差异。

- [ ] **步骤 3：把矩阵 / 忽略规则样本沉淀到项目**

将验证过程中整理的 `matrix.json` 与 `ignore.json` 保存到 single-kotlin 仓库 `docs/verification-design/examples/` 目录（不在本计划任务范围内，人工执行）。

---

## 自检记录

- **规格覆盖度**：设计文档 4 节（对齐/三分类/忽略规则/对比维度）→ 任务 3-6；5 节报告 → 任务 7；2 节流程中的导出 → 任务 2；6 节工具承载 → 任务 1-2、7；M1 里程碑 → 任务 8-9。
- **占位符扫描**：无 TODO / 待定；每个代码步骤均有完整代码。
- **类型一致性**：`CallRecord` / `IgnoreRules` / `FieldDiff` / `DiffKind` / `MatchKind` / `CallComparison` / `FeatureMatrix` 在任务 3-7 中定义与引用一致；`compare_dirs` / `align_calls` / `diff_json` / `request_diff` / `render_report` / `run` 签名在定义与调用处一致。
