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


if __name__ == "__main__":
    sys.exit(0)
