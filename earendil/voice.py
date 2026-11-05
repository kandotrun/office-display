"""Speaking the answer: pluggable text-to-speech, playback and the loudness envelope.

Backends (EARENDIL_TTS):

* voicevox  - a VOICEVOX engine over HTTP (POST /audio_query, POST /synthesis);
              its terms require the credit line "VOICEVOX:<speaker>".
* openjtalk - the open_jtalk CLI (the sentence on stdin, a WAV out).
* say       - macOS `say` (development).
* none      - subtitles only.
* auto      - voicevox if its /version answers within 1 s, else open_jtalk if
              installed, else say on macOS, else none; keeps looking for
              VOICEVOX every few minutes until it is found.

Sentences are queued and spoken in order by one worker thread. Each one is
synthesised into a 0600 temporary WAV in the runtime directory, measured
(RMS per 50 ms, 0..1), played with aplay (Linux) or afplay (macOS) and deleted
right away, on errors too; stop() (at exit) deletes the one in flight, and
start() sweeps away any earendil-*.wav an abrupt exit left behind.

earendil never writes the text anywhere. It reaches the synthesiser on stdin
or, for VOICEVOX, over loopback HTTP only: EARENDIL_VOICEVOX_URL must name
this machine (else voicevox is refused and auto skips it). VOICEVOX takes the
sentence in the query string of POST /audio_query, and the stock engine runs
uvicorn with its access log on, which prints every request line, sentence
included, to its stdout. Run the engine with that output discarded (systemd
StandardOutput=null or LogLevelMax=warning, docker --log-driver none);
nothing on this side can prevent it. Failures are logged with the backend
name and exception class only.
"""
import array
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import wave

import net

STEP = 0.05
MAX_LEVELS = 1200
SYNTH_TIMEOUT = 30.0
PLAY_TIMEOUT = 90.0
PROBE_TIMEOUT = 1.0
REPROBE_EVERY = 300.0
MAX_WAV_BYTES = 32 << 20

VOICEVOX_URL = "http://127.0.0.1:50021"
VOICEVOX_SPEAKER = 13  # 青山龍星 ノーマル
OPENJTALK_DIC = "/var/lib/mecab/dic/open-jtalk/naist-jdic"
OPENJTALK_VOICE = "/usr/share/hts-voice/nitech-jp-atr503-m001/nitech_jp_atr503_m001.htsvoice"


class VoiceError(Exception):
    pass


def run(args, input=None, timeout=None):
    """Run a command quietly; its exit code."""
    return subprocess.run(
        args,
        input=input,
        stdin=None if input is not None else subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=timeout,
        check=False,
    ).returncode


def runtime_dir():
    """Where audio lives while it plays: EARENDIL_RUNTIME_DIR, systemd's RuntimeDirectory, or /tmp."""
    configured = os.environ.get("EARENDIL_RUNTIME_DIR") or os.environ.get("RUNTIME_DIRECTORY", "").split(":")[0]
    return configured or tempfile.gettempdir()


# -- the loudness envelope -----------------------------------------------------------


def envelope(path, step=STEP, max_levels=MAX_LEVELS):
    """(seconds per level, RMS levels normalised to 0..1) of a PCM WAV; (step, []) if unreadable."""
    try:
        with wave.open(path, "rb") as w:
            channels, width, rate, count = w.getnchannels(), w.getsampwidth(), w.getframerate(), w.getnframes()
            frames = w.readframes(count)
    except (wave.Error, EOFError, OSError):
        return step, []
    codes = {1: "B", 2: "h", 4: "i"}
    if width not in codes or rate <= 0 or channels <= 0:
        return step, []
    samples = array.array(codes[width])
    samples.frombytes(frames[: len(frames) - len(frames) % width])
    if width > 1 and sys.byteorder == "big":
        samples.byteswap()
    offset = 128 if width == 1 else 0  # 8-bit WAV is unsigned
    count = len(samples) // channels
    window = max(1, int(round(rate * step)))
    if math.ceil(count / window) > max_levels:
        window = math.ceil(count / max_levels)
    rms = []
    for start in range(0, count, window):
        chunk = samples[start * channels : (start + window) * channels]
        rms.append(math.sqrt(sum((v - offset) * (v - offset) for v in chunk) / len(chunk)))
    peak = max(rms, default=0.0)
    levels = [round(v / peak, 3) if peak > 0 else 0.0 for v in rms]
    return round(window / rate, 4), levels


# -- backends ----------------------------------------------------------------------------


class NoVoice:
    name = "none"

    def synth(self, text, path):
        return False

    def credit(self):
        return ""


class Voicevox:
    name = "voicevox"

    def __init__(self, url=VOICEVOX_URL, speaker=VOICEVOX_SPEAKER, fetch=net.fetch_local):
        self.url = url.rstrip("/")
        self.speaker = int(speaker)
        self.fetch = fetch
        self._credit = None

    def available(self):
        try:
            self.fetch(self.url + "/version", timeout=PROBE_TIMEOUT, max_bytes=4096)
        except net.FetchError:
            return False
        return True

    def synth(self, text, path):
        query = self.fetch(
            "%s/audio_query?%s" % (self.url, urllib.parse.urlencode({"text": text, "speaker": self.speaker})),
            timeout=SYNTH_TIMEOUT,
            max_bytes=1 << 20,
            data=b"",
        )
        audio = self.fetch(
            "%s/synthesis?speaker=%d" % (self.url, self.speaker),
            timeout=SYNTH_TIMEOUT,
            max_bytes=MAX_WAV_BYTES,
            data=query,
            content_type="application/json",
        )
        with open(path, "wb") as f:
            f.write(audio)
        return True

    def credit(self):
        """VOICEVOX:<speaker name>, or VOICEVOX while /speakers cannot tell."""
        if self._credit is None:
            try:
                speakers = json.loads(self.fetch(self.url + "/speakers", timeout=2.0, max_bytes=4 << 20))
                for entry in speakers:
                    if any(style.get("id") == self.speaker for style in entry.get("styles", [])):
                        self._credit = ("VOICEVOX:%s" % entry["name"])[:64]
                        break
            except (net.FetchError, ValueError, TypeError, AttributeError, KeyError):
                pass
        return self._credit or "VOICEVOX"


class _Command:
    """A synthesiser CLI that reads the sentence on stdin and writes a WAV file."""

    def __init__(self, runner=run):
        self.runner = runner

    def args(self, path):
        raise NotImplementedError

    def synth(self, text, path):
        code = self.runner(self.args(path), input=text.encode("utf-8"), timeout=SYNTH_TIMEOUT)
        if code != 0:
            raise VoiceError("%s exited with %s" % (self.name, code))
        return True

    def credit(self):
        return ""


class OpenJTalk(_Command):
    name = "openjtalk"

    def __init__(self, dic=OPENJTALK_DIC, htsvoice=OPENJTALK_VOICE, runner=run):
        super().__init__(runner)
        self.dic = dic
        self.htsvoice = htsvoice

    def args(self, path):
        return ["open_jtalk", "-x", self.dic, "-m", self.htsvoice, "-ow", path]


class Say(_Command):
    name = "say"

    def args(self, path):
        return ["say", "-v", "Kyoko", "--file-format=WAVE", "--data-format=LEI16@22050", "-o", path]


def choose_backend(
    mode,
    voicevox_url=VOICEVOX_URL,
    speaker=VOICEVOX_SPEAKER,
    dic=OPENJTALK_DIC,
    htsvoice=OPENJTALK_VOICE,
    fetch=net.fetch_local,
    runner=run,
    which=shutil.which,
    platform=sys.platform,
):
    """The backend for EARENDIL_TTS (auto probes once; wrap it in Auto to keep looking)."""
    local = net.is_loopback(voicevox_url)
    if mode == "voicevox" and not local:
        raise ValueError("EARENDIL_VOICEVOX_URL must be on this machine (127.0.0.1, ::1 or localhost)")
    voicevox = Voicevox(voicevox_url, speaker, fetch)
    if mode == "voicevox" or (mode == "auto" and local and voicevox.available()):
        return voicevox
    if mode == "openjtalk" or (mode == "auto" and which("open_jtalk")):
        return OpenJTalk(dic, htsvoice, runner)
    if mode == "say" or (mode == "auto" and platform == "darwin" and which("say")):
        return Say(runner)
    if mode in ("none", "auto"):
        return NoVoice()
    raise ValueError("EARENDIL_TTS must be auto, voicevox, openjtalk, say or none, not %r" % mode)


class Auto:
    """EARENDIL_TTS=auto: re-chosen every few minutes until VOICEVOX turns up."""

    def __init__(self, choose, clock=time.monotonic, every=REPROBE_EVERY):
        self.choose = choose
        self.clock = clock
        self.every = every
        self.backend = None
        self.checked = None

    @property
    def name(self):
        return self.backend.name if self.backend is not None else "none"

    def current(self):
        now = self.clock()
        stale = self.checked is None or now - self.checked >= self.every
        if self.backend is None or (self.backend.name != "voicevox" and stale):
            self.backend = self.choose()
            self.checked = now
        return self.backend

    def synth(self, text, path):
        return self.current().synth(text, path)

    def credit(self):
        return self.current().credit()


# -- playback ---------------------------------------------------------------------------------


def player_command(path, platform=sys.platform, device="default"):
    if platform == "darwin":
        return ["afplay", path]
    return ["aplay", "-q", "-D", device, path]


class Player:
    def __init__(self, device="default", runner=run, platform=sys.platform):
        self.device = device
        self.runner = runner
        self.platform = platform

    def __call__(self, path):
        code = self.runner(player_command(path, self.platform, self.device), timeout=PLAY_TIMEOUT)
        if code != 0:
            raise VoiceError("player exited with %s" % code)


class Speaker:
    """The sentence queue: one worker synthesises and plays sentences in order.

    on_start(conv, started, step, levels) fires as a sentence starts playing;
    on_finish(conv) fires for every sentence taken from the queue, played or not.
    """

    def __init__(self, backend, play, directory, on_start=None, on_finish=None, clock=time.time, log=print):
        self.backend = backend
        self.play = play
        self.directory = directory
        self.on_start = on_start
        self.on_finish = on_finish
        self.clock = clock
        self.log = log
        self.cond = threading.Condition()
        self.queue = []  # (conversation, sentence)
        self.current = None  # the WAV of the sentence in flight
        self.stopped = False
        self._credit = ""
        self.failing = False

    def backend_name(self):
        return self.backend.name

    def credit(self):
        return self._credit

    def refresh_credit(self):
        try:
            self._credit = self.backend.credit()
        except Exception as exc:
            self.log("earendil: voice credit failed: %s" % type(exc).__name__)

    def say(self, conv, text):
        with self.cond:
            self.queue.append((conv, text))
            self.cond.notify()

    def clear(self):
        """Drop every queued sentence (not the one playing); how many were dropped."""
        with self.cond:
            dropped = len(self.queue)
            self.queue.clear()
            return dropped

    def _take(self, block):
        with self.cond:
            while block and not self.queue and not self.stopped:
                self.cond.wait()
            return self.queue.pop(0) if self.queue and not self.stopped else None

    def _new_file(self):
        """A fresh 0600 WAV path, remembered for stop(); None once stopped."""
        with self.cond:
            if self.stopped:
                return None
            os.makedirs(self.directory, mode=0o700, exist_ok=True)
            fd, self.current = tempfile.mkstemp(dir=self.directory, prefix="earendil-", suffix=".wav")
            os.close(fd)
            return self.current

    def _done_with(self, path):
        with self.cond:
            if self.current == path:
                self.current = None
        _unlink(path)

    def sweep(self):
        """Delete audio an abrupt exit left behind (the worker is a daemon thread)."""
        try:
            names = os.listdir(self.directory)
        except OSError:
            return
        for name in names:
            if name.startswith("earendil-") and name.endswith(".wav"):
                _unlink(os.path.join(self.directory, name))

    def stop(self):
        """At exit: speak nothing more and delete the audio in flight.

        Daemon threads are stopped without running their finally blocks, so
        the main thread calls this on SIGTERM and Ctrl-C.
        """
        with self.cond:
            self.stopped = True
            self.queue.clear()
            path, self.current = self.current, None
            self.cond.notify_all()
        if path is not None:
            _unlink(path)

    def run_once(self, block=True):
        """Speak the next queued sentence; False when there was none (block=False) or stopped."""
        item = self._take(block)
        if item is None:
            return False
        conv, text = item
        path = None
        try:
            path = self._new_file()
            if path is not None and self.backend.synth(text, path):
                os.chmod(path, 0o600)  # in case the synthesiser replaced the file
                step, levels = envelope(path)
                if self.on_start is not None:
                    self.on_start(conv, self.clock(), step, levels)
                self.play(path)
            if self.failing:
                self.log("earendil: voice %s recovered" % self.backend_name())
            self.failing = False
        except Exception as exc:
            if not self.failing:
                self.log("earendil: voice %s failed: %s" % (self.backend_name(), type(exc).__name__))
            self.failing = True
        finally:
            if path is not None:
                self._done_with(path)
            self.refresh_credit()
            if self.on_finish is not None:
                self.on_finish(conv)
        return True

    def start(self):
        self.sweep()

        def loop():
            self.refresh_credit()
            self.log("earendil: voice %s" % self.backend_name())
            while not self.stopped:
                self.run_once()

        thread = threading.Thread(target=loop, name="voice", daemon=True)
        thread.start()
        return thread


def _unlink(path):
    try:
        os.unlink(path)
    except OSError:
        pass
