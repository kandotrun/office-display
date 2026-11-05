import io
import json
import queue
from types import SimpleNamespace

import numpy as np
import pytest

import speech_server as srv


class EndCapture(BaseException):
    pass


def test_capture_delivers_mono_pcm16_to_vosk_and_keeps_normalised_meter(monkeypatch):
    # One real 100 ms capture block: full scale on both sides, a quieter
    # channel, and opposing channels. Downmixing must neither overflow nor
    # double the recogniser's sample count by sending float32 bytes.
    stereo = np.tile(
        np.array([[32767, 32767], [-32768, -32768], [16384, 0], [-16384, 0],
                  [32767, -32767], [1, 1], [-1, -1], [0, 0]], dtype="<i2"),
        (srv.CHUNK // 8, 1),
    )
    expected = np.tile(np.array([32767, -32768, 8192, -8192, 0, 1, -1, 0], dtype="<i2"), srv.CHUNK // 8)
    proc = SimpleNamespace(stdout=io.BytesIO(stereo.tobytes()), kill=lambda: None)
    audio = queue.Queue()
    monkeypatch.setattr(srv.subprocess, "Popen", lambda *args, **kwargs: proc)
    monkeypatch.setattr(srv, "QUEUE", audio)
    monkeypatch.setattr(srv, "STATE", {**srv.STATE, "phrases": []})

    def finish(*args):
        raise EndCapture

    monkeypatch.setattr(srv.time, "sleep", finish)
    with pytest.raises(EndCapture):
        srv.capture()
    pcm = audio.get_nowait()
    assert audio.empty()
    assert pcm == expected.tobytes()
    assert len(pcm) == srv.CHUNK * 2
    normalised = expected.astype(np.float32) / 32768.0
    assert srv.STATE["db"] == pytest.approx(20 * np.log10(np.sqrt(np.mean(normalised ** 2))))

    # Exercise the consumer too: the bytes above reach the byte-input Vosk
    # interface unchanged, at the capture rate, and publish a final phrase.
    calls = []

    class Recognizer:
        def __init__(self, model, rate):
            assert model == "test-model" and rate == srv.RATE

        def SetWords(self, enabled):
            pass

        def AcceptWaveform(self, data):
            calls.append(data)
            return True

        def Result(self):
            return json.dumps({"text": "エンゼル ヒール なら どう 思う"})

    chunks = iter([pcm])

    def next_chunk():
        try:
            return next(chunks)
        except StopIteration:
            raise EndCapture

    monkeypatch.setattr(audio, "get", next_chunk)
    monkeypatch.setattr(srv.vosk, "Model", lambda path: "test-model", raising=False)
    monkeypatch.setattr(srv.vosk, "KaldiRecognizer", Recognizer, raising=False)
    with pytest.raises(EndCapture):
        srv.transcribe()
    assert calls == [expected.tobytes()]
    assert srv.STATE["phrases"][0]["text"] == "エンゼル ヒール なら どう 思う"
