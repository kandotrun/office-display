#!/usr/bin/env python3
"""Microphone owner and rough live captioning for the 2F display.

ALSA allows one process to hold the capture device, so this service owns the
microphone and serves everything derived from it:

    GET /status   {"db":..., "bands":[...], "text":..., "partial":..., "speaking":bool,
                   "phrases":[{"seq":..., "at":..., "text":...}, ...]}

Captions are deliberately rough: a small Japanese Vosk model fed only the
speech segments. They are kept in memory for the last few phrases and are never
written to disk. Nothing here identifies a speaker.

"text" is the caption (the last two phrases younger than SPEECH_CAPTION_HOLD).
"phrases" are the last SPEECH_MAX_PHRASES final phrases, oldest first, whatever
their age: "seq" counts final phrases from 1 since this process started (a
reader that sees it go backwards resets its cursor) and "at" is the unix time
the phrase was finalised. earendil reads them to hear its wake phrase.
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
    "phrases": [],
}
QUEUE = queue.Queue(maxsize=40)  # ~4 s; drop oldest rather than fall behind


class Phrases:
    """The last few final phrases: numbered, timestamped, nothing else (no audio)."""

    def __init__(self, keep=MAX_PHRASES):
        self.keep = keep
        self.seq = 0
        self.items = []  # (seq, unix time, monotonic time, text), oldest first

    def add(self, text, at, mono):
        """Record a final phrase; its seq, or None for an empty result."""
        text = text.strip()
        if not text:
            return None
        self.seq += 1
        self.items.append((self.seq, at, mono, text))
        del self.items[:-self.keep]
        return self.seq

    def _fresh(self, now, hold):
        return [p for p in self.items if now - p[2] <= hold]

    def caption(self, now, hold):
        """The caption: the last two phrases younger than hold (monotonic seconds)."""
        return " / ".join(p[3] for p in self._fresh(now, hold)[-2:])

    def quiet_for(self, now, hold):
        """Seconds since the newest phrase younger than hold (inf without one)."""
        fresh = self._fresh(now, hold)
        return now - fresh[-1][2] if fresh else float("inf")

    def public(self):
        """The /status "phrases" list (a new list: safe to publish as is)."""
        return [{"seq": seq, "at": round(at, 2), "text": text} for seq, at, _, text in self.items]


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
            frames = np.frombuffer(chunk, dtype="<i2").reshape(-1, 2)
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
                # AcceptWaveform(bytes) expects mono PCM16, not the normalised
                # float32 samples used above for the meter and spectrum.
                QUEUE.put_nowait((mono * 32768.0).astype("<i2").tobytes())
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
    phrases = Phrases(MAX_PHRASES)
    while True:
        data = QUEUE.get()
        done = rec.AcceptWaveform(data)
        if done:
            text = json.loads(rec.Result()).get("text", "")
            phrases.add(text, time.time(), time.monotonic())
        else:
            partial = json.loads(rec.PartialResult()).get("partial", "").strip()
            with LOCK:
                STATE["partial"] = partial
        now = time.monotonic()
        caption = phrases.caption(now, CAPTION_HOLD)
        quiet = phrases.quiet_for(now, CAPTION_HOLD)
        public = phrases.public()
        with LOCK:
            STATE["text"] = caption
            STATE["phrases"] = public
            if not STATE["speaking"] and quiet > 8:
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
