"""Real-shaped fixtures and stand-ins for the network, the Anthropic client and audio."""
import math
import os
import stat
import struct
import wave
from types import SimpleNamespace

import voice

# The real emblem is a single path (fine as a fixture; it is not shipped).
EARENDIL_D = (
    "M202.89,123.89c.4,29.74-40.27,26.97-40.8,48.11h-.05c-.52-21.14-41.19-18.38-40.8-48.11,"
    "27.99,0,40.09-46.29,40.82-101.27.74,54.98,12.83,101.27,40.82,101.27h0Z"
)
EARENDIL_SVG = (
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="120 21 84 153" width="84" height="153" '
    'aria-hidden="true" focusable="false">\n  <defs>\n    <style>.st0 { fill: #fff; }</style>\n  </defs>\n'
    '  <path class="st0" d="%s"/>\n</svg>\n' % EARENDIL_D
).encode()


def final(text=None, stop_reason="end_turn", content=None):
    """A final message as stream.get_final_message() returns it."""
    if content is None:
        content = [SimpleNamespace(type="thinking", thinking="", signature="sig")]
        if text is not None:
            content.append(SimpleNamespace(type="text", text=text))
    return SimpleNamespace(stop_reason=stop_reason, content=content)


class FakeStream:
    def __init__(self, chunks, message):
        self.chunks = chunks
        self.message = message

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False

    @property
    def text_stream(self):
        return iter(self.chunks)

    def get_final_message(self):
        return self.message


class FakeClient:
    """client.beta.messages.stream(**kwargs) streaming canned text; records the calls."""

    def __init__(self, chunks=(), message=None, error=None):
        self.calls = []
        self.chunks = list(chunks)
        self.message = message if message is not None else final("".join(self.chunks))
        self.error = error
        self.beta = SimpleNamespace(messages=SimpleNamespace(stream=self.stream))

    def stream(self, **kwargs):
        self.calls.append(kwargs)
        if self.error is not None:
            raise self.error
        return FakeStream(self.chunks, self.message)


# -- audio ------------------------------------------------------------------------------


def tone(seconds, amplitude, rate=8000, freq=400):
    return [int(amplitude * math.sin(2 * math.pi * freq * i / rate)) for i in range(int(seconds * rate))]


def write_wav(path, samples, rate=8000, channels=1):
    with wave.open(str(path), "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(struct.pack("<%dh" % len(samples), *samples))


def wav_bytes(tmp_path, samples, **kw):
    path = tmp_path / "x.wav"
    write_wav(path, samples, **kw)
    return path.read_bytes()


class FakeBackend:
    """A synthesiser that writes a canned WAV (or fails for some sentences)."""

    name = "fake"

    def __init__(self, wav, fail=()):
        self.wav = wav
        self.fail = set(fail)
        self.texts = []

    def synth(self, text, path):
        self.texts.append(text)
        if text in self.fail:
            raise voice.VoiceError("synth failed")
        with open(path, "wb") as f:
            f.write(self.wav)
        return True

    def credit(self):
        return "FAKE"


class Recorder:
    """A player that records what it was given (and the file mode at the time)."""

    def __init__(self, fail=False):
        self.played = []
        self.modes = []
        self.fail = fail

    def __call__(self, path):
        self.played.append(path)
        self.modes.append(stat.S_IMODE(os.stat(path).st_mode))
        if self.fail:
            raise voice.VoiceError("aplay failed")
