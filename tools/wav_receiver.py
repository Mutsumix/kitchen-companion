#!/usr/bin/env python3
"""CoreS3からPOSTされた録音WAVを受け取って保存する開発用サーバ。

使い方:
    python3 tools/wav_receiver.py [ポート番号]   # 省略時 8000

保存先: recordings/YYYYMMDD-HHMMSS.wav (プロジェクト直下、gitignore対象)
"""
import sys
import datetime
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

SAVE_DIR = Path(__file__).resolve().parent.parent / "recordings"


class WavReceiver(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length)
        SAVE_DIR.mkdir(exist_ok=True)
        name = datetime.datetime.now().strftime("%Y%m%d-%H%M%S") + ".wav"
        path = SAVE_DIR / name
        path.write_bytes(body)
        print(f"received {length} bytes -> {path}")
        self.send_response(200)
        self.send_header("Content-Length", "2")
        self.end_headers()
        self.wfile.write(b"OK")

    def log_message(self, fmt, *args):  # アクセスログは上のprintで足りるので抑制
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
    print(f"listening on 0.0.0.0:{port} (save dir: {SAVE_DIR})")
    HTTPServer(("0.0.0.0", port), WavReceiver).serve_forever()
