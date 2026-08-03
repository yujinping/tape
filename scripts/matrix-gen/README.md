# AI 矩阵生成组件（matrix-gen）

从 `tape export` 导出的 JSONL 生成「功能矩阵」草稿（含 `expected` 业务断言），
人工校正后直接作为 `tape compare --matrix` 的输入。

## 环境变量

| 变量 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- |
| `LLM_API_KEY` | 是 | - | DeepSeek / 通义 / OpenAI API Key |
| `LLM_BASE_URL` | 否 | `https://api.deepseek.com/v1` | OpenAI 兼容 API 地址 |
| `LLM_MODEL` | 否 | `deepseek-v4-flash` | 模型名 |

## 用法

```bash
# 1. 导出录制数据
tape export ./tape-api -o base.jsonl

# 2. 生成矩阵草稿
python3 scripts/matrix-gen/gen_matrix.py base.jsonl -o matrix.draft.json

# 3. 人工校正 matrix.draft.json（修正条目命名、步骤边界、expected 断言）

# 4. 新版录制后对比
tape compare ./tape-api-baseline ./tape-api-current --matrix matrix.draft.json -o report.md
```

## 输出结构

矩阵 JSON 与 `tape compare` 的 `--matrix` 格式一致：
`module → entries[id/name] → steps[action/apis] → expected[path/op/value/desc]`。

## 注意

- 单次处理上限默认 800 条请求（`--max-items`），超限请分段导出；
- 请求/响应体默认截断到 500 字符（`--body-preview`），提示词聚焦 URL/方法/结构；
- 录制数据会发送到配置的 LLM API，注意数据合规；
- LLM 生成的 `expected` 是草稿，需人工确认后再作为正式断言。

## 测试

```bash
python3 scripts/matrix-gen/test_matrix_gen.py
```
