# SPDX-License-Identifier: Apache-2.0
"""Counting provider stub for COMPAT-T4 (standard library only).

Answers the Ollama endpoints (``/api/tags``, ``/api/chat``, ``/api/embed``) and
the OpenAI-compatible ones (``/v1/models``, ``/v1/chat/completions``,
``/v1/embeddings``) with small deterministic bodies, and records every arrival
as one JSON line: time, client address, method, path and request size. It never
records request bodies. ``GET /__counts`` returns the per-client tally and is
not itself recorded.

Run: ``python count_stub.py --port 11434 --log /data/arrivals.jsonl``.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

MODEL = "stub-model:1"
DIM = 8
_lock = threading.Lock()


def embed(text: str) -> list[float]:
    digest = hashlib.sha256(text.encode("utf-8")).digest()
    return [round((b - 128) / 128.0, 6) for b in digest[:DIM]]


def respond(method: str, path: str, body: Any) -> tuple[int, Any]:
    """The stub's response for one request (pure, unit-tested)."""
    path = path.split("?", 1)[0]
    if method == "GET" and path == "/api/tags":
        return 200, {"models": [{"name": MODEL, "model": MODEL, "size": 1, "digest": "0" * 64}]}
    if method == "POST" and path == "/api/chat":
        return 200, {
            "model": (body or {}).get("model", MODEL),
            "created_at": "2026-01-01T00:00:00Z",
            "message": {"role": "assistant", "content": "stub answer"},
            "done": True,
            "done_reason": "stop",
            "prompt_eval_count": 10,
            "eval_count": 2,
        }
    if method == "POST" and path == "/api/embed":
        inputs = (body or {}).get("input", [])
        inputs = [inputs] if isinstance(inputs, str) else inputs
        return 200, {"model": (body or {}).get("model", MODEL), "embeddings": [embed(t) for t in inputs]}
    if method == "GET" and path == "/v1/models":
        return 200, {"object": "list", "data": [{"id": MODEL, "object": "model"}]}
    if method == "POST" and path == "/v1/chat/completions":
        return 200, {
            "id": "stub-1",
            "object": "chat.completion",
            "model": (body or {}).get("model", MODEL),
            "choices": [{"index": 0, "finish_reason": "stop", "message": {"role": "assistant", "content": "stub answer"}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12},
        }
    if method == "POST" and path == "/v1/embeddings":
        inputs = (body or {}).get("input", [])
        inputs = [inputs] if isinstance(inputs, str) else inputs
        return 200, {
            "object": "list",
            "model": (body or {}).get("model", MODEL),
            "data": [{"object": "embedding", "index": i, "embedding": embed(t)} for i, t in enumerate(inputs)],
            "usage": {"prompt_tokens": len(inputs), "total_tokens": len(inputs)},
        }
    return 404, {"error": "not found"}


def tally(lines: list[str]) -> dict[str, dict[str, int]]:
    out: dict[str, dict[str, int]] = {}
    for line in lines:
        if not line.strip():
            continue
        rec = json.loads(line)
        per = out.setdefault(rec["client"], {})
        key = f"{rec['method']} {rec['path']}"
        per[key] = per.get(key, 0) + 1
    return out


def make_handler(log_path: str) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        def _handle(self, method: str) -> None:
            length = int(self.headers.get("Content-Length") or 0)
            raw = self.rfile.read(length) if length else b""
            if self.path == "/__counts":
                with _lock, open(log_path, encoding="utf-8") as fh:
                    payload = tally(fh.readlines())
                self._send(200, payload)
                return
            try:
                body = json.loads(raw) if raw else None
            except ValueError:
                body = None
            rec = {
                "t": time.time(),
                "client": self.client_address[0],
                "method": method,
                "path": self.path.split("?", 1)[0],
                "bytes": len(raw),
            }
            with _lock, open(log_path, "a", encoding="utf-8") as fh:
                fh.write(json.dumps(rec) + "\n")
            status, payload = respond(method, self.path, body)
            self._send(status, payload)

        def _send(self, status: int, payload: Any) -> None:
            data = json.dumps(payload).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self) -> None:  # noqa: N802
            self._handle("GET")

        def do_POST(self) -> None:  # noqa: N802
            self._handle("POST")

        def log_message(self, *_args: Any) -> None:
            return

    return Handler


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=11434)
    ap.add_argument("--log", required=True)
    args = ap.parse_args()
    open(args.log, "a", encoding="utf-8").close()
    ThreadingHTTPServer(("0.0.0.0", args.port), make_handler(args.log)).serve_forever()


if __name__ == "__main__":
    main()
