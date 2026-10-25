#!/usr/bin/env python3
"""Honeypot live view for the 2F display.

Multiplexes the room-survey MJPEG stream (the usual live-tile
source) with a periodically rendered honeypot dashboard, so the
photo frame can keep showing the room and still surface the
honeypot situation in real time.

Data path: Sakura VPS (status producer, tailnet only) -> kan-pi
relay on the office LAN -> this service. The dashboard cuts in
every CUTIN_EVERY seconds for CUTIN_SECONDS, and immediately when
the feed reports a download or upload event. Output follows the
room-survey contract exactly: 720x720 MJPEG parts carrying
X-Frame-Sequence and X-Frame-Timestamp (UTC, <3s old), plus
/frame.jpg and /status on loopback.
"""
import io
import json
import os
import socket
import threading
import time
import urllib.request
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from PIL import Image, ImageDraw, ImageFont

PORT = int(os.environ.get("HONEYPOT_PORT", "8097"))
DATA_PORT = int(os.environ.get("HONEYPOT_DATA_PORT", "8100"))
DATA_URL = os.environ.get("HONEYPOT_DATA_URL", "http://192.168.80.123:8902/status.json")
SURVEY_HOST = os.environ.get("HONEYPOT_SURVEY_HOST", "127.0.0.1")
SURVEY_PORT = int(os.environ.get("HONEYPOT_SURVEY_PORT", "8094"))
CUTIN_EVERY = int(os.environ.get("HONEYPOT_CUTIN_EVERY", "180"))
CUTIN_SECONDS = int(os.environ.get("HONEYPOT_CUTIN_SECONDS", "30"))
FIRST_CUTIN = int(os.environ.get("HONEYPOT_FIRST_CUTIN", "20"))
WIDTH = HEIGHT = 720
FPS = 6
JPEG_QUALITY = 82

BG = (8, 10, 15)
PANEL = (17, 21, 31)
RULE = (35, 43, 61)
TEXT = (230, 235, 242)
DIM = (138, 147, 165)
AMBER = (255, 176, 84)
CYAN = (127, 212, 255)
GREEN = (111, 224, 138)
RED = (224, 90, 90)

MONO_PATHS = (
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
)
JP_PATHS = (
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
)


def load_font(paths, size):
    for path in paths:
        if os.path.exists(path):
            try:
                return ImageFont.truetype(path, size)
            except Exception:
                pass
    return ImageFont.load_default()


def stamp_now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


class Latest:
    """Latest-frame store, same publish/wait shape as room-survey."""

    def __init__(self):
        self.cond = threading.Condition()
        self.seq = 0
        self.jpeg = None
        self.at = 0.0

    def publish(self, jpeg):
        with self.cond:
            self.seq += 1
            self.jpeg = jpeg
            self.at = time.monotonic()
            self.cond.notify_all()

    def wait(self, last, timeout=5.0):
        with self.cond:
            self.cond.wait_for(lambda: self.seq != last, timeout=timeout)
            return self.seq, self.jpeg, self.at


SURVEY = Latest()
OUT = Latest()
DATA = {"snap": None, "at": 0.0}
ALERT = {"pending": False, "seen_ts": ""}


def poll_data():
    while True:
        try:
            req = urllib.request.Request(DATA_URL, headers={"User-Agent": "honeypot-view/1.0"})
            with urllib.request.urlopen(req, timeout=4) as r:
                snap = json.loads(r.read())
            DATA["snap"] = snap
            DATA["at"] = time.monotonic()
            for item in snap.get("feed", []):
                if "⚠" in item.get("kind", "") and item.get("ts", "") > ALERT["seen_ts"]:
                    ALERT["seen_ts"] = item["ts"]
                    ALERT["pending"] = True
        except Exception:
            pass
        time.sleep(5)


def fetch_survey():
    """Continuously read the room-survey MJPEG stream and keep the
    newest complete frame."""
    while True:
        try:
            s = socket.create_connection((SURVEY_HOST, SURVEY_PORT), timeout=5)
            s.sendall(b"GET /stream HTTP/1.0\r\nHost: localhost\r\n\r\n")
            buf = b""
            while b"\r\n\r\n" not in buf:
                chunk = s.recv(4096)
                if not chunk:
                    raise ConnectionError("survey closed during headers")
                buf += chunk
            buf = buf.split(b"\r\n\r\n", 1)[1]
            while True:
                # Align to the next part boundary.
                idx = buf.find(b"--frame\r\n")
                while idx < 0:
                    chunk = s.recv(65536)
                    if not chunk:
                        raise ConnectionError("survey closed")
                    buf += chunk
                    idx = buf.find(b"--frame\r\n")
                buf = buf[idx + len(b"--frame\r\n"):]
                while b"\r\n\r\n" not in buf:
                    chunk = s.recv(65536)
                    if not chunk:
                        raise ConnectionError("survey closed mid-headers")
                    buf += chunk
                head, buf = buf.split(b"\r\n\r\n", 1)
                length = None
                for line in head.split(b"\r\n"):
                    if line.lower().startswith(b"content-length:"):
                        length = int(line.split(b":", 1)[1].strip())
                if length is None:
                    continue
                while len(buf) < length:
                    chunk = s.recv(65536)
                    if not chunk:
                        raise ConnectionError("survey closed mid-frame")
                    buf += chunk
                jpeg, buf = buf[:length], buf[length:]
                if len(jpeg) > 1000:
                    SURVEY.publish(jpeg)
        except Exception:
            time.sleep(2)


def fit_text(draw, text, font, max_w):
    if draw.textlength(text, font=font) <= max_w:
        return text
    while text and draw.textlength(text + "…", font=font) > max_w:
        text = text[:-1]
    return text + "…"


def _runs(text):
    """Split text into (run, is_ascii) groups: the mono font covers
    ASCII only and the fallback font's Latin/digits are tofu, so
    mixed strings must be drawn run by run."""
    out = []
    cur = ""
    cur_ascii = None
    for ch in text:
        a = ord(ch) < 128
        if cur_ascii is None or a == cur_ascii:
            cur += ch
            cur_ascii = a
        else:
            out.append((cur, cur_ascii))
            cur = ch
            cur_ascii = a
    if cur:
        out.append((cur, cur_ascii))
    return out


def measure_mixed(draw, text, mono_font, jp_font):
    return sum(draw.textlength(run, font=mono_font if a else jp_font)
               for run, a in _runs(text))


def draw_mixed(draw, x, y, text, mono_font, jp_font, fill):
    for run, a in _runs(text):
        font = mono_font if a else jp_font
        draw.text((x, y), run, font=font, fill=fill)
        x += draw.textlength(run, font=font)
    return x


def fit_mixed(draw, text, mono_font, jp_font, max_w):
    if measure_mixed(draw, text, mono_font, jp_font) <= max_w:
        return text
    while text and measure_mixed(draw, text + "…", mono_font, jp_font) > max_w:
        text = text[:-1]
    return text + "…"


def render_dashboard():
    snap = DATA["snap"] or {}
    cowrie = snap.get("cowrie", {})
    canary = snap.get("canary", {})
    img = Image.new("RGB", (WIDTH, HEIGHT), BG)
    d = ImageDraw.Draw(img)
    mono_l = load_font(MONO_PATHS, 30)
    mono_m = load_font(MONO_PATHS, 19)
    mono_s = load_font(MONO_PATHS, 15)
    jp_m = load_font(JP_PATHS, 16)
    jp_l = load_font(JP_PATHS, 21)

    gen = snap.get("generated_at", "")
    clock = gen[11:19] if len(gen) > 19 else "--:--:--"
    stale = (time.monotonic() - DATA["at"]) > 30 if DATA["at"] else True
    d.text((28, 24), "HONEYPOT LIVE", font=mono_l, fill=AMBER)
    draw_mixed(d, 28, 62, "さくらVPS への攻撃状況", mono_l, jp_l, DIM)
    clk_w = d.textlength(clock, font=mono_l)
    d.text((WIDTH - 28 - clk_w, 24), clock, font=mono_l, fill=TEXT)
    dot = RED if stale else GREEN
    d.ellipse([WIDTH - 48, 66, WIDTH - 28, 86], fill=dot)
    if stale:
        d.text((WIDTH - 150, 66), "STALE", font=mono_s, fill=RED)
    d.line([(28, 100), (WIDTH - 28, 100)], fill=RULE, width=2)

    stats = [
        (str(cowrie.get("sessions_today", "-")), "今日のセッション", TEXT),
        (str(cowrie.get("sessions_1h", "-")), "直近1時間", TEXT),
        (str(len(cowrie.get("active_sessions", []))), "接続中",
         AMBER if cowrie.get("active_sessions") else TEXT),
        (str(canary.get("events_today", "-")), "他ポートの攻撃", TEXT),
    ]
    box_w = (WIDTH - 56 - 30) // 4
    for i, (value, label, color) in enumerate(stats):
        x = 28 + i * (box_w + 10)
        d.rounded_rectangle([x, 116, x + box_w, 196], radius=10, fill=PANEL)
        vw = d.textlength(value, font=load_font(MONO_PATHS, 38))
        d.text((x + (box_w - vw) / 2, 126), value, font=load_font(MONO_PATHS, 38), fill=color)
        lw = measure_mixed(d, label, mono_s, jp_m)
        draw_mixed(d, x + (box_w - lw) / 2, 170, label, mono_s, jp_m, DIM)

    d.text((28, 214), "LAST 60 MIN / MIN", font=mono_s, fill=DIM)
    rates = snap.get("events_per_minute", []) or [0] * 60
    top = max(max(rates), 1)
    x0, y0, x1, y1 = 28, 244, WIDTH - 28, 330
    d.rectangle([x0, y0, x1, y1], outline=RULE)
    step = (x1 - x0) / (len(rates) - 1)
    pts = [(x0 + i * step, y1 - 4 - (v / top) * (y1 - y0 - 10)) for i, v in enumerate(rates)]
    if len(pts) > 1:
        d.line(pts, fill=AMBER, width=2)

    d.text((28, 348), "LIVE FEED", font=mono_s, fill=DIM)
    y = 374
    for item in (snap.get("feed", []) or [])[:9]:
        t = item.get("ts", "")[11:19]
        kind = item.get("kind", "")
        src = item.get("src", "")
        detail = item.get("detail", "")
        color = AMBER if "⚠" in kind else TEXT
        d.text((28, y), t, font=mono_s, fill=DIM)
        d.text((118, y), src, font=mono_s, fill=CYAN)
        line = fit_mixed(d, f"{kind} {detail}", mono_s, jp_m, WIDTH - 292 - 28)
        draw_mixed(d, 292, y, line, mono_s, jp_m, color)
        y += 24

    d.line([(28, 606), (WIDTH - 28, 606)], fill=RULE, width=2)
    d.text((28, 618), "TOP SOURCES", font=mono_s, fill=DIM)
    y = 642
    for ip, n in (cowrie.get("top_ips", []) or [])[:3]:
        d.text((28, y), fit_text(d, f"{ip}  ×{n}", mono_s, 320), font=mono_s, fill=TEXT)
        y += 20
    d.text((380, 618), "CANARY PORTS", font=mono_s, fill=DIM)
    y = 642
    for name, n in (canary.get("by_port", []) or [])[:3]:
        d.text((380, y), fit_text(d, f"{name}  ×{n}", mono_s, 300), font=mono_s, fill=TEXT)
        y += 20

    buf = io.BytesIO()
    img.save(buf, "JPEG", quality=JPEG_QUALITY)
    return buf.getvalue()


def produce():
    started = time.monotonic()
    next_cutin = started + FIRST_CUTIN
    cutin_until = 0.0
    dash_cache = {"jpeg": None, "at": 0.0}
    interval = 1.0 / FPS
    while True:
        t0 = time.monotonic()
        if ALERT["pending"] and t0 >= cutin_until:
            cutin_until = t0 + CUTIN_SECONDS
            next_cutin = t0 + CUTIN_EVERY
            ALERT["pending"] = False
        if t0 >= next_cutin and t0 >= cutin_until:
            cutin_until = t0 + CUTIN_SECONDS
            next_cutin = t0 + CUTIN_EVERY
        in_cutin = t0 < cutin_until
        survey_fresh = SURVEY.jpeg is not None and (t0 - SURVEY.at) < 10
        if in_cutin or not survey_fresh:
            if t0 - dash_cache["at"] > 0.5 or dash_cache["jpeg"] is None:
                try:
                    dash_cache["jpeg"] = render_dashboard()
                    dash_cache["at"] = t0
                except Exception:
                    dash_cache["jpeg"] = dash_cache["jpeg"]
            if dash_cache["jpeg"]:
                OUT.publish(dash_cache["jpeg"])
            OUT_SOURCE["name"] = "honeypot"
        else:
            OUT.publish(SURVEY.jpeg)
            OUT_SOURCE["name"] = "survey"
        elapsed = time.monotonic() - t0
        time.sleep(max(0.0, interval - elapsed))


OUT_SOURCE = {"name": "survey"}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, *args):
        pass

    def do_GET(self):
        if self.path.startswith("/stream"):
            self.send_response(200)
            self.send_header("Content-Type", "multipart/x-mixed-replace; boundary=frame")
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            last = -1
            try:
                while True:
                    seq, jpeg, _ = OUT.wait(last)
                    if seq == last or not jpeg:
                        continue
                    last = seq
                    self.wfile.write(b"--frame\r\n")
                    self.wfile.write(b"Content-Type: image/jpeg\r\n")
                    self.wfile.write(b"Content-Length: %d\r\n" % len(jpeg))
                    self.wfile.write(b"X-Frame-Sequence: %d\r\n" % seq)
                    self.wfile.write(b"X-Frame-Timestamp: %s\r\n" % stamp_now().encode())
                    self.wfile.write(b"\r\n")
                    self.wfile.write(jpeg)
                    self.wfile.write(b"\r\n")
            except (BrokenPipeError, ConnectionResetError):
                return
        elif self.path.startswith("/frame.jpg"):
            _, jpeg, _ = OUT.wait(-1, timeout=3.0)
            if not jpeg:
                self.send_error(503)
                return
            self.send_response(200)
            self.send_header("Content-Type", "image/jpeg")
            self.send_header("Content-Length", str(len(jpeg)))
            self.end_headers()
            self.wfile.write(jpeg)
        elif self.path.startswith("/status"):
            body = json.dumps({
                "source": OUT_SOURCE["name"],
                "seq": OUT.seq,
                "snapshot_age_s": round(time.monotonic() - DATA["at"], 1) if DATA["at"] else None,
                "survey_age_s": round(time.monotonic() - SURVEY.at, 1) if SURVEY.at else None,
            }).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_error(404)


class DataHandler(BaseHTTPRequestHandler):
    """The raw snapshot for the photo frame's attack-map strip view."""

    protocol_version = "HTTP/1.0"

    def log_message(self, *args):
        pass

    def do_GET(self):
        if self.path.startswith("/status.json"):
            snap = DATA["snap"]
            if snap is None:
                self.send_error(503)
                return
            body = json.dumps(snap, ensure_ascii=False).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_error(404)


def serve_data():
    ThreadingHTTPServer(("127.0.0.1", DATA_PORT), DataHandler).serve_forever()


if __name__ == "__main__":
    threading.Thread(target=poll_data, daemon=True).start()
    threading.Thread(target=fetch_survey, daemon=True).start()
    threading.Thread(target=produce, daemon=True).start()
    threading.Thread(target=serve_data, daemon=True).start()
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
