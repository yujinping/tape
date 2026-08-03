# 功能复刻验证使用指南

> 适用于「新版（Kotlin + MVVM）完整复刻旧版（Java + MVP）功能」的验证场景。
> 方案设计见 [2026-08-02-feature-replication-design.md](./2026-08-02-feature-replication-design.md)。

## 验证能证明什么、不能证明什么

本工具链自动化验证**接口行为层（L1）**与**业务流层（L2）**：

- L1 接口契约：两遍录制请求/响应结构、字段、状态码是否一致（含忽略规则过滤动态值）；
- L2 业务流：调用序列是否一致（LCS 对比）、矩阵定义的业务结果断言是否成立。

**不能证明**：UI 呈现、页面跳转、客户端状态、本地缓存等不体现在 HTTP 上的行为——这部分仍需人工功能测试或代码级测试配合。

## 完整链路（7 步）

```text
旧版录制 → 导出 → AI 生成矩阵草稿 → 人工校正 → 新版录制 → 对比 → 报告解读
```

### 1. 旧版实跑录制（有网环境）

```bash
tape record -d ./tape-api-baseline --name 基线-旧版v2.1
```

在旧版 App 上按既定路径完整操作一遍业务（测试同事实跑）。**注意**：两遍录制必须同一台盒子、同一网络环境。

### 2. 导出录制数据

```bash
tape export ./tape-api-baseline -o baseline.jsonl
```

JSONL 每行一条完整快照，是 AI 矩阵生成的输入。

### 3. AI 生成矩阵草稿

```bash
export LLM_API_KEY=sk-xxx              # 必填
export LLM_MODEL=deepseek-v4-flash     # 可选，默认 deepseek-v4-flash
export LLM_SSL_NO_VERIFY=1             # 公司网关插自签证书导致 CERTIFICATE_VERIFY_FAILED 时
python3 scripts/matrix-gen/gen_matrix.py baseline.jsonl -o matrix.draft.json
```

生成的草稿包含：模块 → 功能条目 → 按钮级步骤 → 接口集 + `expected` 业务断言。

### 4. 人工校正矩阵

编辑 `matrix.draft.json`（校正后建议另存为 `matrix.json` 并纳入版本管理）：

- 修正功能条目命名与步骤边界；
- 把录制输入的具体值（如 `"demo"`、`"电影"`）改成通用断言（如 `exists`）；
- 确认 `expected` 断言确实代表业务结果。

矩阵格式示例：

```json
{
  "module": "首页",
  "entries": [
    {
      "id": "home-search",
      "name": "搜索流程",
      "steps": [
        { "action": "点击搜索框", "apis": [{ "method": "POST", "path": "/api/search/query" }] }
      ],
      "expected": [
        { "path": "$.data.list", "op": "nonEmpty", "desc": "搜索结果非空" },
        { "path": "$.errorCode", "op": "eq", "value": 0, "desc": "无错误码" }
      ]
    }
  ]
}
```

`expected` 操作符：`eq` / `exists` / `nonEmpty` / `gt` / `contains`；路径为 JSONPath（如 `$.data.list[0].id`）。

### 5. 新版实跑录制

```bash
tape record -d ./tape-api-current --name 新版v3.0
```

同一盒子上，新版按同一路径操作一遍（操作顺序不要求与旧版完全一致，按接口对齐）。

### 6. 对比

```bash
tape compare ./tape-api-baseline ./tape-api-current \
  --matrix matrix.json \
  --ignore ignore.json \
  -o report.md
```

`ignore.json` 为动态字段忽略规则（字段路径数组）：

```json
["$.data.token", "$.data.list[*].id", "$.ts"]
```

### 7. 报告解读

`report.md` 包含三部分：

1. **汇总**：一致 / 变更 / 缺失 / 新增 数量；
2. **业务流验证**：
   - 调用序列：两遍录制配对接口的顺序差异（缺失/新增步骤）——业务流少一步会在这里点名；
   - 业务结果断言：逐功能条目「基线 X/N 通过，新版 Y/N 通过」，新版不通过直接标 ❌；
3. **按功能条目**：✅ / ❌ / ⚠️（未捕获到接口调用）+ 接口差异详情（结构 diff / 值 diff / 状态码 / 请求差异）。

## 环境变量速查

| 变量 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- |
| `LLM_API_KEY` | 是 | - | DeepSeek / 通义 / OpenAI API Key |
| `LLM_BASE_URL` | 否 | `https://api.deepseek.com/v1` | OpenAI 兼容 API 地址 |
| `LLM_MODEL` | 否 | `deepseek-v4-flash` | 模型名 |
| `LLM_SSL_NO_VERIFY` | 否 | 关闭 | `1` 时跳过证书校验（公司网关/代理场景） |

## 边界与注意事项

- 录制数据会发送到配置的 LLM API，注意数据合规；
- 单次矩阵生成默认上限 800 条请求（`--max-items`），超限请分段导出；
- 未覆盖的功能条目在报告中显式列出，**不允许默认声称完整**；
- 动态字段差异先补 `ignore.json` 再判结果，避免假差异淹没报告。

## 相关文档

- [方案设计（含 L2 附录）](./2026-08-02-feature-replication-design.md)
- [M1 实现计划（L1 工具）](./2026-08-02-m1-verification-tools-plan.md)
- [M2 实现计划（L2 业务流）](./2026-08-03-m2-business-flow-plan.md)
- [M3-A 实现计划（AI 矩阵生成）](./2026-08-03-matrix-gen-plan.md)
- [matrix-gen 组件说明](../../scripts/matrix-gen/README.md)
