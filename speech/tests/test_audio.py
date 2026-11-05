import json
import queue
from types import SimpleNamespace

import numpy as np
import pytest

import speech_server as srv


class EndCapture(BaseException):
    pass


class FragmentedStream:
    def __init__(self, data):
        self.data = data
        self.offset = 0
        self.reads = 0

    def read(self, size):
        # Pipe reads can end within a sample or between the stereo channels.
        sizes = (1, 2, 5, 17, 4093)
        count = min(size, sizes[self.reads % len(sizes)])
        self.reads += 1
        chunk = self.data[self.offset:self.offset + count]
        self.offset += len(chunk)
        return chunk


@pytest.fixture
def counters(monkeypatch):
    state = {**srv.STATE, "capture_samples": 0, "decoded_samples": 0,
             "dropped_chunks": 0, "dropped_samples": 0,
             "discarded_capture_bytes": 0, "last_decode_at": 0.0}
    monkeypatch.setattr(srv, "STATE", state)
    return state


def test_fragmented_reads_preserve_complete_audio_and_stable_blocks(counters):
    raw = np.arange(srv.CHUNK * 4 + 6, dtype="<i2").tobytes() + b"\x01\x02"
    blocks = list(srv.capture_blocks(FragmentedStream(raw)))
    assert [len(block) for block in blocks] == [srv.CHUNK * 4, srv.CHUNK * 4, 12]
    assert b"".join(blocks) == raw[:-2]
    assert counters["discarded_capture_bytes"] == 2


def test_read_failure_retains_complete_frames_and_counts_incomplete_bytes(counters):
    class FailedStream:
        def __init__(self):
            self.reads = 0

        def read(self, size):
            self.reads += 1
            if self.reads == 1:
                return b"\x01\x00\x02\x00\x03\x00"
            raise OSError("capture ended")

    assert list(srv.capture_blocks(FailedStream())) == [b"\x01\x00\x02\x00"]
    assert counters["discarded_capture_bytes"] == 2


def test_full_queue_evicts_old_audio_and_retains_current_block(monkeypatch, counters):
    audio = srv.AudioQueue(maxsize=1)
    monkeypatch.setattr(srv, "QUEUE", audio)
    old = np.full(srv.CHUNK, 1000, dtype="<i2").tobytes()
    newest = np.full(srv.CHUNK, 2000, dtype="<i2").tobytes()
    srv.queue_audio(old)
    srv.queue_audio(newest)
    assert audio.milliseconds() == 100.0
    assert counters["capture_samples"] == srv.CHUNK * 2
    assert counters["dropped_samples"] == srv.CHUNK
    assert counters["dropped_chunks"] == 1
    assert audio.get_nowait() == newest
    assert audio.milliseconds() == 0.0


def test_current_audio_is_retained_if_decoder_drains_queue_during_eviction(monkeypatch, counters):
    class DrainedQueue:
        def __init__(self):
            self.calls = 0
            self.data = None

        def put_nowait(self, data):
            self.calls += 1
            if self.calls == 1:
                raise queue.Full
            self.data = data

        def get_nowait(self):
            raise queue.Empty

    audio = DrainedQueue()
    monkeypatch.setattr(srv, "QUEUE", audio)
    srv.queue_audio(b"\x01\x00\x02\x00")
    assert audio.data == b"\x01\x00\x02\x00"
    assert counters["capture_samples"] == 2
    assert counters["dropped_chunks"] == 0
    assert counters["dropped_samples"] == 0


def test_capture_delivers_mono_pcm16_to_vosk_and_keeps_normalised_meter(monkeypatch, counters):
    # One real 100 ms capture block: full scale on both sides, a quieter
    # channel, and opposing channels. Downmixing must neither overflow nor
    # double the recogniser's sample count by sending float32 bytes.
    stereo = np.tile(
        np.array([[32767, 32767], [-32768, -32768], [16384, 0], [-16384, 0],
                  [32767, -32767], [1, 1], [-1, -1], [0, 0]], dtype="<i2"),
        (srv.CHUNK // 8, 1),
    )
    expected = np.tile(np.array([32767, -32768, 8192, -8192, 0, 1, -1, 0], dtype="<i2"), srv.CHUNK // 8)
    proc = SimpleNamespace(stdout=FragmentedStream(stereo.tobytes()), kill=lambda: None)
    audio = queue.Queue()
    monkeypatch.setattr(srv.subprocess, "Popen", lambda *args, **kwargs: proc)
    monkeypatch.setattr(srv, "QUEUE", audio)
    monkeypatch.setitem(counters, "phrases", [])

    def finish(*args):
        raise EndCapture

    monkeypatch.setattr(srv.time, "sleep", finish)
    with pytest.raises(EndCapture):
        srv.capture()
    pcm = audio.get_nowait()
    assert audio.empty()
    assert pcm == expected.tobytes()
    assert len(pcm) == srv.CHUNK * 2
    assert counters["capture_samples"] == srv.CHUNK
    assert counters["decoded_samples"] == 0
    assert counters["last_decode_at"] == 0.0
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
    monkeypatch.setattr(srv, "vosk", SimpleNamespace(Model=lambda path: "test-model", KaldiRecognizer=Recognizer))
    with pytest.raises(EndCapture):
        srv.transcribe()
    assert calls == [expected.tobytes()]
    assert counters["decoded_samples"] == srv.CHUNK
    assert counters["last_decode_at"] > 0
    assert srv.STATE["phrases"][0]["text"] == "エンゼル ヒール なら どう 思う"
