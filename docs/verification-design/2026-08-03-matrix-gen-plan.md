# AI 矩阵生成组件（M3-A）实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 开发 `scripts/matrix-gen/gen_matrix.py`：读取 `tape export` 输出的 JSONL，调用 OpenAI 兼容 LLM API（默认 DeepSeek），生成功能矩阵草稿 JSON（含 `expected` 业务断言），供人工校正后直接给 `tape compare --matrix` 使用。

**架构：** 零依赖 Python 3 脚本（仅标准库 `argparse/json/os/sys/urllib`）：`load_jsonl` 读数据 → `build_records` 摘要 → `build_prompt` 组提示词 → `call_llm` 调用 API（环境变量配置、失败重试一次）→ `extract_json` + `validate_matrix` 校验 → 落盘。LLM 协议为 OpenAI 兼容 `/chat/completions`。

**技术栈：** Python 3（标准库，无第三方依赖）、unittest（测试）、OpenAI 兼容 Chat Completions API。

**工作仓库：** `/Users/yujinping/data/workspace/rust-projects/box-proxy`（tape），独立分支 `feat/matrix-gen`。

---

## 文件结构

**创建：**
- `scripts/matrix-gen/gen_matrix.py` —— 主脚本（全部逻辑）
- `scripts/matrix-gen/test_matrix_gen.py` —— unittest 测试
- `scripts/matrix-gen/README.md` —— 使用说明（环境变量、示例命令、人工校正衔接）
- `scripts/matrix-gen/examples/example.jsonl` —— 示例输入（3 条快照，覆盖登录/搜索/下单）

**依赖关系：** 任务 1（骨架+读入）→ 任务 2（摘要）→ 任务 3（提示词）→ 任务 4（LLM 调用）→ 任务 5（解析校验）→ 任务 6（main 流程+集成测试）→ 任务 7（示例与 README）。

---

### 任务 1：脚本骨架与 JSONL 读取

**文件：**
- 创建：`scripts/matrix-gen/gen_matrix.py`
- 创建：`scripts/matrix-gen/test_matrix_gen.py`

- [ ] **步骤 1：编写失败测试**

```python
import json
import os
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(__file__))
import gen_matrix as gm


class TestLoadJsonl(unittest.TestCase):
    def test_load_parses_each_line(self):
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
            f.write('{"id":"000001"}\n{"id":"000002"}\n\n')
            path = f.name
        items = gm.load_jsonl(path)
        os.unlink(path)
        self.assertEqual([i["id"] for i in items], ["000001", "000002"])

    def test_load_missing_file_raises(self):
        with self.assertRaises(FileNotFoundError):
            gm.load_jsonl("/nonexistent/x.jsonl")


if __name__ == "__main__":
    unittest.main()
```

- [ ] **步骤 2：运行测试确认失败**

运行：`python3 scripts/matrix-gen/test_matrix_gen.py`
预期：FAIL，报 `ModuleNotFoundError: No module named 'gen_matrix'`（脚本尚未创建）。

- [ ] **步骤 3：实现脚本骨架**

```python
#!/usr/bin/env python3
"""AI 矩阵生成组件：从 tape export 的 JSONL 生成功能矩阵草稿。

用法：
    tape export <录制目录> -o base.jsonl
    python3 scripts/matrix-gen/gen_matrix.py base.jsonl -o matrix.draft.json

环境变量：
    LLM_API_KEY  必填，DeepSeek / 通义 / OpenAI 的 API Key
    LLM_BASE_URL 默认 https://api.deepseek.com/v1
    LLM_MODEL    默认 deepseek-v4-flash
"""

import argparse
import json
import os
import sys

DEFAULT_BASE_URL = "https://api.deepseek.com/v1"
DEFAULT_MODEL = "deepseek-v4-flash"
DEFAULT_BODY_PREVIEW = 500
DEFAULT_MAX_ITEMS = 800


def parse_args():
    p = argparse.ArgumentParser(description="从 tape export JSONL 生成功能矩阵草稿")
    p.add_argument("input", help="tape export 输出的 JSONL 文件")
    p.add_argument("-o", "--output", default="matrix.draft.json", help="矩阵草稿输出路径")
    p.add_argument(
        "--max-items",
        type=int,
        default=DEFAULT_MAX_ITEMS,
        help=f"单次处理的最大请求条数（默认 {DEFAULT_MAX_ITEMS}）",
    )
    p.add_argument(
        "--body-preview",
        type=int,
        default=DEFAULT_BODY_PREVIEW,
        help=f"请求/响应体摘要长度（默认 {DEFAULT_BODY_PREVIEW} 字符）",
    )
    return p.parse_args()


def load_jsonl(path):
    """读取 JSONL 文件，逐行解析为对象列表（跳过空行）。"""
    items = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            items.append(json.loads(line))
    return items


if __name__ == "__main__":
    sys.exit(0)
```

- [ ] **步骤 4：运行测试确认通过**

运行：`python3 scripts/matrix-gen/test_matrix_gen.py`
预期：PASS（2 个测试）

- [ ] **步骤 5：Commit**

```bash
git add scripts/matrix-gen/gen_matrix.py scripts/matrix-gen/test_matrix_gen.py
git commit -m "feat(matrix-gen): 脚本骨架与 JSONL 读取"
```

---

### 任务 2：数据摘要（preview_text / build_records）

**文件：**
- 修改：`scripts/matrix-gen/gen_matrix.py`
- 修改：`scripts/matrix-gen/test_matrix_gen.py`

- [ ] **步骤 1：编写失败测试**

```python
class TestPreviewAndRecords(unittest.TestCase):
    def test_preview_truncates_long_text(self):
        self.assertEqual(gm.preview_text("abc", 2), "ab…")
        self.assertEqual(gm.preview_text("ab", 5), "ab")

    def test_build_records_extracts_summary(self):
        item = {
            "id": "000001",
            "recorded_at": "2026-08-02T00:00:00Z",
            "request": {"method": "POST", "url": "http://h/api/login", "body": "{\"user\":\"a\"}"},
            "response": {"status": 200, "body": "{\"token\":\"T\"}"},
        }
        r = gm.build_records([item], 10)[0]
        self.assertEqual(r["method"], "POST")
        self.assertEqual(r["url"], "http://h/api/login")
        self.assertEqual(r["status"], 200)
        self.assertTrue(r["resp_body"].endswith("…"), "响应体应被截断")
```

- [ ] **步骤 2：运行测试确认失败**

运行：`python3 scripts/matrix-gen/test_matrix_gen.py`
预期：FAIL，报 `preview_text` 未定义。

- [ ] **步骤 3：实现摘要函数**

```python
def preview_text(text, limit):
    """截断文本到 limit 字符（尾部加省略号）；None/非字符串安全处理。"""
    if text is None:
        return ""
    text = str(text)
    return text if len(text) <= limit else text[:limit] + "…"


def build_records(items, body_preview):
    """把 tape 快照转换为供提示词使用的摘要记录列表。"""
    records = []
    for it in items:
        req = it.get("request", {})
        resp = it.get("response", {})
        records.append(
            {
                "id": it.get("id"),
                "recorded_at": it.get("recorded_at"),
                "method": req.get("method"),
                "url": req.get("url"),
                "req_body": preview_text(req.get("body", ""), body_preview),
                "status": resp.get("status"),
                "resp_body": preview_text(resp.get("body", ""), body_preview),
            }
        )
    return records
```

- [ ] **步骤 4：运行测试确认通过**

运行：`python3 scripts/matrix-gen/test_matrix_gen.py`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add scripts/matrix-gen/
git commit -m "feat(matrix-gen): 数据摘要与记录构建"
```

---

### 任务 3：提示词构建（build_prompt）

**文件：**
- 修改：`scripts/matrix-gen/gen_matrix.py`
- 修改：`scripts/matrix-gen/test_matrix_gen.py`

- [ ] **步骤 1：编写失败测试**

```python
class TestBuildPrompt(unittest.TestCase):
    def test_prompt_contains_schema_and_data(self):
        records = [
            {
                "id": "000001",
                "recorded_at": "2026-08-02T00:00:00Z",
                "method": "POST",
                "url": "http://h/api/login",
                "req_body": "",
                "status": 200,
                "resp_body": "{}",
            }
        ]
        prompt = gm.build_prompt(records)
        self.assertIn("entries", prompt, "提示词应包含矩阵 schema")
        self.assertIn("expected", prompt, "提示词应包含 expected 断言说明")
        self.assertIn("/api/login", prompt, "提示词应包含录制数据")
```

- [ ] **步骤 2：运行测试确认失败**

运行：`python3 scripts/matrix-gen/test_matrix_gen.py`
预期：FAIL，报 `build_prompt` 未定义。

- [ ] **步骤 3：实现提示词构建**

```python
SYSTEM_PROMPT = """你是电视盒子 App 功能复刻验证助手。给定一段 HTTP 录制数据（旧版实跑），
生成「功能矩阵」JSON，用于证明新版是否完整复刻旧版功能。

矩阵结构：
{
  "module": "模块名（如 首页）",
  "entries": [
    {
      "id": "短横线小写标识，如 home-search",
      "name": "功能条目名（如 搜索流程）",
      "steps": [
        {
          "action": "按钮级操作描述（如 点击搜索框 / 输入关键词 / 点击搜索按钮）",
          "apis": [{"method": "POST", "path": "/api/search/query"}]
        }
      ],
      "expected": [
        {"path": "$.data.list", "op": "nonEmpty", "desc": "搜索结果非空"},
        {"path": "$.errorCode", "op": "eq", "value": 0, "desc": "无错误码"}
      ]
    }
  ]
}

要求：
1. 只输出 JSON，不要输出任何解释文字或 Markdown 围栏；
2. steps 切分到按钮级：每次用户可感知的操作（点击、输入、提交、返回）一个 step；
3. 同一路径不同参数的多次调用归入同一 step 的 apis（用 path 去 query，只保留一次）；
4. expected 从该接口响应推断业务结果断言，op 仅限 eq / exists / nonEmpty / gt / contains；
   无法可靠推断时省略 expected；
5. 接口 method 用大写。
"""


def build_prompt(records):
    """把摘要记录组装成 user 消息内容。"""
    lines = [json.dumps(r, ensure_ascii=False) for r in records]
    data_block = "\n".join(lines)
    return SYSTEM_PROMPT + "\n\n录制数据（JSONL，每条一行）：\n" + data_block
```

- [ ] **步骤 4：运行测试确认通过**

运行：`python3 scripts/matrix-gen/test_matrix_gen.py`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add scripts/matrix-gen/
git commit -m "feat(matrix-gen): 提示词构建（矩阵 schema + 录制数据）"
```

---

### 任务 4：LLM 调用（call_llm，OpenAI 兼容 + 重试）

**文件：**
- 修改：`scripts/matrix-gen/gen_matrix.py`
- 修改：`scripts/matrix-gen/test_matrix_gen.py`

- [ ] **步骤 1：编写失败测试（mock urlopen）**

```python
class TestCallLlm(unittest.TestCase):
    def test_call_sends_openai_compatible_request(self):
        fake_response = json.dumps(
            {"choices": [{"message": {"content": '{"module":"首页","entries":[]}'}}]}
        ).encode("utf-8")

        captured = {}

        class FakeResp:
            def read(self):
                return fake_response

        def fake_urlopen(req, timeout):
            captured["url"] = req.full_url
            captured["auth"] = req.headers.get("Authorization")
            captured["body"] = json.loads(req.data.decode("utf-8"))
            return FakeResp()

        with mock.patch("urllib.request.urlopen", side_effect=fake_urlopen):
            content = gm.call_llm("prompt", "KEY", "https://api.deepseek.com/v1", "deepseek-v4-flash")

        self.assertEqual(content, '{"module":"首页","entries":[]}')
        self.assertIn("/chat/completions", captured["url"])
        self.assertEqual(captured["auth"], "Bearer KEY")
        self.assertEqual(captured["body"]["model"], "deepseek-v4-flash")
        self.assertEqual(captured["body"]["messages"][1]["content"], "prompt")

    def test_call_retries_once_on_http_error(self):
        import urllib.error

        calls = {"n": 0}

        def fake_urlopen(req, timeout):
            calls["n"] += 1
            if calls["n"] == 1:
                raise urllib.error.HTTPError(req.full_url, 429, "rate", None, None)
            return mock.MagicMock(read=mock.MagicMock(return_value=b'{"choices":[{"message":{"content":"{}"}}]}'))

        with mock.patch("urllib.request.urlopen", side_effect=fake_urlopen):
            content = gm.call_llm("p", "K", "https://x/v1", "m")
        self.assertEqual(calls["n"], 2, "失败应重试一次")
        self.assertEqual(content, "{}")
```

- [ ] **步骤 2：运行测试确认失败**

运行：`python3 scripts/matrix-gen/test_matrix_gen.py`
预期：FAIL，报 `call_llm` 未定义。

- [ ] **步骤 3：实现 LLM 调用**

```python
import urllib.request
import urllib.error


def call_llm(prompt, api_key, base_url, model, temperature=0.2):
    """调用 OpenAI 兼容 chat/completions，失败重试一次；返回 message.content。"""
    url = base_url.rstrip("/") + "/chat/completions"
    payload = {
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": prompt},
        ],
        "temperature": temperature,
    }
    for attempt in range(2):
        try:
            req = urllib.request.Request(
                url,
                data=json.dumps(payload).encode("utf-8"),
                headers={
                    "Authorization": f"Bearer {api_key}",
                    "Content-Type": "application/json",
                },
                method="POST",
            )
            with urllib.request.urlopen(req, timeout=120) as resp:
                data = json.loads(resp.read().decode("utf-8"))
            return data["choices"][0]["message"]["content"]
        except (urllib.error.HTTPError, urllib.error.URLError, OSError, KeyError) as e:
            if attempt == 1:
                raise
    raise RuntimeError("LLM 调用失败")  # 理论不可达，防御
```

- [ ] **步骤 4：运行测试确认通过**

运行：`python3 scripts/matrix-gen/test_matrix_gen.py`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add scripts/matrix-gen/
git commit -m "feat(matrix-gen): LLM 调用（OpenAI 兼容 + 失败重试）"
```

---

### 任务 5：响应解析与矩阵校验

**文件：**
- 修改：`scripts/matrix-gen/gen_matrix.py`
- 修改：`scripts/matrix-gen/test_matrix_gen.py`

- [ ] **步骤 1：编写失败测试**

```python
class TestExtractAndValidate(unittest.TestCase):
    def test_extract_json_strips_fence(self):
        text = '```json\n{"module":"首页"}\n```'
        self.assertEqual(gm.extract_json(text).strip(), '{"module":"首页"}')
        self.assertEqual(gm.extract_json('{"module":"首页"}').strip(), '{"module":"首页"}')

    def test_validate_matrix_accepts_valid(self):
        obj = {
            "module": "首页",
            "entries": [
                {
                    "id": "s",
                    "name": "搜索",
                    "steps": [{"action": "搜索", "apis": [{"method": "POST", "path": "/api/s"}]}],
                    "expected": [{"path": "$.data", "op": "nonEmpty"}],
                }
            ],
        }
        ok, err = gm.validate_matrix(obj)
        self.assertTrue(ok, err)

    def test_validate_matrix_rejects_missing_path(self):
        ok, err = gm.validate_matrix({"module": "首页", "entries": [{"id": "s"}]})
        self.assertFalse(ok)
        self.assertIn("name", err)
```

- [ ] **步骤 2：运行测试确认失败**

运行：`python3 scripts/matrix-gen/test_matrix_gen.py`
预期：FAIL，报 `extract_json` 未定义。

- [ ] **步骤 3：实现解析与校验**

```python
def extract_json(text):
    """从 LLM 返回文本中提取 JSON：剥离 Markdown 围栏。"""
    text = text.strip()
    if text.startswith("```"):
        text = text.strip("`")
        if text.startswith("json"):
            text = text[4:]
    return text


def validate_matrix(obj):
    """校验矩阵结构，返回 (是否合法, 错误信息)。"""
    if not isinstance(obj, dict):
        return False, "顶层应为对象"
    if not isinstance(obj.get("module"), str):
        return False, "module 应为字符串"
    entries = obj.get("entries")
    if not isinstance(entries, list):
        return False, "entries 应为数组"
    for e in entries:
        if not isinstance(e, dict):
            return False, "条目应为对象"
        if not isinstance(e.get("id"), str) or not isinstance(e.get("name"), str):
            return False, "条目 id/name 应为字符串"
        steps = e.get("steps", [])
        if not isinstance(steps, list):
            return False, "steps 应为数组"
        for s in steps:
            apis = s.get("apis", [])
            if not isinstance(apis, list):
                return False, "apis 应为数组"
            for a in apis:
                if not isinstance(a.get("method"), str) or not isinstance(a.get("path"), str):
                    return False, "api method/path 应为字符串"
    return True, ""
```

- [ ] **步骤 4：运行测试确认通过**

运行：`python3 scripts/matrix-gen/test_matrix_gen.py`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add scripts/matrix-gen/
git commit -m "feat(matrix-gen): 响应解析与矩阵结构校验"
```

---

### 任务 6：main 主流程与集成测试（mock LLM）

**文件：**
- 修改：`scripts/matrix-gen/gen_matrix.py`
- 修改：`scripts/matrix-gen/test_matrix_gen.py`

- [ ] **步骤 1：编写失败测试（mock 完整流程）**

```python
class TestMain(unittest.TestCase):
    def test_main_writes_valid_matrix_draft(self):
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
            f.write(
                '{"id":"000001","recorded_at":"2026-08-02T00:00:00Z",'
                '"request":{"method":"POST","url":"http://h/api/login","body":""},'
                '"response":{"status":200,"body":"{\\"token\\":\\"T\\"}"}}\n'
            )
            inp = f.name
        out = inp + ".matrix.json"

        llm_result = (
            '{"module":"首页","entries":[{"id":"login","name":"登录",'
            '"steps":[{"action":"输入账号密码并登录","apis":[{"method":"POST","path":"/api/login"}]}],'
            '"expected":[{"path":"$.token","op":"exists","desc":"返回 token"}]}]}'
        )

        with mock.patch(
            "urllib.request.urlopen",
            return_value=mock.MagicMock(read=mock.MagicMock(return_value=json.dumps(
                {"choices": [{"message": {"content": llm_result}}]}
            ).encode("utf-8"))),
        ):
            with mock.patch.dict(os.environ, {"LLM_API_KEY": "K"}, clear=False):
                rc = gm.main(["dummy.jsonl", "-o", out])

        self.assertEqual(rc, 0)
        with open(out, encoding="utf-8") as f:
            matrix = json.load(f)
        self.assertEqual(matrix["entries"][0]["id"], "login")
        self.assertEqual(matrix["entries"][0]["expected"][0]["op"], "exists")
        os.unlink(inp)
        os.unlink(out)

    def test_main_exits_on_missing_api_key(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            rc = gm.main(["x.jsonl"])
        self.assertEqual(rc, 1)
```

注意：`main` 需要设计为可测试形态——参数列表 + 返回退出码（见步骤 3），当前 `main` 是 `parse_args()` 无参形式，需重构为 `main(argv=None)` 并返回 `int`。

- [ ] **步骤 2：运行测试确认失败**

运行：`python3 scripts/matrix-gen/test_matrix_gen.py`
预期：FAIL，报 `main` 参数不匹配或行为不符。

- [ ] **步骤 3：重构并实现 main 主流程**

```python
def main(argv=None):
    """主流程：读数据 → 摘要 → 提示词 → LLM → 校验 → 落盘。返回退出码。"""
    args = parse_args(argv)
    api_key = os.environ.get("LLM_API_KEY")
    if not api_key:
        print("错误：请设置 LLM_API_KEY 环境变量", file=sys.stderr)
        return 1
    base_url = os.environ.get("LLM_BASE_URL", DEFAULT_BASE_URL)
    model = os.environ.get("LLM_MODEL", DEFAULT_MODEL)

    items = load_jsonl(args.input)
    if len(items) > args.max_items:
        print(
            f"错误：数据 {len(items)} 条超过单次上限 {args.max_items}，请分段导出",
            file=sys.stderr,
        )
        return 1
    if not items:
        print("错误：输入文件为空", file=sys.stderr)
        return 1

    records = build_records(items, args.body_preview)
    prompt = build_prompt(records)
    try:
        content = call_llm(prompt, api_key, base_url, model)
    except Exception as e:  # noqa: BLE001
        print(f"错误：LLM 调用失败：{e}", file=sys.stderr)
        return 1

    try:
        obj = json.loads(extract_json(content))
    except json.JSONDecodeError as e:
        print(f"错误：LLM 返回不是合法 JSON：{e}", file=sys.stderr)
        return 1
    ok, err = validate_matrix(obj)
    if not ok:
        print(f"错误：矩阵校验失败：{err}", file=sys.stderr)
        return 1

    with open(args.output, "w", encoding="utf-8") as f:
        json.dump(obj, f, ensure_ascii=False, indent=2)
    n_entries = len(obj["entries"])
    n_apis = sum(
        len(s.get("apis", [])) for e in obj["entries"] for s in e.get("steps", [])
    )
    print(f"矩阵草稿已生成：{n_entries} 个功能条目、{n_apis} 个接口 -> {args.output}")
    print("请人工校正后用于 tape compare --matrix")
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

`parse_args` 改为接受可选 `argv`：

```python
def parse_args(argv=None):
    p = argparse.ArgumentParser(description="从 tape export JSONL 生成功能矩阵草稿")
    # ... 同上 ...
    return p.parse_args(argv)
```

- [ ] **步骤 4：运行测试确认通过**

运行：`python3 scripts/matrix-gen/test_matrix_gen.py`
预期：PASS（全部测试）

- [ ] **步骤 5：Commit**

```bash
git add scripts/matrix-gen/
git commit -m "feat(matrix-gen): main 主流程（LLM 调用/校验/落盘）与集成测试"
```

---

### 任务 7：示例数据与 README

**文件：**
- 创建：`scripts/matrix-gen/examples/example.jsonl`
- 创建：`scripts/matrix-gen/README.md`

- [ ] **步骤 1：创建示例 JSONL（3 条快照：登录/搜索/下单）**

`examples/example.jsonl` 内容（每行一条，含完整快照结构）：

```jsonl
{"id":"000001","origin":"http://10.1.2.3:8080","recorded_at":"2026-08-02T00:00:00Z","duration_ms":5,"request":{"method":"POST","url":"http://10.1.2.3:8080/api/login","headers":[],"body":"{\"user\":\"demo\"}","body_encoding":"utf8"},"response":{"status":200,"headers":[],"body":"{\"token\":\"T123\",\"user\":{\"name\":\"demo\"}}","body_encoding":"utf8"}}
{"id":"000002","origin":"http://10.1.2.3:8080","recorded_at":"2026-08-02T00:00:01Z","duration_ms":6,"request":{"method":"POST","url":"http://10.1.2.3:8080/api/search/query?kw=电影","headers":[],"body":"{\"kw\":\"电影\"}","body_encoding":"utf8"},"response":{"status":200,"headers":[],"body":"{\"data\":{\"list\":[{\"id\":1,\"title\":\"影\"}]},\"errorCode\":0}","body_encoding":"utf8"}}
{"id":"000003","origin":"http://10.1.2.3:8080","recorded_at":"2026-08-02T00:00:02Z","duration_ms":8,"request":{"method":"POST","url":"http://10.1.2.3:8080/api/order","headers":[],"body":"{\"videoId\":1}","body_encoding":"utf8"},"response":{"status":200,"headers":[],"body":"{\"orderId\":\"O1\",\"errorCode\":0}","body_encoding":"utf8"}}
```

- [ ] **步骤 2：创建 README.md**

`README.md` 内容（中文，含：用途、环境变量表、示例命令、人工校正衔接、与 tape compare 的关系、失败排查）：

```markdown
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
```

- [ ] **步骤 3：验证示例可被脚本读取**

运行：`python3 -c "import json,sys; sys.path.insert(0,'scripts/matrix-gen'); import gen_matrix; print(len(gen_matrix.load_jsonl('scripts/matrix-gen/examples/example.jsonl')))"`
预期：输出 `3`

- [ ] **步骤 4：全量测试 + 手动冒烟（可选真实 API）**

运行：`python3 scripts/matrix-gen/test_matrix_gen.py`
预期：全部 PASS

可选（有 LLM_API_KEY 时）：
`LLM_API_KEY=... python3 scripts/matrix-gen/gen_matrix.py scripts/matrix-gen/examples/example.jsonl -o /tmp/matrix.draft.json`
预期：输出「矩阵草稿已生成：N 个功能条目、M 个接口」，人工检查 JSON 结构。

- [ ] **步骤 5：Commit**

```bash
git add scripts/matrix-gen/
git commit -m "feat(matrix-gen): 示例数据与使用文档"
```

---

## 自检记录

- **规格覆盖度**：设计要点全部覆盖——JSONL 输入（任务 1）、摘要/分片上限（任务 2、6）、提示词 schema（任务 3）、OpenAI 兼容 + 重试 + 环境变量（任务 4）、严格 JSON 校验（任务 5）、人工校正衔接与 compare 集成（任务 6、7）。
- **占位符扫描**：无 TODO / 待定；每步有完整代码。
- **类型一致性**：`load_jsonl` / `preview_text` / `build_records` / `build_prompt` / `call_llm` / `extract_json` / `validate_matrix` / `main(argv)->int` 定义与调用一致；`parse_args(argv=None)` 支持测试注入。
