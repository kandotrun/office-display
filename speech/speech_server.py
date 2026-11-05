#!/usr/bin/env python3
"""Microphone owner and rough live captioning for the 2F display.

ALSA allows one process to hold the capture device, so this service owns the
microphone and serves everything derived from it:

    GET /status   {"db":..., "bands":[...], "text":..., "partial":..., "speaking":bool,
                   "phrases":[{"seq":..., "at":..., "text":...}, ...]}

Captions use Vosk by default. SPEECH_BACKEND=whisper selects optional local
faster-whisper inference over bounded utterances. Both receive RAM-only mono
PCM; captions are kept for the last few phrases and never written to disk.
Nothing here identifies a speaker.

"text" is the caption (the last two phrases younger than SPEECH_CAPTION_HOLD).
"phrases" are the last SPEECH_MAX_PHRASES final phrases, oldest first, whatever
their age: "seq" counts final phrases from 1 since this process started (a
reader that sees it go backwards resets its cursor) and "at" is the unix time
of captured speech, not later CPU decoding. Earendil uses it for echo rejection.

Whisper settings (all optional except the explicit local model directory):
    SPEECH_WHISPER_MODEL                 existing local model directory
    SPEECH_WHISPER_THREADS               2 (CPU int8)
    SPEECH_WHISPER_PROMPT                empty; proper-name vocabulary only
    SPEECH_WHISPER_VAD_DB                SPEECH_VAD_DB (-42)
    SPEECH_WHISPER_PRE_ROLL_SECONDS      0.3
    SPEECH_WHISPER_SILENCE_SECONDS       0.7
    SPEECH_WHISPER_MIN_SPEECH_SECONDS    0.3
    SPEECH_WHISPER_MAX_SECONDS           8
    SPEECH_WHISPER_QUEUE_SIZE            1 (1..4 pending utterances)
    SPEECH_WHISPER_MAX_AGE_SECONDS       20 (stale utterances/results are dropped)

Whisper keeps model files on disk but never creates audio files or downloads
a model at runtime. Its package is imported only when that backend is selected.

Numeric diagnostics expose capture/decode sample counts, queue duration,
dropped chunks/samples, discarded incomplete capture bytes and last_decode_at.
They contain no audio or transcription content.
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

vosk = None  # imported only when the default backend is initialised

DEVICE = os.environ.get("SPEECH_DEVICE", "plughw:2,0")
BACKEND = os.environ.get("SPEECH_BACKEND", "vosk").strip().lower()
MODEL_PATH = os.environ.get("SPEECH_MODEL", os.path.expanduser("~/speech/vosk-model-small-ja-0.22"))
PORT = int(os.environ.get("SPEECH_PORT", "8098"))
RATE = 16000
BANDS = 56
CHUNK = RATE // 10  # 100 ms
VAD_DB = float(os.environ.get("SPEECH_VAD_DB", "-42"))
VAD_HANGOVER = float(os.environ.get("SPEECH_VAD_HANGOVER", "1.5"))
MAX_PHRASES = int(os.environ.get("SPEECH_MAX_PHRASES", "6"))
CAPTION_HOLD = float(os.environ.get("SPEECH_CAPTION_HOLD", "25"))
MAX_PHRASE_CHARS = 300
WHISPER = None

LOCK = threading.Lock()
STATE = {
    "db": -90.0,
    "bands": [0.0] * BANDS,
    "text": "",
    "partial": "",
    "speaking": False,
    "updated": 0.0,
    "phrases": [],
    "sample_rate": RATE,
    "capture_samples": 0,
    "decoded_samples": 0,
    "dropped_chunks": 0,
    "dropped_samples": 0,
    "discarded_capture_bytes": 0,
    "last_decode_at": 0.0,
}


class PCMBlock(bytes):
    """PCM bytes that retain their capture time while waiting in the queue."""

    def __new__(cls, data, captured_at=None, captured_mono=None):
        result = super().__new__(cls, data)
        result.captured_at = time.time() if captured_at is None else captured_at
        result.captured_mono = time.monotonic() if captured_mono is None else captured_mono
        return result


class AudioQueue(queue.Queue):
    """A bounded PCM16 queue with an exact, content-free backlog duration."""

    def __init__(self, maxsize):
        self.pending_bytes = 0
        super().__init__(maxsize=maxsize)

    # Queue calls these while holding its mutex.
    def _put(self, data):
        super()._put(data)
        self.pending_bytes += len(data)

    def _get(self):
        data = super()._get()
        self.pending_bytes -= len(data)
        return data

    def milliseconds(self):
        with self.mutex:
            return round(self.pending_bytes * 1000.0 / (RATE * 2), 3)


QUEUE = AudioQueue(maxsize=40)  # ~4 s; drop oldest rather than fall behind


def queue_audio(data):
    """Retain the newest PCM even when an old block must be evicted."""
    if not isinstance(data, PCMBlock):
        data = PCMBlock(data)
    dropped_chunks = dropped_samples = 0
    while True:
        try:
            QUEUE.put_nowait(data)
            break
        except queue.Full:
            try:
                dropped = QUEUE.get_nowait()
            except queue.Empty:  # the decoder drained it since put_nowait
                continue
            dropped_chunks += 1
            dropped_samples += len(dropped) // 2
    with LOCK:
        STATE["capture_samples"] += len(data) // 2
        STATE["dropped_chunks"] += dropped_chunks
        STATE["dropped_samples"] += dropped_samples


def capture_blocks(stream):
    """Join pipe fragments into 100 ms blocks; never split a stereo frame.

    A final short block retains every complete frame at EOF/read failure.
    Only the final 1..3 bytes of an incomplete frame are discarded.
    """
    pending = bytearray()
    size = CHUNK * 4  # stereo S16_LE
    while True:
        try:
            data = stream.read(size - len(pending))
        except Exception:
            data = b""
        if not data:
            complete = len(pending) - len(pending) % 4
            with LOCK:
                STATE["discarded_capture_bytes"] += len(pending) - complete
            if complete:
                yield bytes(pending[:complete])
            return
        pending.extend(data)
        while len(pending) >= size:
            yield bytes(pending[:size])
            del pending[:size]


class Phrases:
    """The last few final phrases: numbered, timestamped, nothing else (no audio)."""

    def __init__(self, keep=MAX_PHRASES):
        self.keep = keep
        self.seq = 0
        self.items = []  # (seq, unix time, monotonic time, text), oldest first
        self.started_at = {}

    def add(self, text, at, mono, started_at=None):
        """Record a final phrase; its seq, or None for an empty result."""
        text = text.strip()[:MAX_PHRASE_CHARS]
        if not text:
            return None
        self.seq += 1
        self.items.append((self.seq, at, mono, text))
        del self.items[:-self.keep]
        if started_at is not None:
            self.started_at[self.seq] = started_at
        kept = {p[0] for p in self.items}
        self.started_at = {seq: start for seq, start in self.started_at.items() if seq in kept}
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
        return [{"seq": seq, "at": round(at, 2), "text": text,
                 **({"started_at": round(self.started_at[seq], 2)} if seq in self.started_at else {})}
                for seq, at, _, text in self.items]


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
        for chunk in capture_blocks(proc.stdout):
            captured_at, captured_mono = time.time(), time.monotonic()
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
            # AcceptWaveform(bytes) expects mono PCM16, not the normalised
            # float32 samples used above for the meter and spectrum.
            queue_audio(PCMBlock((mono * 32768.0).astype("<i2").tobytes(),
                                 captured_at, captured_mono))
        try:
            proc.kill()
        except Exception:
            pass
        time.sleep(1)


def load_vosk():
    global vosk
    if vosk is None:
        import vosk as module
        vosk = module
    model = vosk.Model(MODEL_PATH)
    rec = vosk.KaldiRecognizer(model, RATE)
    rec.SetWords(False)
    return rec


def decoded(samples):
    with LOCK:
        STATE["decoded_samples"] += samples
        STATE["last_decode_at"] = time.time()


def publish_phrases(phrases, text=None, at=None, mono=None, started_at=None):
    now = time.monotonic()
    with LOCK:
        if text is not None:
            phrases.add(text, at, mono, started_at)
        STATE["text"] = phrases.caption(now, CAPTION_HOLD)
        STATE["phrases"] = phrases.public()
        if not STATE["speaking"] and phrases.quiet_for(now, CAPTION_HOLD) > 8:
            STATE["partial"] = ""


def transcribe(rec=None):
    rec = load_vosk() if rec is None else rec
    phrases = Phrases(MAX_PHRASES)
    while True:
        data = QUEUE.get()
        done = rec.AcceptWaveform(data)
        decoded(len(data) // 2)
        if done:
            text = json.loads(rec.Result()).get("text", "")
            at = getattr(data, "captured_at", time.time())
            mono = getattr(data, "captured_mono", time.monotonic())
            publish_phrases(phrases, text, at, mono)
        else:
            partial = json.loads(rec.PartialResult()).get("partial", "").strip()[:MAX_PHRASE_CHARS]
            with LOCK:
                STATE["partial"] = partial
            publish_phrases(phrases)


def load_whisper():
    import whisper_backend as whisper
    phrases = Phrases(MAX_PHRASES)
    decoder = whisper.LocalWhisper(
        os.path.expanduser(os.environ.get("SPEECH_WHISPER_MODEL", "")),
        threads=int(os.environ.get("SPEECH_WHISPER_THREADS", "2")),
        prompt=os.environ.get("SPEECH_WHISPER_PROMPT", ""),
    )
    segmenter = whisper.Segmenter(
        threshold_db=float(os.environ.get("SPEECH_WHISPER_VAD_DB", str(VAD_DB))),
        pre_roll_seconds=float(os.environ.get("SPEECH_WHISPER_PRE_ROLL_SECONDS", "0.3")),
        silence_seconds=float(os.environ.get("SPEECH_WHISPER_SILENCE_SECONDS", "0.7")),
        min_speech_seconds=float(os.environ.get("SPEECH_WHISPER_MIN_SPEECH_SECONDS", "0.3")),
        max_seconds=float(os.environ.get("SPEECH_WHISPER_MAX_SECONDS", "8")),
    )
    backend = whisper.WhisperBackend(
        decoder, segmenter=segmenter,
        on_result=lambda text, at, mono, start: publish_phrases(phrases, text, at, mono, start),
        on_decode=decoded,
        queue_size=int(os.environ.get("SPEECH_WHISPER_QUEUE_SIZE", "1")),
        max_age_seconds=float(os.environ.get("SPEECH_WHISPER_MAX_AGE_SECONDS", "20")),
    )
    return backend, phrases


def transcribe_whisper(backend, phrases):
    # Segmentation never waits for CPU inference, so the raw microphone queue
    # keeps draining while the one inference worker is busy.
    while True:
        data = QUEUE.get()
        backend.feed(data, getattr(data, "captured_at", time.time()),
                     getattr(data, "captured_mono", time.monotonic()))
        publish_phrases(phrases)


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
        payload["queued_audio_ms"] = QUEUE.milliseconds()
        payload["backend"] = BACKEND
        if WHISPER is not None:
            payload.update(WHISPER.diagnostics())
        body = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)


def main():
    global WHISPER
    # Load before capture starts: model initialisation must not fill/drop the
    # raw microphone queue, and a missing local model fails without recording.
    if BACKEND == "vosk":
        consumer, args = transcribe, (load_vosk(),)
    elif BACKEND == "whisper":
        WHISPER, phrases = load_whisper()
        WHISPER.start()
        consumer, args = transcribe_whisper, (WHISPER, phrases)
    else:
        raise ValueError("SPEECH_BACKEND must be vosk or whisper")
    threading.Thread(target=consumer, args=args, daemon=True).start()
    threading.Thread(target=capture, name="capture", daemon=True).start()
    print("speech on 127.0.0.1:%d backend=%s" % (PORT, BACKEND), flush=True)
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()


if __name__ == "__main__":
    main()
