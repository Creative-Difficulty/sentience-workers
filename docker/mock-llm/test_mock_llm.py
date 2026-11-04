"""Unit tests for the mock LLM: python3 -m unittest discover -s docker/mock-llm"""

import json
import threading
import unittest
import urllib.request
from http.server import ThreadingHTTPServer

import mock_llm


class MockLLMTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.httpd = ThreadingHTTPServer(("127.0.0.1", 0), mock_llm.Handler)
        cls.base = f"http://127.0.0.1:{cls.httpd.server_address[1]}"
        threading.Thread(target=cls.httpd.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.httpd.shutdown()

    def chat(self, schema_name, user):
        body = json.dumps({
            "model": "mock",
            "messages": [{"role": "system", "content": "sys"}, {"role": "user", "content": user}],
            "response_format": {"type": "json_schema", "json_schema": {"name": schema_name, "schema": {}}},
        }).encode()
        req = urllib.request.Request(self.base + "/v1/chat/completions", data=body,
                                     headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req) as r:
            resp = json.load(r)
        self.assertEqual(resp["object"], "chat.completion")
        return json.loads(resp["choices"][0]["message"]["content"])

    def test_extracted_records_match_schema(self):
        out = self.chat("extracted_records", "I live in Vienna and write Rust")
        kinds = sorted(r["kind"] for r in out["records"])
        self.assertEqual(kinds, ["fact", "skill"])
        required = {"kind", "type", "value", "confidence", "reasoning",
                    "started_at", "ended_at", "level", "is_current"}
        for r in out["records"]:
            self.assertEqual(set(r), required)
            self.assertTrue(0 <= r["confidence"] <= 1)
            self.assertTrue(r["reasoning"].startswith("[mock-llm]"))

    def test_extracted_records_empty(self):
        self.assertEqual(self.chat("extracted_records", "hello"), {"records": []})

    def test_topic_groups_use_given_ids_and_skip_singletons(self):
        user = ("New messages to classify:\n[11]: ramen near the office?\n[12]: deploy failed\n"
                "[13]: ichiran ramen is good\n[14]: unrelated\n")
        out = self.chat("topic_groups", user)
        self.assertEqual(out, {"groups": [{"topic": "mock ramen", "message_ids": [11, 13]}]})

    def test_unknown_schema_and_bad_request(self):
        self.assertEqual(self.chat("something_else", "x"), {})
        req = urllib.request.Request(self.base + "/v1/chat/completions", data=b"not json")
        with self.assertRaises(urllib.error.HTTPError) as cm:
            urllib.request.urlopen(req)
        self.assertEqual(cm.exception.code, 400)

    def test_health(self):
        with urllib.request.urlopen(self.base + "/health") as r:
            self.assertEqual(r.status, 200)


if __name__ == "__main__":
    unittest.main()
