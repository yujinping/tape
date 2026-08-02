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
    pub fn from_snapshot(snap: &Snapshot) -> Self {
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
            parts.extend(collect_json_keys(&value, "$", rules));
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

    /// 判断字段路径是否命中规则：精确匹配、前缀匹配（`$.data` 命中 `$.data.token`）、
    /// 或 `[*]` 通配任意数字下标（`$.data.list[*].id` 命中 `$.data.list[0].id`）。
    pub fn matches(&self, path: &str) -> bool {
        self.paths.iter().any(|rule| {
            if path == rule || path.starts_with(&format!("{rule}.")) {
                return true;
            }
            if rule.contains("[*]") {
                // 先整体转义正则特殊字符（如 $ 锚点），再把 [*] 替换成匹配任意数字下标的模式
                let escaped = regex::escape(rule);
                let pattern = escaped.replace(r"\[\*\]", r"\[\d+\]");
                if let Ok(re) = regex::Regex::new(&pattern) {
                    return re.is_match(path);
                }
            }
            false
        })
    }
}

fn split_path_query(url: &str) -> (String, Vec<(String, String)>) {
    let path = rewrite::url_path(url);
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let query = after_scheme.split_once('?').map(|(_, q)| q).unwrap_or("");
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

/// 匹配类别：基线有新版无 / 新版有基线无 / 两边都有。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Missing,
    Added,
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
    pub status_diff: Option<(u16, u16)>,
    pub response_diffs: Vec<FieldDiff>,
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
    Ok(snaps.iter().map(CallRecord::from_snapshot).collect())
}

fn align_calls(
    baseline: &[CallRecord],
    current: &[CallRecord],
    rules: &IgnoreRules,
) -> Vec<CallComparison> {
    let n = baseline.len();
    let mut buckets: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (i, r) in baseline.iter().enumerate() {
        buckets
            .entry((r.method.clone(), r.path.clone()))
            .or_default()
            .push(i);
    }
    for (j, r) in current.iter().enumerate() {
        buckets
            .entry((r.method.clone(), r.path.clone()))
            .or_default()
            .push(n + j);
    }

    let mut out = Vec::new();
    for ((method, path), idxs) in buckets {
        let base_idx: Vec<usize> = idxs.iter().copied().filter(|&i| i < n).collect();
        let curr_idx: Vec<usize> = idxs
            .iter()
            .copied()
            .filter(|&i| i >= n)
            .map(|i| i - n)
            .collect();
        // 指纹分组：HashMap<fingerprint, (基线下标列表, 新版下标列表)>
        let mut groups: HashMap<String, (Vec<usize>, Vec<usize>)> = HashMap::new();
        for &i in &base_idx {
            groups
                .entry(baseline[i].fingerprint(rules))
                .or_default()
                .0
                .push(i);
        }
        for &j in &curr_idx {
            groups
                .entry(current[j].fingerprint(rules))
                .or_default()
                .1
                .push(j);
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

/// 递归对比两个 JSON 值：对象 key 增删为 Structure，标量变化为 Value；数组按索引对比。
pub fn diff_json(
    base: &Value,
    current: &Value,
    prefix: &str,
    rules: &IgnoreRules,
) -> Vec<FieldDiff> {
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

/// 对比两次调用的请求（query + body），返回人类可读差异描述；一致返回 None。
pub fn request_diff(base: &CallRecord, current: &CallRecord) -> Option<String> {
    let mut parts = Vec::new();
    if base.query != current.query {
        parts.push(format!(
            "query: {} → {}",
            format_query(&base.query),
            format_query(&current.query)
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

fn format_query(query: &[(String, String)]) -> String {
    query
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn summarize_body(body: &str) -> String {
    if body.len() > 120 {
        format!("{}…", &body[..120])
    } else {
        body.to_string()
    }
}

/// `tape compare` 主流程：完整实现由任务 7 填充。
pub fn run(
    _baseline: &Path,
    _current: &Path,
    _matrix: Option<&Path>,
    _ignore: Option<&Path>,
    _output: Option<&Path>,
) -> Result<()> {
    unimplemented!("tape compare 由 M1 任务 3-7 实现")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{ENCODING_UTF8, RequestRecord, ResponseRecord, Snapshot};

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
        let r = CallRecord::from_snapshot(&call(
            "000001",
            "http://10.1.2.3:8080/api/search?kw=电影&page=2",
            "",
        ));
        assert_eq!(r.path, "/api/search");
        assert_eq!(
            r.query,
            vec![
                ("kw".to_string(), "电影".to_string()),
                ("page".to_string(), "2".to_string()),
            ]
        );
    }

    #[test]
    fn fingerprint_differs_by_param_value_but_ignores_rule_paths() {
        let rules = IgnoreRules::from_paths(vec!["$.ts".to_string()]);
        let a = CallRecord::from_snapshot(&call(
            "000001",
            "http://10.1.2.3:8080/api/search?kw=电影",
            r#"{"kw":"电影","ts":1}"#,
        ));
        let b = CallRecord::from_snapshot(&call(
            "000002",
            "http://10.1.2.3:8080/api/search?kw=电视剧",
            r#"{"kw":"电视剧","ts":999}"#,
        ));
        assert_ne!(
            a.fingerprint(&rules),
            b.fingerprint(&rules),
            "不同参数指纹必须不同"
        );

        let c = CallRecord::from_snapshot(&call(
            "000003",
            "http://10.1.2.3:8080/api/search?kw=电影",
            r#"{"kw":"电影","ts":2}"#,
        ));
        assert_eq!(
            a.fingerprint(&rules),
            c.fingerprint(&rules),
            "动态字段 ts 被忽略后指纹应相同"
        );
    }

    #[test]
    fn align_groups_by_path_then_fingerprint_then_order() {
        let rules = IgnoreRules::default();
        // 基线：同 path 三个调用（kw=电影 / kw=电视剧 / kw=综艺）
        let base = vec![
            CallRecord::from_snapshot(&call(
                "000001",
                "http://10.1.2.3:8080/api/search?kw=电影",
                "",
            )),
            CallRecord::from_snapshot(&call(
                "000002",
                "http://10.1.2.3:8080/api/search?kw=电视剧",
                "",
            )),
            CallRecord::from_snapshot(&call(
                "000003",
                "http://10.1.2.3:8080/api/search?kw=综艺",
                "",
            )),
        ];
        // 新版：kw=电影 重复两次 + kw=电视剧（顺序不同）
        let curr = vec![
            CallRecord::from_snapshot(&call(
                "000001",
                "http://10.1.2.3:8080/api/search?kw=电视剧",
                "",
            )),
            CallRecord::from_snapshot(&call(
                "000002",
                "http://10.1.2.3:8080/api/search?kw=电影",
                "",
            )),
            CallRecord::from_snapshot(&call(
                "000003",
                "http://10.1.2.3:8080/api/search?kw=电影",
                "",
            )),
        ];
        let result = align_calls(&base, &curr, &rules);
        let missing = result
            .iter()
            .filter(|c| c.kind == MatchKind::Missing)
            .count();
        let added = result.iter().filter(|c| c.kind == MatchKind::Added).count();
        let matched = result
            .iter()
            .filter(|c| c.kind == MatchKind::Matched)
            .count();
        assert_eq!(missing, 1, "综艺 应缺失");
        assert_eq!(added, 1, "新版多出的电影调用应标新增");
        assert_eq!(matched, 2, "电影与电视剧各配对一条");
    }

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
        let rules = IgnoreRules::from_paths(vec![
            "$.data.token".to_string(),
            "$.data.list[*].id".to_string(),
        ]);
        let diffs = diff_json(&base, &curr, "$", &rules);
        assert!(
            diffs
                .iter()
                .any(|d| d.path == "$.data.title" && d.kind == DiffKind::Value),
            "title 值变化应检出: {diffs:?}"
        );
        assert!(
            diffs
                .iter()
                .any(|d| d.path == "$.data.list[0].name" && d.kind == DiffKind::Value),
            "list[0].name 值变化应检出: {diffs:?}"
        );
        assert!(
            diffs
                .iter()
                .any(|d| d.path == "$.data.extra" && d.kind == DiffKind::Structure),
            "extra 字段新增应检出: {diffs:?}"
        );
        assert!(
            !diffs.iter().any(|d| d.path.contains("token")),
            "token 应被忽略: {diffs:?}"
        );
        assert!(
            !diffs.iter().any(|d| d.path.contains("[0].id")),
            "list[*].id 应被忽略: {diffs:?}"
        );
    }

    #[test]
    fn request_diff_detects_param_and_body_changes() {
        let b = CallRecord::from_snapshot(&call(
            "000001",
            "http://10.1.2.3:8080/api/search?kw=电影",
            r#"{"kw":"电影"}"#,
        ));
        let c = CallRecord::from_snapshot(&call(
            "000002",
            "http://10.1.2.3:8080/api/search?kw=电视剧",
            r#"{"kw":"电视剧"}"#,
        ));
        let diff = request_diff(&b, &c);
        assert!(diff.is_some(), "query 变化应产生请求差异");
        let text = diff.unwrap();
        assert!(text.contains("kw=电影"), "应指出基线参数: {text}");
        assert!(text.contains("kw=电视剧"), "应指出新版参数: {text}");
    }
}
