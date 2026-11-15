# SPDX-License-Identifier: AGPL-3.0-or-later
"""为同机插件提供伤害引擎的 JSON HTTP 接口。"""

import argparse
import hmac
import json
import os
import subprocess
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

ENGINE = Path(__file__).resolve().parent / "target/release/warframe-damage-engine"
SOURCE = "https://github.com/skyti1437/warframe-damage-engine"
ROUTES = {"/api/panel", "/api/simulate", "/api/riven", "/api/candidates"}
MAX_REQUEST_BYTES = 1024 * 1024


class Handler(BaseHTTPRequestHandler):
    timeout = 10

    def reply(self, status, payload):
        raw = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(raw)))
        self.send_header("Link", f'<{SOURCE}>; rel="source"')
        self.end_headers()
        self.wfile.write(raw)

    def authorized(self):
        token = os.environ.get("WARFRAME_ENGINE_TOKEN")
        header = self.headers.get("Authorization", "")
        return not token or hmac.compare_digest(header.encode(), f"Bearer {token}".encode())

    def calculate(self, body):
        request = json.dumps({"path": self.path, "body": body}, allow_nan=False)
        try:
            proc = subprocess.run(
                [ENGINE],
                input=request + "\n",
                capture_output=True,
                text=True,
                timeout=30,
                check=True,
            )
            payload = json.loads(proc.stdout)["response"]
        except subprocess.TimeoutExpired:
            return self.reply(504, {"ok": False, "error": "伤害计算超时"})
        except (OSError, subprocess.SubprocessError, ValueError, KeyError):
            return self.reply(503, {"ok": False, "error": "伤害引擎不可用"})
        self.reply(400 if payload.get("ok") is False else 200, payload)

    def do_GET(self):
        if self.path != "/healthz":
            return self.reply(404, {"ok": False, "error": "未知接口"})
        return self.calculate({})

    def do_POST(self):
        if self.path not in ROUTES:
            return self.reply(404, {"ok": False, "error": "未知接口"})
        if not self.authorized():
            return self.reply(401, {"ok": False, "error": "令牌无效"})
        try:
            size = int(self.headers["Content-Length"])
        except (TypeError, ValueError):
            return self.reply(400, {"ok": False, "error": "请求长度无效"})
        if not 0 < size <= MAX_REQUEST_BYTES:
            return self.reply(413, {"ok": False, "error": "请求超出长度限制"})
        try:
            body = json.loads(self.rfile.read(size))
            if not isinstance(body, dict):
                raise ValueError
            json.dumps(body, allow_nan=False)
        except (ValueError, UnicodeError, RecursionError):
            return self.reply(400, {"ok": False, "error": "需要有效的 JSON 对象"})
        return self.calculate(body)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=8788)
    args = parser.parse_args()
    if not ENGINE.is_file():
        parser.error("请先运行 bash build.sh")
    with HTTPServer(("127.0.0.1", args.port), Handler) as server:
        print(f"伤害引擎监听 http://127.0.0.1:{server.server_port}", flush=True)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    main()
