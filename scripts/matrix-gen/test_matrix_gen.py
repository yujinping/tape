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
