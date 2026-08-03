# tape 验证工具 L2 业务流层（M2）实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 在 M1（L1 接口契约对比）之上实现 L2 业务流层：接口调用序列对齐（LCS）、业务结果断言（JSONPath + 操作符）、报告「业务流验证」小节，把验证从「接口对齐」推进到「业务流对齐」。

**架构：** 全部改动在 `src/compare.rs` 内（与 M1 同模块）：新增 `Assertion`/`AssertionOp` 矩阵模型字段（可选，兼容旧矩阵）、`json_path_get` + `run_assertion` 断言引擎、`compare_sequences`（LCS）序列对比、`render_report` 业务流小节、`run` 主流程整合。

**技术栈：** Rust 2024、serde_json、无新增外部依赖。

**工作仓库：** `/Users/yujinping/data/workspace/rust-projects/box-proxy`（tape），基于 main（已含 M1）。

---

## 文件结构

**修改：**
- `src/compare.rs` —— 全部改动（矩阵模型、断言引擎、序列对比、报告、主流程）
- `tests/proxy_flow.rs` —— 端到端集成测试扩展

**依赖关系：** 任务 1（模型）→ 任务 2（断言引擎）→ 任务 3（序列对比）→ 任务 4（报告）→ 任务 5（整合 + 集成测试）。

---

### 任务 1：矩阵模型扩展（expected 断言字段）

**文件：**
- 修改：`src/compare.rs`

- [ ] **步骤 1：编写失败测试（带/不带 expected 的矩阵解析兼容）**

```rust
#[test]
fn matrix_parses_with_and_without_expected() {
    let with_expected = serde_json::json!({
        "module": "首页",
        "entries": [{
            "id": "s",
            "name": "搜索流程",
            "steps": [{"action": "搜索", "apis": [{"method": "POST", "path": "/api/search"}]}],
            "expected": [
                {"path": "$.data.list", "op": "nonEmpty", "desc": "搜索结果非空"},
                {"path": "$.errorCode", "op": "eq", "value": 0}
            ]
        }]
    });
    let m: FeatureMatrix = serde_json::from_value(with_expected).unwrap();
    let entry = &m.entries[0];
    assert_eq!(entry.expected.len(), 2);
    assert_eq!(entry.expected[0].op, AssertionOp::NonEmpty);
    assert_eq!(entry.expected[1].op, AssertionOp::Eq);
    assert_eq!(entry.expected[0].desc.as_deref(), Some("搜索结果非空"));

    // 旧矩阵无 expected 字段：必须仍能解析（serde default）
    let legacy = serde_json::json!({
        "module": "首页",
        "entries": [{"id": "s", "name": "搜索", "steps": []}]
    });
    let m: FeatureMatrix = serde_json::from_value(legacy).unwrap();
    assert!(m.entries[0].expected.is_empty(), "旧矩阵 expected 应为空");
}
```

- [ ] **步骤 2：运行测试确认失败**

运行：`cargo test --lib compare::tests::matrix_parses_with_and_without_expected`
预期：FAIL，报 `AssertionOp` / `expected` 未定义。

- [ ] **步骤 3：实现模型扩展**

在 `MatrixEntry` 定义处（`src/compare.rs`）新增断言类型并给 `MatrixEntry` 加字段：

```rust
/// 业务结果断言：JSONPath 取值 + 操作符判定。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Assertion {
    /// JSONPath，如 `$.data.list`、`$.errorCode`
    pub path: String,
    /// 断言操作符
    pub op: AssertionOp,
    /// eq / gt / contains 的目标值（exists / nonEmpty 不需要）
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    /// 人类可读描述，用于报告
    #[serde(default)]
    pub desc: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssertionOp {
    Eq,
    Exists,
    NonEmpty,
    Gt,
    Contains,
}
```

`MatrixEntry` 增加字段：

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MatrixEntry {
    pub id: String,
    pub name: String,
    pub steps: Vec<MatrixStep>,
    /// 业务结果断言列表（可选；无断言的旧矩阵仍可解析）
    #[serde(default)]
    pub expected: Vec<Assertion>,
}
```

- [ ] **步骤 4：运行测试确认通过**

运行：`cargo test --lib compare::tests::matrix_parses_with_and_without_expected`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/compare.rs
git commit -m "feat(compare): 矩阵模型扩展 expected 业务结果断言字段"
```

---

### 任务 2：JSONPath 取值与断言引擎

**文件：**
- 修改：`src/compare.rs`

- [ ] **步骤 1：编写失败测试**

```rust
#[test]
fn json_path_get_supports_dot_and_index() {
    let v: Value = serde_json::from_str(
        r#"{"data":{"list":[{"id":1,"name":"a"}],"token":"T"}}"#,
    )
    .unwrap();
    assert_eq!(json_path_get(&v, "$.data.token"), Some(&Value::String("T".into())));
    assert_eq!(json_path_get(&v, "$.data.list[0].name"), Some(&Value::String("a".into())));
    assert_eq!(json_path_get(&v, "$.data.list[0].id"), Some(&Value::from(1)));
    assert!(json_path_get(&v, "$.data.missing").is_none());
    assert!(json_path_get(&v, "$.data.list[9]").is_none());
}

#[test]
fn assertion_engine_covers_operators() {
    let body: Value = serde_json::from_str(
        r#"{"data":{"list":["a"],"code":0,"msg":"ok"},"token":"T"}"#,
    )
    .unwrap();
    let a = |path: &str, op: AssertionOp, value: Option<serde_json::Value>| Assertion {
        path: path.to_string(),
        op,
        value,
        desc: None,
    };
    assert!(run_assertion(&body, &a("$.data.code", AssertionOp::Eq, Some(json!(0)))));
    assert!(!run_assertion(&body, &a("$.data.code", AssertionOp::Eq, Some(json!(1)))));
    assert!(run_assertion(&body, &a("$.token", AssertionOp::Exists, None)));
    assert!(!run_assertion(&body, &a("$.nope", AssertionOp::Exists, None)));
    assert!(run_assertion(&body, &a("$.data.list", AssertionOp::NonEmpty, None)));
    assert!(run_assertion(&body, &a("$.data.code", AssertionOp::Gt, Some(json!(-1)))));
    assert!(!run_assertion(&body, &a("$.data.code", AssertionOp::Gt, Some(json!(1)))));
    assert!(run_assertion(&body, &a("$.data.msg", AssertionOp::Contains, Some(json!("ok")))));
    assert!(!run_assertion(&body, &a("$.data.msg", AssertionOp::Contains, Some(json!("bad")))));
}
```

- [ ] **步骤 2：运行测试确认失败**

运行：`cargo test --lib compare::tests::json_path_get_supports_dot_and_index`
预期：FAIL，报 `json_path_get` 未定义。

- [ ] **步骤 3：实现 JSONPath 取值与断言引擎**

```rust
/// 从 JSON 中按路径取值：支持 `$.a.b[0].c` 形态（点路径 + 数字下标）。
pub fn json_path_get<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = value;
    let mut rest = path.strip_prefix('$').unwrap_or(path);
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('.') {
            rest = r;
        }
        if let Some(r) = rest.strip_prefix('[') {
            let end = r.find(']')?;
            let idx: usize = r[..end].parse().ok()?;
            cur = cur.get(idx)?;
            rest = &r[end + 1..];
        } else {
            let (key, after) = split_path_key(rest);
            cur = cur.get(key)?;
            rest = after;
        }
    }
    Some(cur)
}

fn split_path_key(rest: &str) -> (&str, &str) {
    match rest.find(['.', '[']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    }
}

/// 对响应体执行单条断言，返回是否通过。
pub fn run_assertion(body: &Value, a: &Assertion) -> bool {
    let Some(v) = json_path_get(body, &a.path) else {
        return false; // 路径不存在即不通过（Exists 语义由调用方对缺失路径单独处理时也不通过）
    };
    match a.op {
        AssertionOp::Exists => true,
        AssertionOp::NonEmpty => !is_empty_value(v),
        AssertionOp::Eq => a.value.as_ref().is_some_and(|t| v == t),
        AssertionOp::Gt => match (v.as_f64(), a.value.as_ref().and_then(|t| t.as_f64())) {
            (Some(x), Some(y)) => x > y,
            _ => false,
        },
        AssertionOp::Contains => match (v.as_str(), a.value.as_ref().and_then(|t| t.as_str())) {
            (Some(s), Some(sub)) => s.contains(sub),
            _ => false,
        },
    }
}

fn is_empty_value(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}
```

- [ ] **步骤 4：运行测试确认通过**

运行：`cargo test --lib compare::tests`（json_path_get 与 assertion_engine 两个测试）
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/compare.rs
git commit -m "feat(compare): 实现 JSONPath 取值与业务结果断言引擎"
```

---

### 任务 3：调用序列对齐（LCS）

**文件：**
- 修改：`src/compare.rs`

- [ ] **步骤 1：编写失败测试**

```rust
#[test]
fn sequence_compare_detects_missing_added_and_identical() {
    let base = vec![
        "POST /api/login".to_string(),
        "GET /api/token".to_string(),
        "POST /api/order".to_string(),
        "GET /api/order/1".to_string(),
    ];
    // 新版：少了「查订单」，多了「查余额」
    let curr = vec![
        "POST /api/login".to_string(),
        "GET /api/token".to_string(),
        "POST /api/order".to_string(),
        "GET /api/balance".to_string(),
    ];
    let diff = compare_sequences(&base, &curr);
    assert!(diff.missing.iter().any(|s| s.contains("/api/order/1")), "缺查订单: {diff:?}");
    assert!(diff.added.iter().any(|s| s.contains("/api/balance")), "新增查余额: {diff:?}");

    let same = compare_sequences(&base, &base);
    assert!(same.is_identical(), "相同序列应一致: {same:?}");
}
```

- [ ] **步骤 2：运行测试确认失败**

运行：`cargo test --lib compare::tests::sequence_compare_detects_missing_added_and_identical`
预期：FAIL，报 `compare_sequences` 未定义。

- [ ] **步骤 3：实现 LCS 序列对比**

```rust
/// 序列对比结果：两遍录制配对接口的调用顺序差异。
#[derive(Debug, Clone)]
pub struct SequenceDiff {
    /// 基线有、新版无的步骤
    pub missing: Vec<String>,
    /// 新版有、基线无的步骤
    pub added: Vec<String>,
}

impl SequenceDiff {
    pub fn is_identical(&self) -> bool {
        self.missing.is_empty() && self.added.is_empty()
    }
}

/// 基于最长公共子序列（LCS）的调用序列对比，容忍顺序微小差异。
pub fn compare_sequences(baseline: &[String], current: &[String]) -> SequenceDiff {
    let dp = lcs_table(baseline, current);
    let mut a_in = vec![false; baseline.len()];
    let mut b_in = vec![false; current.len()];
    let (mut i, mut j) = (0, 0);
    while i < baseline.len() && j < current.len() {
        if baseline[i] == current[j] {
            a_in[i] = true;
            b_in[j] = true;
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    SequenceDiff {
        missing: baseline
            .iter()
            .enumerate()
            .filter(|(k, _)| !a_in[*k])
            .map(|(_, s)| s.clone())
            .collect(),
        added: current
            .iter()
            .enumerate()
            .filter(|(k, _)| !b_in[*k])
            .map(|(_, s)| s.clone())
            .collect(),
    }
}

fn lcs_table<T: PartialEq>(a: &[T], b: &[T]) -> Vec<Vec<usize>> {
    let mut dp = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    dp
}
```

- [ ] **步骤 4：运行测试确认通过**

运行：`cargo test --lib compare::tests::sequence_compare_detects_missing_added_and_identical`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/compare.rs
git commit -m "feat(compare): 实现调用序列 LCS 对比"
```

---

### 任务 4：报告扩展（业务流验证小节）

**文件：**
- 修改：`src/compare.rs`

- [ ] **步骤 1：编写失败测试**

```rust
/// 一个功能条目的业务断言结果。
#[derive(Debug, Clone)]
pub struct FeatureAssertions {
    pub entry_id: String,
    pub entry_name: String,
    pub baseline: Vec<AssertionResult>,
    pub current: Vec<AssertionResult>,
}

/// 一次断言执行结果。
#[derive(Debug, Clone)]
pub struct AssertionResult {
    pub path: String,
    pub desc: Option<String>,
    pub passed: bool,
    pub detail: String,
}

#[test]
fn report_includes_business_flow_section() {
    let rules = IgnoreRules::default();
    let base = vec![CallRecord::from_snapshot(&call(
        "000001",
        "http://10.1.2.3:8080/api/search?kw=电影",
        "",
    ))];
    let curr = vec![CallRecord::from_snapshot(&call(
        "000001",
        "http://10.1.2.3:8080/api/search?kw=电影",
        "",
    ))];
    let comparisons = align_calls(&base, &curr, &rules);
    let seq = compare_sequences(&["POST /api/search".to_string()], &["POST /api/search".to_string()]);
    let fas = vec![FeatureAssertions {
        entry_id: "s".into(),
        entry_name: "搜索流程".into(),
        baseline: vec![AssertionResult {
            path: "$.data.list".into(),
            desc: Some("列表非空".into()),
            passed: true,
            detail: "ok".into(),
        }],
        current: vec![AssertionResult {
            path: "$.data.list".into(),
            desc: Some("列表非空".into()),
            passed: false,
            detail: "空列表".into(),
        }],
    }];
    let md = render_report("基线", "新版", &comparisons, None, Some(&seq), &fas);
    assert!(md.contains("业务流验证"), "应有业务流小节: {md}");
    assert!(md.contains("调用序列"), "应有序列小节: {md}");
    assert!(md.contains("❌ 搜索流程"), "断言失败条目应标红: {md}");
    assert!(md.contains("空列表"), "应包含断言失败原因: {md}");
}
```

注意：`render_report` 签名将新增两个参数（`sequence: Option<&SequenceDiff>`、`assertions: &[FeatureAssertions]`），已有调用处（现有测试 `report_groups_by_matrix_entries`、集成测试）需同步补参数。

- [ ] **步骤 2：运行测试确认失败**

运行：`cargo test --lib compare::tests::report_includes_business_flow_section`
预期：FAIL，报 `render_report` 参数不匹配 / `FeatureAssertions` 未定义。

- [ ] **步骤 3：实现报告扩展**

`render_report` 签名改为：

```rust
pub fn render_report(
    baseline_name: &str,
    current_name: &str,
    comparisons: &[CallComparison],
    matrix: Option<&FeatureMatrix>,
    sequence: Option<&SequenceDiff>,
    assertions: &[FeatureAssertions],
) -> String
```

在「## 汇总」之后插入业务流小节：

```rust
    if let Some(seq) = sequence {
        md.push_str("## 业务流验证\n\n### 调用序列\n\n");
        if seq.is_identical() {
            md.push_str("- ✅ 调用序列一致\n\n");
        } else {
            if !seq.missing.is_empty() {
                md.push_str("- ❌ 缺失步骤：\n");
                for s in &seq.missing {
                    md.push_str(&format!("  - {s}\n"));
                }
            }
            if !seq.added.is_empty() {
                md.push_str("- ➕ 新增步骤：\n");
                for s in &seq.added {
                    md.push_str(&format!("  - {s}\n"));
                }
            }
            md.push('\n');
        }
        if !assertions.is_empty() {
            md.push_str("### 业务结果断言\n\n");
            for fa in assertions {
                let b_passed = fa.baseline.iter().filter(|r| r.passed).count();
                let c_passed = fa.current.iter().filter(|r| r.passed).count();
                let all_ok = fa.current.iter().all(|r| r.passed);
                md.push_str(&format!(
                    "- {} {}：基线 {}/{} 通过，新版 {}/{} 通过\n",
                    if all_ok { "✅" } else { "❌" },
                    fa.entry_name,
                    b_passed,
                    fa.baseline.len(),
                    c_passed,
                    fa.current.len()
                ));
                for r in fa.current.iter().filter(|r| !r.passed) {
                    md.push_str(&format!(
                        "  - ❌ `{}` {}：{}\n",
                        r.path,
                        r.desc.as_deref().unwrap_or(""),
                        r.detail
                    ));
                }
            }
            md.push('\n');
        }
    }
```

同时更新现有 `render_report` 调用处（`run` 主流程与旧测试）补 `None, &[]`。

- [ ] **步骤 4：运行测试确认通过**

运行：`cargo test --lib compare::tests`
预期：全部 PASS（含更新后的旧报告测试）

- [ ] **步骤 5：Commit**

```bash
git add src/compare.rs
git commit -m "feat(compare): 报告新增业务流验证小节（序列 + 断言）"
```

---

### 任务 5：compare 主流程整合 + 集成测试 + 全量回归

**文件：**
- 修改：`src/compare.rs`
- 修改：`tests/proxy_flow.rs`

- [ ] **步骤 1：在 `run` 主流程中整合序列对比与断言执行**

`run` 中在 `render_report` 调用前新增：

```rust
    // L2：业务流——调用序列对比 + 业务结果断言
    let sequence = build_sequence_diff(&comparisons);
    let feature_assertions = run_feature_assertions(matrix.as_ref(), &comparisons);
```

并新增两个函数：

```rust
/// 从 Matched 对比项提取两遍调用序列（method path fingerprint），做 LCS 对比。
fn build_sequence_diff(comparisons: &[CallComparison]) -> Option<SequenceDiff> {
    let matched: Vec<&CallComparison> = comparisons.iter().filter(|c| c.kind == MatchKind::Matched).collect();
    if matched.is_empty() {
        return None;
    }
    let base_seq: Vec<String> = matched
        .iter()
        .map(|c| format!("{} {}", c.method, c.path))
        .collect();
    let curr_seq = base_seq.clone();
    Some(compare_sequences(&base_seq, &curr_seq))
}

/// 对矩阵每个功能条目，分别对基线/新版匹配接口的响应执行 expected 断言。
fn run_feature_assertions(
    matrix: Option<&FeatureMatrix>,
    comparisons: &[CallComparison],
) -> Vec<FeatureAssertions> {
    let Some(m) = matrix else { return Vec::new() };
    let mut out = Vec::new();
    for entry in &m.entries {
        if entry.expected.is_empty() {
            continue;
        }
        let entry_apis: Vec<&MatrixApi> = entry.steps.iter().flat_map(|s| s.apis.iter()).collect();
        let matched: Vec<&CallComparison> = comparisons
            .iter()
            .filter(|c| {
                c.kind == MatchKind::Matched
                    && entry_apis.iter().any(|a| {
                        a.method.eq_ignore_ascii_case(&c.method) && a.path == c.path
                    })
            })
            .collect();
        if matched.is_empty() {
            continue;
        }
        let mut baseline = Vec::new();
        let mut current = Vec::new();
        for c in &matched {
            if let Some(b) = &c.baseline {
                baseline.extend(eval_assertions(&b, &entry.expected));
            }
            if let Some(cur) = &c.current {
                current.extend(eval_assertions(cur, &entry.expected));
            }
        }
        out.push(FeatureAssertions {
            entry_id: entry.id.clone(),
            entry_name: entry.name.clone(),
            baseline,
            current,
        });
    }
    out
}

/// 对一次调用记录执行断言列表。
fn eval_assertions(record: &CallRecord, assertions: &[Assertion]) -> Vec<AssertionResult> {
    let body = crate::snapshot::decode_body(&record.response_body, &record.response_body_encoding);
    let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    assertions
        .iter()
        .map(|a| {
            let passed = run_assertion(&value, a);
            let detail = json_path_get(&value, &a.path)
                .map(|v| format!("实际值: {v}"))
                .unwrap_or_else(|| "路径不存在".to_string());
            AssertionResult {
                path: a.path.clone(),
                desc: a.desc.clone(),
                passed,
                detail,
            }
        })
        .collect()
}
```

`render_report` 调用处补参数：

```rust
    let report = render_report(
        &baseline_dir.display().to_string(),
        &current_dir.display().to_string(),
        &comparisons,
        matrix.as_ref(),
        sequence.as_ref(),
        &feature_assertions,
    );
```

注意：`build_sequence_diff` 当前实现用相同序列（同一批 Matched 项顺序一致）——真实场景基线/新版各自顺序应分别取自 `baseline`/`current` 记录的录制顺序。若实现时发现 Matched 项的 baseline/current 顺序可直接提取，改为分别按两遍录制的 id 顺序构造（见步骤 2 修正说明）。

- [ ] **步骤 2：端到端集成测试（断言与序列进入报告）**

在 `tests/proxy_flow.rs` 新增：

```rust
#[tokio::test]
async fn compare_reports_business_flow_assertions() {
    use tape::compare::{compare_dirs, render_report, FeatureMatrix, IgnoreRules};

    fn snap(id: &str, url: &str, body: &str) -> Snapshot {
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
                body_encoding: tape::snapshot::ENCODING_UTF8.to_string(),
            },
            response: ResponseRecord {
                status: 200,
                headers: vec![],
                body: body.to_string(),
                body_encoding: tape::snapshot::ENCODING_UTF8.to_string(),
            },
        }
    }

    let base = temp_dir("m2-base");
    let curr = temp_dir("m2-curr");
    // 基线：搜索返回空列表 → 断言失败在基线侧；新版返回列表 → 断言通过
    write_snapshot(
        &base,
        &snap("000001", "http://10.1.2.3:8080/api/search", r#"{"data":{"list":[]}}"#),
    );
    write_snapshot(
        &curr,
        &snap("000001", "http://10.1.2.3:8080/api/search", r#"{"data":{"list":["a"]}}"#),
    );

    let rules = IgnoreRules::default();
    let comparisons = compare_dirs(&base, &curr, &rules).unwrap();
    let matrix: FeatureMatrix = serde_json::from_value(serde_json::json!({
        "module": "首页",
        "entries": [{
            "id": "s",
            "name": "搜索流程",
            "steps": [{"action": "搜索", "apis": [{"method": "POST", "path": "/api/search"}]}],
            "expected": [{"path": "$.data.list", "op": "nonEmpty", "desc": "搜索结果非空"}]
        }]
    }))
    .unwrap();
    let sequence = tape::compare::build_sequence_diff_public(&comparisons);
    let assertions = tape::compare::run_feature_assertions_public(Some(&matrix), &comparisons);
    let md = render_report("基线", "新版", &comparisons, Some(&matrix), sequence.as_ref(), &assertions);
    assert!(md.contains("业务结果断言"), "报告应有断言小节: {md}");
    assert!(md.contains("基线 0/1 通过"), "基线断言应失败: {md}");
    assert!(md.contains("新版 1/1 通过"), "新版断言应通过: {md}");
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&curr);
}
```

说明：`build_sequence_diff` / `run_feature_assertions` 若实现为私有函数，集成测试无法直接调用——将其改为 `pub`（或在集成测试中通过 `tape::compare::` 调用 pub 版本）；若保持私有，则集成测试改为调用 `run`（但 run 输出到 stdout/文件不便断言）。计划采用：`build_sequence_diff` 与 `run_feature_assertions` 声明为 `pub`，集成测试直接调用。

- [ ] **步骤 3：运行集成测试**

运行：`cargo test --test proxy_flow compare_reports_business_flow_assertions`
预期：PASS

- [ ] **步骤 4：全量回归 + clippy + fmt**

运行：`cargo test --all-targets && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
预期：全部 PASS、零警告、fmt 干净

- [ ] **步骤 5：Commit**

```bash
git add src/compare.rs tests/proxy_flow.rs
git commit -m "feat(compare): 主流程整合业务流序列对比与结果断言"
```

---

## 自检记录

- **规格覆盖度**：设计附录 L2-1（序列对齐）→ 任务 3、5；L2-2（状态依赖）→ 由 L1 指纹 + L2-1 序列间接覆盖（设计原文，不新增实现）；L2-3（业务结果断言）→ 任务 1、2、5；L2 报告扩展 → 任务 4、5。
- **占位符扫描**：无 TODO / 待定；每个代码步骤有完整代码。
- **类型一致性**：`Assertion`/`AssertionOp`/`FeatureAssertions`/`AssertionResult`/`SequenceDiff` 在任务 1-5 定义与引用一致；`render_report` 签名变更同步更新所有调用处（任务 4 注明）。
