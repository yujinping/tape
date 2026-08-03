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


class TestPreviewAndRecords(unittest.TestCase):
    def test_preview_truncates_long_text(self):
        self.assertEqual(gm.preview_text("abc", 2), "ab…")
        self.assertEqual(gm.preview_text("ab", 5), "ab")

    def test_build_records_extracts_summary(self):
        item = {
            "id": "000001",
            "recorded_at": "2026-08-02T00:00:00Z",
            "request": {"method": "POST", "url": "http://h/api/login", "body": '{"user":"a"}'},
            "response": {"status": 200, "body": '{"token":"T"}'},
        }
        r = gm.build_records([item], 10)[0]
        self.assertEqual(r["method"], "POST")
        self.assertEqual(r["url"], "http://h/api/login")
        self.assertEqual(r["status"], 200)
        self.assertTrue(r["resp_body"].endswith("…"), "响应体应被截断")


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


class TestCallLlm(unittest.TestCase):
    def test_call_sends_openai_compatible_request(self):
        fake_response = json.dumps(
            {"choices": [{"message": {"content": '{"module":"首页","entries":[]}'}}]}
        ).encode("utf-8")

        captured = {}

        class FakeResp:
            def __enter__(self):
                return self

            def __exit__(self, *exc):
                return False

            def read(self):
                return fake_response

        def fake_urlopen(req, timeout):
            captured["url"] = req.full_url
            captured["auth"] = req.headers.get("Authorization")
            captured["body"] = json.loads(req.data.decode("utf-8"))
            return FakeResp()

        with mock.patch("urllib.request.urlopen", side_effect=fake_urlopen):
            content = gm.call_llm(
                "prompt", "KEY", "https://api.deepseek.com/v1", "deepseek-v4-flash"
            )

        self.assertEqual(content, '{"module":"首页","entries":[]}')
        self.assertIn("/chat/completions", captured["url"])
        self.assertEqual(captured["auth"], "Bearer KEY")
        self.assertEqual(captured["body"]["model"], "deepseek-v4-flash")
        self.assertEqual(captured["body"]["messages"][1]["content"], "prompt")

    def test_call_retries_once_on_http_error(self):
        import urllib.error

        calls = {"n": 0}

        class FakeResp:
            def __enter__(self):
                return self

            def __exit__(self, *exc):
                return False

            def read(self):
                return b'{"choices":[{"message":{"content":"{}"}}]}'

        def fake_urlopen(req, timeout):
            calls["n"] += 1
            if calls["n"] == 1:
                raise urllib.error.HTTPError(req.full_url, 429, "rate", None, None)
            return FakeResp()

        with mock.patch("urllib.request.urlopen", side_effect=fake_urlopen):
            content = gm.call_llm("p", "K", "https://x/v1", "m")
        self.assertEqual(calls["n"], 2, "失败应重试一次")
        self.assertEqual(content, "{}")


class TestExtractAndValidate(unittest.TestCase):
    def test_extract_json_strips_fence(self):
        text = '```json\n{"module":"首页"}\n```'
        self.assertEqual(gm.extract_json(text).strip(), '{"module":"首页"}')
        self.assertEqual(
            gm.extract_json('{"module":"首页"}').strip(), '{"module":"首页"}'
        )

    def test_validate_matrix_accepts_valid(self):
        obj = {
            "module": "首页",
            "entries": [
                {
                    "id": "s",
                    "name": "搜索",
                    "steps": [
                        {"action": "搜索", "apis": [{"method": "POST", "path": "/api/s"}]}
                    ],
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

        class FakeResp:
            def __enter__(self):
                return self

            def __exit__(self, *exc):
                return False

            def read(self):
                return json.dumps(
                    {"choices": [{"message": {"content": llm_result}}]}
                ).encode("utf-8")

        with mock.patch(
            "urllib.request.urlopen",
            return_value=FakeResp(),
        ):
            with mock.patch.dict(os.environ, {"LLM_API_KEY": "K"}, clear=False):
                rc = gm.main([inp, "-o", out])

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


if __name__ == "__main__":
    unittest.main()
