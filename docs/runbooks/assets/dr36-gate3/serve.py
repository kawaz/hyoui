#!/usr/bin/env python3
"""DR-0036 gate 3 の観測用サーバ。検証専用の使い捨て。

同一 site・別 origin の 2 つを立てる (= ccmsg webui と hyoui endpoint の関係の模型)。
`localhost:<port>` を使うのは、WebAuthn の RP ID が domain であって IP は使えず、
かつ `http://localhost` が secure context 扱いになるため。

routes:
  /                 index.html (dir から)
  /setcookie        Secure / 非 Secure の cookie を 1 組 set する
  /whoami           server に届いた Cookie ヘッダをそのまま返す
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

PORT = int(sys.argv[1])
ROOT = Path(sys.argv[2])


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _send(self, status, body: bytes, ctype: str, extra_headers=()):
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in extra_headers:
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/whoami":
            body = json.dumps({"cookie": self.headers.get("Cookie", "")}).encode()
            self._send(200, body, "application/json")
            return
        if path == "/setcookie":
            # Secure 付き (= https 以外では保存されないはず) と、比較用の非 Secure。
            headers = [
                (
                    "Set-Cookie",
                    "__Secure-hyoui-probe=secure-value; Path=/; HttpOnly; Secure; SameSite=Strict",
                ),
                (
                    "Set-Cookie",
                    "hyoui-probe-plain=plain-value; Path=/; HttpOnly; SameSite=Strict",
                ),
            ]
            self._send(200, b'{"set":true}', "application/json", headers)
            return
        rel = "index.html" if path == "/" else path.lstrip("/")
        target = ROOT / rel
        if not target.is_file():
            self._send(404, b"not found", "text/plain")
            return
        ctype = "text/html; charset=utf-8" if rel.endswith(".html") else "text/plain"
        self._send(200, target.read_bytes(), ctype)

    def log_message(self, *args):
        pass


print(f"serve: http://localhost:{PORT} root={ROOT}", flush=True)
ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
