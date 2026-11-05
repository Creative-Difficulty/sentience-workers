#!/usr/bin/env python3
"""Deterministic mock of an OpenAI-compatible chat completions API.

For local development and the end-to-end test only: it lets fact-extractor and
topic-sorter run without an API key, network access or token costs.

Endpoints
  POST /v1/chat/completions   (also /chat/completions)
      response_format.json_schema.name == "extracted_records"
          -> {"records": [...]} derived from keywords in the user message
      response_format.json_schema.name == "topic_groups"
          -> {"groups": [...]} grouping the "[<id>]: <text>" lines of the
             user message by keyword; topic labels start with "mock "
      anything else -> "{}"
  GET /health   -> 200 "ok"
  GET /stats    -> request counters as JSON

Every record/topic produced here is recognisable: fact reasoning starts with
"[mock-llm]" and topic names start with "mock ". Stdlib only.
"""

import json
import os
import re
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MAX_BODY = 8 * 1024 * 1024

# keyword (lowercase substring) -> fact-extractor record
FACT_RULES = [
    ("vienna", {"kind": "fact", "type": "location", "value": "Vienna", "confidence": 0.95,
                "level": None, "is_current": True}),
    ("rust", {"kind": "skill", "type": "programming_language", "value": "Rust", "confidence": 0.9,
              "level": 6, "is_current": True}),
    ("marathon", {"kind": "activity", "type": "race_completed", "value": "marathon",
                  "confidence": 0.9, "level": None, "is_current": False}),
]

# keyword -> topic label (first match wins)
TOPIC_RULES = [
    ("borrow checker", "mock borrow checker"),
    ("ramen", "mock ramen"),
    ("lunch", "mock ramen"),
    ("deploy", "mock deploy failure"),
    ("migration", "mock deploy failure"),
    ("marathon", "mock running"),
]

ID_LINE = re.compile(r"^\[(\d+)\]:\s?(.*)$")

_lock = threading.Lock()
STATS = {"requests": 0, "extracted_records": 0, "topic_groups": 0, "other": 0, "errors": 0}


def _bump(key):
    with _lock:
        STATS[key] += 1


def extract_records(user_text):
    text = user_text.lower()
    records = []
    for keyword, rec in FACT_RULES:
        if keyword in text:
            r = dict(rec)
            r["reasoning"] = f"[mock-llm] message mentions '{keyword}'."
            r["started_at"] = None
            r["ended_at"] = None
            records.append(r)
    return {"records": records}


def topic_groups(user_text):
    groups = {}
    for line in user_text.splitlines():
        m = ID_LINE.match(line.strip())
        if not m:
            continue
        msg_id, content = int(m.group(1)), m.group(2).lower()
        for keyword, label in TOPIC_RULES:
            if keyword in content:
                groups.setdefault(label, []).append(msg_id)
                break
    # The real prompt forbids singleton groups; mirror that.
    return {"groups": [{"topic": t, "message_ids": ids} for t, ids in groups.items() if len(ids) >= 2]}


def last_user_text(messages):
    for msg in reversed(messages or []):
        if msg.get("role") == "user":
            content = msg.get("content")
            if isinstance(content, str):
                return content
            if isinstance(content, list):  # content parts
                return "\n".join(p.get("text", "") for p in content if isinstance(p, dict))
    return ""


def completion(model, content):
    return {
        "id": f"chatcmpl-mock-{int(time.time() * 1000)}",
        "object": "chat.completion",
        "created": int(time.time()),
        "model": model or "mock",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop",
            "logprobs": None,
        }],
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0},
    }


class Handler(BaseHTTPRequestHandler):
    server_version = "mock-llm/1"

    def log_message(self, fmt, *args):
        if os.environ.get("MOCK_LLM_QUIET") != "1":
            sys.stderr.write("mock-llm: " + (fmt % args) + "\n")

    def _send(self, status, payload, ctype="application/json"):
        body = payload if isinstance(payload, bytes) else json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            return self._send(200, b"ok\n", "text/plain")
        if self.path == "/stats":
            with _lock:
                return self._send(200, dict(STATS))
        return self._send(404, {"error": {"message": "not found"}})

    def do_POST(self):
        if self.path.rstrip("/") not in ("/v1/chat/completions", "/chat/completions"):
            return self._send(404, {"error": {"message": "not found"}})
        _bump("requests")
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if length <= 0 or length > MAX_BODY:
                raise ValueError(f"bad Content-Length {length}")
            req = json.loads(self.rfile.read(length))
            fmt = (req.get("response_format") or {}).get("json_schema") or {}
            name = fmt.get("name")
            user_text = last_user_text(req.get("messages"))
            if name == "extracted_records":
                _bump("extracted_records")
                content = extract_records(user_text)
            elif name == "topic_groups":
                _bump("topic_groups")
                content = topic_groups(user_text)
            else:
                _bump("other")
                content = {}
            return self._send(200, completion(req.get("model"), json.dumps(content)))
        except Exception as e:  # noqa: BLE001 - report any malformed request as 400
            _bump("errors")
            return self._send(400, {"error": {"message": str(e), "type": "invalid_request_error"}})


def main():
    port = int(os.environ.get("PORT", "8080"))
    httpd = ThreadingHTTPServer(("0.0.0.0", port), Handler)
    sys.stderr.write(f"mock-llm: listening on :{port}\n")
    httpd.serve_forever()


if __name__ == "__main__":
    main()
