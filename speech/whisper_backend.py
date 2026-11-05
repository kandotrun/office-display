"""Optional, RAM-only Japanese STT with a local faster-whisper model.

The inexpensive segmenter drains microphone PCM while a separate worker
decodes bounded utterances. Nothing here opens an audio file, uploads audio,
or logs recognized text. Utterance times refer to captured speech, never to
the later time at which CPU inference happens to finish.
"""
from __future__ import annotations

from collections import deque
from dataclasses import dataclass
import math
import os
import queue
import re
import threading
import time
import unicodedata

import numpy as np

RATE = 16000
MAX_TEXT = 300
NO_SPEECH_THRESHOLD = 0.6
MIN_LOGPROB = -1.0
MAX_COMPRESSION_RATIO = 2.4


def normalized_words(text):
    out = []
    for ch in unicodedata.normalize("NFKC", text).casefold():
        if 0x30A1 <= ord(ch) <= 0x30F6:
            ch = chr(ord(ch) - 0x60)
        if ch.isalnum():
            out.append(ch)
    return "".join(out)


@dataclass(frozen=True)
class Utterance:
    pcm: bytes
    ended_at: float
    ended_mono: float
    started_at: float | None = None

    @property
    def samples(self):
        return len(self.pcm) // 2


@dataclass(frozen=True)
class Recognition:
    text: str
    rejected_segments: int = 0


class Segmenter:
    def __init__(self, threshold_db=-42.0, pre_roll_seconds=0.3,
                 silence_seconds=0.7, min_speech_seconds=0.3, max_seconds=8.0):
        self.threshold_db = float(threshold_db)
        self.pre_limit = int(pre_roll_seconds * RATE)
        self.silence_limit = int(silence_seconds * RATE)
        self.minimum = int(min_speech_seconds * RATE)
        self.maximum = int(max_seconds * RATE)
        if not (math.isfinite(self.threshold_db) and self.pre_limit >= 0 and
                self.silence_limit > 0 and self.minimum > 0 and
                self.maximum >= self.pre_limit + self.minimum):
            raise ValueError("invalid whisper segmentation settings")
        self.pre = deque()
        self.pre_samples = 0
        self.parts = []
        self.samples = self.voiced = self.quiet = 0
        self.ended_at = self.ended_mono = 0.0
        self.started_at = 0.0
        self.last_input_mono = None

    def _remember(self, pcm):
        self.pre.append(bytes(pcm))
        self.pre_samples += len(pcm) // 2
        while self.pre_samples > self.pre_limit:
            excess = self.pre_samples - self.pre_limit
            head = self.pre.popleft()
            removed = min(excess, len(head) // 2)
            self.pre_samples -= removed
            if removed * 2 < len(head):
                self.pre.appendleft(head[removed * 2:])

    def _finish(self):
        result = None
        if self.voiced >= self.minimum:
            result = Utterance(b"".join(self.parts), self.ended_at, self.ended_mono, self.started_at)
        self.parts = []
        self.samples = self.voiced = self.quiet = 0
        return result

    def feed(self, pcm, ended_at, ended_mono):
        """Zero or more utterances, with an exact duration cap including pre-roll."""
        if len(pcm) % 2:
            raise ValueError("mono PCM16 must contain complete samples")
        out = []
        input_start = ended_mono - len(pcm) / (RATE * 2)
        if self.last_input_mono is not None and input_start - self.last_input_mono > 0.05:
            # Do not join words from opposite sides of a device restart or
            # a dropped audio block into one apparent continuous call.
            previous = self._finish()
            if previous is not None:
                out.append(previous)
            self.pre.clear()
            self.pre_samples = 0
        self.last_input_mono = ended_mono
        remaining = bytes(pcm)
        while remaining:
            data = np.frombuffer(remaining, dtype="<i2").astype(np.float32) / 32768.0
            rms = float(np.sqrt(np.mean(data * data)))
            loud = 20.0 * math.log10(max(rms, 1e-9)) >= self.threshold_db
            if not self.parts:
                if not loud:
                    self._remember(remaining)
                    break
                self.parts = list(self.pre)
                self.samples = self.pre_samples
                self.started_at = ended_at - (len(remaining) // 2 + self.pre_samples) / RATE
                self.pre.clear()
                self.pre_samples = 0
            take = min(len(remaining) // 2, self.maximum - self.samples)
            part, remaining = remaining[:take * 2], remaining[take * 2:]
            part_at = ended_at - len(remaining) / (RATE * 2)
            part_mono = ended_mono - len(remaining) / (RATE * 2)
            self.parts.append(part)
            self.samples += take
            if loud:
                self.voiced += take
                self.quiet = 0
                self.ended_at, self.ended_mono = part_at, part_mono
            else:
                self.quiet += take
            if self.quiet >= self.silence_limit or self.samples >= self.maximum:
                utterance = self._finish()
                if utterance is not None:
                    out.append(utterance)
                if not loud:
                    self._remember(part)
        return out


class LocalWhisper:
    def __init__(self, model_dir, threads=2, prompt="", model_factory=None):
        if not model_dir or not os.path.isdir(model_dir):
            raise ValueError("SPEECH_WHISPER_MODEL must be an existing local model directory")
        if int(threads) < 1:
            raise ValueError("SPEECH_WHISPER_THREADS must be positive")
        if model_factory is None:
            from faster_whisper import WhisperModel
            model_factory = WhisperModel
        self.model = model_factory(model_dir, device="cpu", compute_type="int8",
                                   cpu_threads=int(threads), num_workers=1,
                                   local_files_only=True)
        self.prompt = prompt.strip()[:MAX_TEXT] or None
        hint = unicodedata.normalize("NFKC", self.prompt or "")
        self.prompt_terms = [normalized_words(term) for term in re.split(r"[\s、,。.;]+", hint)]

    def recognize(self, audio):
        # ndarray input bypasses all file/media decoders and stays in memory.
        samples = np.asarray(audio, dtype=np.float32)
        if samples.ndim != 1:
            raise ValueError("Whisper expects mono samples")
        segments, _ = self.model.transcribe(
            samples, language="ja", beam_size=1, temperature=0,
            condition_on_previous_text=False, vad_filter=False,
            initial_prompt=self.prompt, no_speech_threshold=NO_SPEECH_THRESHOLD,
            log_prob_threshold=MIN_LOGPROB,
        )
        text, rejected = [], 0
        for segment in segments:
            # Whisper's default no-speech skip is conditional on logprob.
            # Reject either failure independently: a name prompt can otherwise
            # hallucinate a confident wake phrase on ambient room noise.
            compression = getattr(segment, "compression_ratio", 0.0)
            if (not math.isfinite(segment.no_speech_prob) or
                    not math.isfinite(segment.avg_logprob) or
                    not math.isfinite(compression) or compression > MAX_COMPRESSION_RATIO or
                    segment.no_speech_prob > NO_SPEECH_THRESHOLD or
                    segment.avg_logprob < MIN_LOGPROB):
                rejected += 1
                continue
            text.append(segment.text)
        result = "".join(text).strip()[:MAX_TEXT]
        if self.prompt:
            content = normalized_words(result)
            for term in sorted(self.prompt_terms, key=len, reverse=True):
                if term:
                    content = content.replace(term, "")
            if len(content) < 3:
                return Recognition("", rejected + len(text))
        return Recognition(result, rejected)


class WhisperBackend:
    def __init__(self, decoder, segmenter=None, on_result=None, on_decode=None,
                 queue_size=1, max_age_seconds=20.0, clock=time.monotonic, log=print):
        if (not 1 <= int(queue_size) <= 4 or
                not math.isfinite(max_age_seconds) or max_age_seconds <= 0):
            raise ValueError("invalid whisper queue settings")
        self.decoder = decoder
        self.segmenter = segmenter or Segmenter()
        self.on_result = on_result or (lambda text, at, mono, started_at: None)
        self.on_decode = on_decode or (lambda samples: None)
        self.pending = queue.Queue(maxsize=int(queue_size))
        self.max_age = float(max_age_seconds)
        self.clock = clock
        self.log = log
        self.stopped = threading.Event()
        self.lock = threading.Lock()
        self.stats = {"segmented_samples": 0, "utterances_submitted": 0,
                      "utterances_dropped": 0, "utterances_stale": 0,
                      "whisper_rejected_segments": 0, "whisper_decode_failures": 0,
                      "whisper_active": 0, "last_segment_at": 0.0,
                      "last_inference_seconds": 0.0}

    def _add(self, **values):
        with self.lock:
            for key, value in values.items():
                self.stats[key] += value

    def diagnostics(self):
        with self.lock:
            result = dict(self.stats)
        result["queued_utterances"] = self.pending.qsize()
        return result

    def submit(self, utterance):
        dropped = 0
        while True:
            try:
                self.pending.put_nowait(utterance)
                break
            except queue.Full:
                try:
                    self.pending.get_nowait()
                    dropped += 1
                except queue.Empty:
                    pass
        self._add(utterances_submitted=1, utterances_dropped=dropped)

    def feed(self, pcm, ended_at, ended_mono):
        for utterance in self.segmenter.feed(pcm, ended_at, ended_mono):
            self.submit(utterance)
        with self.lock:
            self.stats["segmented_samples"] += len(pcm) // 2
            self.stats["last_segment_at"] = ended_at

    def run_once(self, block=True):
        try:
            utterance = self.pending.get(timeout=0.5) if block else self.pending.get_nowait()
        except queue.Empty:
            return False
        if self.clock() - utterance.ended_mono > self.max_age:
            self._add(utterances_stale=1)
            return True
        started = self.clock()
        self._add(whisper_active=1)
        try:
            audio = np.frombuffer(utterance.pcm, dtype="<i2").astype(np.float32) / 32768.0
            result = self.decoder.recognize(audio)
            self.on_decode(utterance.samples)
            self._add(whisper_rejected_segments=result.rejected_segments)
            if self.clock() - utterance.ended_mono > self.max_age:
                self._add(utterances_stale=1)
            elif result.text and not self.stopped.is_set():
                self.on_result(result.text, utterance.ended_at, utterance.ended_mono, utterance.started_at)
        except Exception as exc:
            self._add(whisper_decode_failures=1)
            self.log("speech: whisper decode failed: %s" % type(exc).__name__)
        finally:
            with self.lock:
                self.stats["whisper_active"] = 0
                self.stats["last_inference_seconds"] = round(self.clock() - started, 3)
        return True

    def start(self):
        def work():
            while not self.stopped.is_set():
                self.run_once()
        thread = threading.Thread(target=work, name="whisper-inference", daemon=True)
        thread.start()
        return thread

    def stop(self):
        self.stopped.set()
        while True:
            try:
                self.pending.get_nowait()
            except queue.Empty:
                return
