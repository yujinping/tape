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
import ssl
import sys
import urllib.error
import urllib.request

DEFAULT_BASE_URL = "https://api.deepseek.com/v1"
DEFAULT_MODEL = "deepseek-v4-flash"
DEFAULT_BODY_PREVIEW = 500
DEFAULT_MAX_ITEMS = 800


def parse_args(argv=None):
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
    return p.parse_args(argv)


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
            kwargs = {"timeout": 120}
            # 公司网关/代理插自签证书导致证书校验失败时，可设 LLM_SSL_NO_VERIFY=1 跳过
            # （仅限受信任网络调试，与 tape 的 TAPE_INSECURE_TLS 同理）
            if ssl_skip_verify():
                kwargs["context"] = ssl._create_unverified_context()
            with urllib.request.urlopen(req, **kwargs) as resp:
                data = json.loads(resp.read().decode("utf-8"))
            return data["choices"][0]["message"]["content"]
        except (urllib.error.HTTPError, urllib.error.URLError, OSError, KeyError) as e:
            if attempt == 1:
                raise
    raise RuntimeError("LLM 调用失败")  # 理论不可达，防御


def ssl_skip_verify():
    """LLM_SSL_NO_VERIFY=1/true/yes/on 时跳过 HTTPS 证书校验。"""
    return os.environ.get("LLM_SSL_NO_VERIFY", "").strip().lower() in (
        "1",
        "true",
        "yes",
        "on",
    )


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
