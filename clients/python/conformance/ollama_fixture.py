# SPDX-License-Identifier: Apache-2.0
"""Controlled Ollama protocol fixture; run only on an isolated test network.

No model runs. /calls records requests so conformance can verify routing.
"""

from __future__ import annotations

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Lock
from typing import Any

CALLS: list[dict[str, Any]] = []
LOCK = Lock()
MODELS = [
    "baseline-fast",
    "baseline-capable",
    "selected-fast",
    "selected-capable",
    "explicit",
    "embed",
]


class Handler(BaseHTTPRequestHandler):
    def log_message(self, format: str, *args: Any) -> None:
        pass

    def respond(self, body: Any, status: int = 200) -> None:
        raw = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self) -> None:
        if self.path == "/api/tags":
            self.respond({"models": [{"name": model, "model": model} for model in MODELS]})
        elif self.path == "/calls":
            with LOCK:
                calls = list(CALLS)
            self.respond(calls)
        else:
            self.respond({"error": "unknown fixture route"}, 404)

    def do_POST(self) -> None:
        size = int(self.headers.get("Content-Length", "0"))
        if not 0 < size <= 1024 * 1024:
            self.respond({"error": "invalid fixture request size"}, 400)
            return
        body = json.loads(self.rfile.read(size))
        with LOCK:
            CALLS.append({"path": self.path, **body})
        if self.path == "/api/chat":
            self.respond(
                {
                    "model": body["model"],
                    "done": True,
                    "done_reason": "stop",
                    "message": {"role": "assistant", "content": '["journey"]'},
                    "prompt_eval_count": 9,
                    "eval_count": 3,
                }
            )
        elif self.path == "/api/embed":
            self.respond(
                {
                    "model": body["model"],
                    "embeddings": [[1.0, float(i + 1), 0.5] for i, _ in enumerate(body["input"])],
                    "prompt_eval_count": 4,
                }
            )
        else:
            self.respond({"error": "unknown fixture route"}, 404)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bind", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=11434)
    args = parser.parse_args()
    ThreadingHTTPServer((args.bind, args.port), Handler).serve_forever()
