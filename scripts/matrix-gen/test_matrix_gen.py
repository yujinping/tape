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


if __name__ == "__main__":
    unittest.main()
