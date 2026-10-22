#!/usr/bin/env python3
"""Microphone owner and rough live captioning for the 2F display.

ALSA allows one process to hold the capture device, so this service owns the
microphone and serves everything derived from it:

    GET /status   {"db":..., "bands":[...], "text":..., "partial":..., "speaking":bool}

Captions are deliberately rough: a small Japanese Vosk model fed only the
speech segments. They are kept in memory for the last few phrases and are never
written to disk. Nothing here identifies a speaker.
"""
import json
import os
import queue
import subprocess
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
import vosk

DEVICE = os.environ.get("SPEECH_DEVICE", "plughw:2,0")
MODEL_PATH = os.environ.get("SPEECH_MODEL", os.path.expanduser("~/speech/vosk-model-small-ja-0.22"))
PORT = int(os.environ.get("SPEECH_PORT", "8098"))
RATE = 16000
BANDS = 56
CHUNK = RATE // 10  # 100 ms
VAD_DB = float(os.environ.get("SPEECH_VAD_DB", "-42"))
VAD_HANGOVER = float(os.environ.get("SPEECH_VAD_HANGOVER", "1.5"))
MAX_PHRASES = int(os.environ.get("SPEECH_MAX_PHRASES", "6"))
CAPTION_HOLD = float(os.environ.get("SPEECH_CAPTION_HOLD", "25"))

LOCK = threading.Lock()
STATE = {
    "db": -90.0,
    "bands": [0.0] * BANDS,
    "text": "",
    "partial": "",
    "speaking": False,
    "updated": 0.0,
}
QUEUE = queue.Queue(maxsize=40)  # ~4 s; drop oldest rather than fall behind


def bands_from(window):
    spec = np.abs(np.fft.rfft(window * np.hanning(len(window))))
    freqs = np.fft.rfftfreq(len(window), 1.0 / RATE)
    edges = np.logspace(np.log10(40.0), np.log10(8000.0), BANDS + 1)
    out = np.zeros(BANDS, dtype=np.float32)
    for i in range(BANDS):
        mask = (freqs >= edges[i]) & (freqs < edges[i + 1])
        out[i] = 20.0 * np.log10(max(float(spec[mask].mean()), 1e-9)) if mask.any() else -120.0
    lo, hi = float(out.min()), float(out.max())
    span = max(hi - lo, 8.0)
    return np.clip((out - lo) / span, 0.0, 1.0).tolist()


def capture():
    while True:
        try:
            proc = subprocess.Popen(
                ["arecord", "-q", "-D", DEVICE, "-f", "S16_LE", "-r", str(RATE),
                 "-c", "2", "-t", "raw"],
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                bufsize=0,
            )
        except Exception:
            time.sleep(2)
            continue
        window = np.zeros(0, dtype=np.float32)
        last_speech = 0.0
        while True:
            try:
                chunk = proc.stdout.read(CHUNK * 4)
            except Exception:
                break
            if not chunk:
                break
            frames = np.frombuffer(chunk, dtype=np.int16).reshape(-1, 2)
            mono = frames.mean(axis=1).astype(np.float32) / 32768.0
            rms = float(np.sqrt(np.mean(mono ** 2)))
            db = 20.0 * np.log10(max(rms, 1e-9))
            window = np.concatenate([window, mono])[-4096:]
            now = time.monotonic()
            if db >= VAD_DB:
                last_speech = now
            speaking = (now - last_speech) <= VAD_HANGOVER
            with LOCK:
                STATE["db"] = db
                STATE["speaking"] = speaking
                if len(window) >= 4096:
                    STATE["bands"] = bands_from(window)
                STATE["updated"] = time.time()
            try:
                QUEUE.put_nowait(mono.astype(np.float32).tobytes())
            except queue.Full:
                try:
                    QUEUE.get_nowait()
                except queue.Empty:
                    pass
        try:
            proc.kill()
        except Exception:
            pass
        time.sleep(1)


def transcribe():
    model = vosk.Model(MODEL_PATH)
    rec = vosk.KaldiRecognizer(model, RATE)
    rec.SetWords(False)
    phrases = []  # (monotonic time, text)
    while True:
        data = QUEUE.get()
        done = rec.AcceptWaveform(data)
        if done:
            text = json.loads(rec.Result()).get("text", "").strip()
            if text:
                phrases.append((time.monotonic(), text))
                del phrases[:-MAX_PHRASES]
        else:
            partial = json.loads(rec.PartialResult()).get("partial", "").strip()
            with LOCK:
                STATE["partial"] = partial
        now = time.monotonic()
        fresh = [p for p in phrases if now - p[0] <= CAPTION_HOLD]
        with LOCK:
            STATE["text"] = " / ".join(t for _, t in fresh[-2:])
            if not STATE["speaking"] and now - (fresh[-1][0] if fresh else 0) > 8:
                STATE["partial"] = ""


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, *args):
        pass

    def do_GET(self):
        if not self.path.startswith("/status"):
            self.send_error(404)
            return
        with LOCK:
            payload = dict(STATE)
        body = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)


def main():
    threading.Thread(target=capture, name="capture", daemon=True).start()
    threading.Thread(target=transcribe, name="transcribe", daemon=True).start()
    print("speech on 127.0.0.1:%d model=%s" % (PORT, MODEL_PATH), flush=True)
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()


if __name__ == "__main__":
    main()
