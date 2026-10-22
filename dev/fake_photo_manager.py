"""Stand-in for the display's photo-manager (dev only).

    python dev/fake_photo_manager.py <dir>

Serves GET /api/photos and GET /photos/<name> on 127.0.0.1:8081.
"""
import json
import os
import sys
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else ".")
PORT = int(os.environ.get("FAKE_PHOTOS_PORT", "8081"))


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def send(self, code, body, kind):
        self.send_response(code)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/api/photos":
            names = sorted(n for n in os.listdir(ROOT) if n.lower().endswith((".jpg", ".jpeg", ".png")))
            self.send(200, json.dumps(names).encode(), "application/json")
            return
        if self.path.startswith("/photos/"):
            name = urllib.parse.unquote(self.path[len("/photos/"):])
            path = os.path.join(ROOT, os.path.basename(name))
            if os.path.isfile(path):
                with open(path, "rb") as f:
                    kind = "image/png" if path.lower().endswith(".png") else "image/jpeg"
                    self.send(200, f.read(), kind)
                return
        self.send(404, b"{}", "application/json")


if __name__ == "__main__":
    print("fake photo-manager on 127.0.0.1:%d serving %s" % (PORT, ROOT), flush=True)
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
