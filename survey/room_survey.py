#!/usr/bin/env python3
"""Room survey: render the room's analysis as a live drawing.

The stream is drawn from three real signals only:

* brightness contours of the camera frame (the room as terrain),
* frame-to-frame motion (where something happened, as rings),
* the microphone spectrum (level per band).

It is published as an MJPEG stream so the photo frame can use it as its live
tile. No image is written to disk; the previous frame is kept in memory only.
"""
import io
import json
import os
import subprocess
import threading
import time
import urllib.request
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont, ImageOps

CAMERA_URL = os.environ.get("SURVEY_CAMERA_URL", "http://127.0.0.1:8082/snapshot.jpg")
FACES_URL = os.environ.get("SURVEY_FACES_URL", "http://127.0.0.1:8093/status")
STATS_URL = os.environ.get("SURVEY_STATS_URL", "http://127.0.0.1:8095/stats/week")
PORT = int(os.environ.get("SURVEY_PORT", "8094"))
W = int(os.environ.get("SURVEY_WIDTH", "720"))
H = int(os.environ.get("SURVEY_HEIGHT", "720"))
FPS = float(os.environ.get("SURVEY_FPS", "8"))
AUDIO_DEVICE = os.environ.get("SURVEY_AUDIO_DEVICE", "plughw:2,0")
AUDIO_RATE = 16000
BANDS = 56
GRID = (32, 18)
JPEG_QUALITY = int(os.environ.get("SURVEY_JPEG_QUALITY", "82"))

BG = (7, 9, 13)
CONTOUR = (150, 205, 235)
ACCENT = (255, 150, 60)
FOOT = (120, 215, 255)
TEXT = (238, 244, 250)
DIM = (115, 135, 155)

FONT_DIRS = (
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
)


def load_font(size):
    for path in FONT_DIRS:
        if os.path.exists(path):
            try:
                return ImageFont.truetype(path, size)
            except Exception:
                pass
    return ImageFont.load_default()


BIG = load_font(21)
SMALL = load_font(13)


class Audio:
    """Microphone level and spectrum, from a long-lived arecord process."""

    def __init__(self):
        self.lock = threading.Lock()
        self.bands = np.zeros(BANDS, dtype=np.float32)
        self.db = -90.0
        self.ceiling = -70.0

    def start(self):
        threading.Thread(target=self._loop, name="audio", daemon=True).start()

    def _loop(self):
        while True:
            try:
                proc = subprocess.Popen(
                    ["arecord", "-q", "-D", AUDIO_DEVICE, "-f", "S16_LE",
                     "-r", str(AUDIO_RATE), "-c", "1", "-t", "raw"],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL,
                    bufsize=0,
                )
            except Exception:
                time.sleep(2)
                continue
            window = np.zeros(0, dtype=np.float32)
            while True:
                try:
                    chunk = proc.stdout.read(AUDIO_RATE // 5 * 2)
                except Exception:
                    break
                if not chunk:
                    break
                samples = np.frombuffer(chunk, dtype=np.int16).astype(np.float32) / 32768.0
                window = np.concatenate([window, samples])[-8192:]
                if len(window) < 4096:
                    continue
                self._update(window)
            try:
                proc.kill()
            except Exception:
                pass
            time.sleep(1)

    def _update(self, window):
        rms = float(np.sqrt(np.mean(window ** 2)))
        db = 20.0 * np.log10(max(rms, 1e-9))
        spec = np.abs(np.fft.rfft(window * np.hanning(len(window))))
        freqs = np.fft.rfftfreq(len(window), 1.0 / AUDIO_RATE)
        edges = np.logspace(np.log10(40.0), np.log10(8000.0), BANDS + 1)
        bands = np.zeros(BANDS, dtype=np.float32)
        for i in range(BANDS):
            mask = (freqs >= edges[i]) & (freqs < edges[i + 1])
            if mask.any():
                bands[i] = 20.0 * np.log10(max(float(spec[mask].mean()), 1e-9))
            else:
                bands[i] = -120.0
        # Show the shape of the spectrum, not its absolute level: the room is
        # often near the noise floor, so a fixed scale would look dead.
        lo = float(bands.min())
        hi = float(bands.max())
        span = max(hi - lo, 8.0)
        with self.lock:
            self.db = db
            self.ceiling = hi
            self.bands = np.clip((bands - lo) / span, 0.0, 1.0)

    def read(self):
        with self.lock:
            return self.db, self.bands.copy()


class Latest:
    """Single-slot broadcast of the newest rendered frame."""

    def __init__(self):
        self.cond = threading.Condition()
        self.seq = 0
        self.jpeg = b""
        self.stamp = ""

    def publish(self, seq, jpeg, stamp):
        with self.cond:
            self.seq, self.jpeg, self.stamp = seq, jpeg, stamp
            self.cond.notify_all()

    def wait(self, after, timeout=5.0):
        with self.cond:
            if self.seq == after:
                self.cond.wait(timeout)
            return self.seq, self.jpeg, self.stamp


LATEST = Latest()
AUDIO = Audio()
STATE = {"motion": 0.0, "faces": 0, "db": -90.0}
STATS = {"grid": None, "at": 0.0}


def fetch_stats():
    """Weekly activity grid; refreshed in the background so rendering never waits."""
    while True:
        try:
            with urllib.request.urlopen(STATS_URL, timeout=3) as response:
                data = json.loads(response.read())
            STATS["grid"] = data.get("grid")
            STATS["at"] = time.time()
        except Exception:
            pass
        time.sleep(60)


def fetch_snapshot():
    try:
        with urllib.request.urlopen(CAMERA_URL, timeout=3) as response:
            return response.read()
    except Exception:
        return None


def fetch_faces():
    try:
        with urllib.request.urlopen(FACES_URL, timeout=2) as response:
            status = json.loads(response.read())
        if status.get("healthy"):
            return int(status.get("detected_faces") or 0)
    except Exception:
        pass
    return 0


def motion_grid(current, previous, decay):
    small = np.asarray(current.resize((160, 90), Image.BILINEAR), dtype=np.float32)
    if previous is None:
        return np.zeros((GRID[1], GRID[0]), dtype=np.float32), small
    diff = np.abs(small - previous)
    gh, gw = GRID[1], GRID[0]
    cells = diff.reshape(gh, 90 // gh, gw, 160 // gw).mean(axis=(1, 3))
    return cells.astype(np.float32), small


def heatmap(draw, grid, x0, y0, cw, chh, now_hour):
    """7x24 activity grid; empty cells stay dark so 'no data yet' is visible."""
    top = 0.0
    for row in grid or []:
        for cell in row:
            if cell:
                top = max(top, cell["motion"])
    scale = max(top, 1.0)
    for wd in range(7):
        for hr in range(24):
            cell = grid[wd][hr] if grid else None
            if not cell:
                fill = (255, 255, 255, 14)
            else:
                v = min(1.0, (cell["motion"] / scale) ** 0.55)
                fill = (
                    int(16 + (255 - 16) * v),
                    int(22 + (150 - 22) * v),
                    int(30 + (60 - 30) * v),
                    int(60 + 180 * v),
                )
            box = [x0 + hr * cw, y0 + wd * chh, x0 + hr * cw + cw - 2, y0 + wd * chh + chh - 2]
            draw.rectangle(box, fill=fill)
            if hr == now_hour:
                draw.rectangle(box, outline=CONTOUR + (60,))


def render(gray, motion, spec, db, faces, t):
    img = Image.new("RGB", (W, H), BG)
    draw = ImageDraw.Draw(img, "RGBA")

    for x in range(0, W, 48):
        draw.line([(x, 0), (x, H)], fill=(255, 255, 255, 7))
    for y in range(0, H, 48):
        draw.line([(0, y), (W, y)], fill=(255, 255, 255, 7))

    smooth = gray.filter(ImageFilter.GaussianBlur(6))
    for threshold, alpha in ((64, 60), (96, 85), (128, 110), (160, 145), (196, 190)):
        edge = (
            smooth.point(lambda v: 255 if v > threshold else 0)
            .filter(ImageFilter.FIND_EDGES)
            .filter(ImageFilter.GaussianBlur(0.4))
        )
        img.paste(Image.new("RGB", (W, H), CONTOUR), (0, 0), edge.point(lambda v: min(alpha, v)))

    cw, ch = W // GRID[0], H // GRID[1]
    peak = 0.0
    for gy in range(GRID[1]):
        for gx in range(GRID[0]):
            m = float(motion[gy, gx])
            peak = max(peak, m)
            cx, cy = gx * cw + cw // 2, gy * ch + ch // 2
            if m > 2.2:
                r = min(18.0, 5.0 + m * 1.6)
                a = min(235, int(70 + m * 22))
                draw.ellipse([cx - r, cy - r, cx + r, cy + r], outline=ACCENT + (a,), width=2)
                draw.ellipse([cx - 1, cy - 1, cx + 1, cy + 1], fill=ACCENT + (a,))
            elif m > 0.9:
                draw.ellipse([cx - 2, cy - 2, cx + 2, cy + 2], fill=ACCENT + (70,))

    header_h = int(H * 0.13)
    footer_top = int(H * 0.55)
    draw.rectangle([0, 0, W, header_h], fill=BG)
    draw.line([(48, header_h), (W - 48, header_h)], fill=CONTOUR + (55,), width=1)
    draw.rectangle([0, footer_top, W, H], fill=BG)
    draw.line([(48, footer_top), (W - 48, footer_top)], fill=FOOT + (55,), width=1)

    # spectrum (top half of the footer)
    base_y = footer_top + int(H * 0.20)
    xs = np.linspace(48, W - 48, BANDS)
    ys = base_y - spec * (H * 0.12)
    for x, y, v in zip(xs, ys, spec):
        draw.line([(x, base_y), (x, y)], fill=FOOT + (int(22 + 70 * v),), width=2)
    draw.line(list(zip(xs, ys)), fill=FOOT + (235,), width=2)
    draw.line([(48, base_y), (W - 48, base_y)], fill=FOOT + (80,), width=1)

    # weekly activity grid (bottom half of the footer)
    grid = STATS["grid"]
    cell_w, cell_h = 19, 15
    hx = (W - cell_w * 24 + 18) // 2
    hy = base_y + int(H * 0.05)
    heatmap(draw, grid, hx, hy, cell_w, cell_h, datetime.now().hour)
    for wd, label in enumerate(("S", "M", "T", "W", "T", "F", "S")):
        draw.text((hx - 8, hy + wd * cell_h + cell_h // 2 - 1), label,
                  font=SMALL, fill=DIM, anchor="rm")
    draw.text((hx, hy + cell_h * 7 + 6), "28 DAYS / ACTIVITY", font=SMALL, fill=DIM)
    for hr in (0, 6, 12, 18):
        draw.text((hx + hr * cell_w, hy - 16), "%02d" % hr, font=SMALL, fill=DIM)

    # "vs usual": current activity against this weekday/hour average
    usual = None
    if grid:
        sqlite_wd = (datetime.now().weekday() + 1) % 7
        cell = grid[sqlite_wd][datetime.now().hour]
        # one or two minutes is not a baseline; wait for a real sample
        if cell and cell.get("motion") and cell.get("minutes", 0) >= 20:
            usual = peak / cell["motion"]
    items = [
        ("MOTION", "%.2f" % peak),
        ("AUDIO", "%.0f dBFS" % db),
        ("BANDS", "%d" % BANDS),
        ("FACES", "%d" % faces),
        ("VS USUAL", "-" if usual is None else "%.1fx" % usual),
    ]
    for i, (key, value) in enumerate(items):
        x = 48 + i * (W - 96) // len(items)
        draw.text((x, 16), key, font=SMALL, fill=CONTOUR)
        draw.text((x, 34), value, font=BIG, fill=TEXT)

    draw.text((48, footer_top + 8), "2F DISPLAY / LIVE ANALYSIS", font=SMALL, fill=DIM)
    draw.text((W - 48, footer_top + 8), "SPECTRUM 40Hz-8kHz", font=SMALL, fill=DIM, anchor="ra")
    return img


def loop():
    previous_small = None
    motion = np.zeros((GRID[1], GRID[0]), dtype=np.float32)
    seq = int(time.time())
    last_faces = 0
    last_face_poll = 0.0
    interval = 1.0 / max(1.0, FPS)
    while True:
        started = time.monotonic()
        jpeg = fetch_snapshot()
        if jpeg is None:
            time.sleep(0.5)
            continue
        frame = Image.open(io.BytesIO(jpeg)).convert("L")
        # centre-crop to the canvas aspect so the room is not squashed
        aspect = W / H
        fw, fh = frame.size
        if fw / fh > aspect:
            cw = int(fh * aspect)
            left = (fw - cw) // 2
            frame = frame.crop((left, 0, left + cw, fh))
        else:
            chh = int(fw / aspect)
            top = (fh - chh) // 2
            frame = frame.crop((0, top, fw, top + chh))
        frame = frame.resize((W, H), Image.LANCZOS)
        fresh, previous_small = motion_grid(frame, previous_small, 0.82)
        motion = np.maximum(fresh, motion * 0.86)

        now = time.monotonic()
        if now - last_face_poll > 2.0:
            last_faces = fetch_faces()
            last_face_poll = now
        db, spec = AUDIO.read()

        img = render(frame, motion, spec, db, last_faces, now)
        buf = io.BytesIO()
        img.save(buf, "JPEG", quality=JPEG_QUALITY)
        seq += 1
        stamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"
        STATE.update(motion=float(motion.max()), faces=last_faces, db=db)
        LATEST.publish(seq, buf.getvalue(), stamp)

        elapsed = time.monotonic() - started
        time.sleep(max(0.0, interval - elapsed))


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
                    seq, jpeg, stamp = LATEST.wait(last)
                    if seq == last or not jpeg:
                        continue
                    last = seq
                    self.wfile.write(b"--frame\r\n")
                    self.wfile.write(b"Content-Type: image/jpeg\r\n")
                    self.wfile.write(b"Content-Length: %d\r\n" % len(jpeg))
                    self.wfile.write(b"X-Frame-Sequence: %d\r\n" % seq)
                    self.wfile.write(b"X-Frame-Timestamp: %s\r\n" % stamp.encode())
                    self.wfile.write(b"\r\n")
                    self.wfile.write(jpeg)
                    self.wfile.write(b"\r\n")
            except (BrokenPipeError, ConnectionResetError):
                return
        elif self.path.startswith("/frame.jpg"):
            seq, jpeg, _ = LATEST.wait(-1, timeout=3.0)
            if not jpeg:
                self.send_error(503)
                return
            self.send_response(200)
            self.send_header("Content-Type", "image/jpeg")
            self.send_header("Content-Length", str(len(jpeg)))
            self.end_headers()
            self.wfile.write(jpeg)
        elif self.path.startswith("/status"):
            body = (
                '{"motion":%.3f,"faces":%d,"db":%.1f,"seq":%d}'
                % (STATE["motion"], STATE["faces"], STATE["db"], LATEST.seq)
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_error(404)


def main():
    AUDIO.start()
    threading.Thread(target=fetch_stats, name="stats", daemon=True).start()
    threading.Thread(target=loop, name="render", daemon=True).start()
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    print("room-survey on 127.0.0.1:%d (%dx%d @ %.0ffps)" % (PORT, W, H, FPS), flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
