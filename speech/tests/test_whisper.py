import subprocess
import sys
import threading
from types import SimpleNamespace

import numpy as np
import pytest

import speech_server as srv
import whisper_backend as wb


def pcm(value=10000, samples=1600):
    return np.full(samples, value, dtype="<i2").tobytes()


def feed(segmenter, values):
    out = []
    for i, value in enumerate(values, 1):
        out += segmenter.feed(pcm(value), 1000 + i / 10, 100 + i / 10)
    return out


def test_segmenter_preserves_pre_roll_and_captured_speech_end():
    segmenter = wb.Segmenter(threshold_db=-30, pre_roll_seconds=.2,
                             silence_seconds=.2, min_speech_seconds=.2, max_seconds=1)
    (utterance,) = feed(segmenter, [0, 0, 0, 10000, 10000, 0, 0])
    assert utterance.pcm == pcm(0) * 2 + pcm(10000) * 2 + pcm(0) * 2
    assert utterance.ended_at == 1000.5
    assert utterance.ended_mono == 100.5
    assert utterance.started_at == pytest.approx(1000.1)
    assert utterance.samples == 9600


def test_silence_and_short_noise_do_not_create_decode_jobs():
    segmenter = wb.Segmenter(pre_roll_seconds=.2, silence_seconds=.2,
                             min_speech_seconds=.2, max_seconds=1)
    assert feed(segmenter, [0] * 30 + [10000, 0, 0] + [0] * 30) == []
    assert segmenter.pre_samples <= 3200
    assert segmenter.samples == 0


def test_capture_gap_finishes_old_words_and_drops_pre_gap_context():
    segmenter = wb.Segmenter(pre_roll_seconds=.2, silence_seconds=.2,
                             min_speech_seconds=.2, max_seconds=8)
    assert segmenter.feed(pcm(), 1000.1, 100.1) == []
    assert segmenter.feed(pcm(), 1000.2, 100.2) == []
    (before_gap,) = segmenter.feed(pcm(), 1100.1, 200.1)
    assert before_gap.ended_at == 1000.2
    assert before_gap.pcm == pcm() * 2
    assert segmenter.feed(pcm(), 1100.2, 200.2) == []
    assert segmenter.feed(pcm(0), 1100.3, 200.3) == []
    (after_gap,) = segmenter.feed(pcm(0), 1100.4, 200.4)
    assert after_gap.started_at == pytest.approx(1100.0)
    assert after_gap.pcm == pcm() * 2 + pcm(0) * 2


def test_long_speech_is_split_at_the_exact_bounded_duration():
    segmenter = wb.Segmenter(pre_roll_seconds=.1, silence_seconds=.2,
                             min_speech_seconds=.1, max_seconds=.4)
    utterances = feed(segmenter, [0] + [10000] * 10 + [0, 0])
    assert len(utterances) == 3
    assert all(u.samples <= 6400 for u in utterances)
    assert sum(u.samples for u in utterances) == 19200
    assert [u.ended_at for u in utterances] == sorted(u.ended_at for u in utterances)


def make_decoder(tmp_path, segments, prompt=""):
    calls = []

    class Model:
        def __init__(self, path, **kwargs):
            calls.append((path, kwargs))

        def transcribe(self, audio, **kwargs):
            calls.append((audio, kwargs))
            return iter(segments), None

    decoder = wb.LocalWhisper(str(tmp_path), prompt=prompt, model_factory=Model)
    return decoder, calls


def test_local_decoder_uses_numpy_and_rejects_prompted_ambient_hallucination(tmp_path):
    segments = [
        SimpleNamespace(text="エンデルヒールなら、休もう。", no_speech_prob=.16, avg_logprob=-.74),
        # Physical ambient-tail case: logprob alone looks good, but the
        # no-speech probability must independently prevent a false wake.
        SimpleNamespace(text="エンデルヒール", no_speech_prob=.77, avg_logprob=-.56),
        SimpleNamespace(text="不確かな声", no_speech_prob=.47, avg_logprob=-1.37),
        SimpleNamespace(text="invalid", no_speech_prob=float("nan"), avg_logprob=-.1),
    ]
    decoder, calls = make_decoder(tmp_path, segments, prompt="エンデルヒール、Earendil")
    result = decoder.recognize(np.zeros(16000, dtype=np.float32))
    assert result == wb.Recognition("エンデルヒールなら、休もう。", 3)
    path, settings = calls[0]
    assert path == str(tmp_path)
    assert settings == {"device": "cpu", "compute_type": "int8", "cpu_threads": 2,
                        "num_workers": 1, "local_files_only": True}
    audio, options = calls[1]
    assert isinstance(audio, np.ndarray) and audio.dtype == np.float32 and audio.ndim == 1
    assert options == {"language": "ja", "beam_size": 1, "temperature": 0,
                       "condition_on_previous_text": False, "vad_filter": False,
                       "initial_prompt": "エンデルヒール、Earendil",
                       "no_speech_threshold": .6, "log_prob_threshold": -1.0}


def test_model_is_explicitly_local_and_optional_prompt_defaults_empty(tmp_path):
    decoder, calls = make_decoder(tmp_path, [])
    decoder.recognize(np.zeros(1600, dtype=np.float32))
    assert calls[1][1]["initial_prompt"] is None
    for path in ("", "Systran/faster-whisper-small"):
        with pytest.raises(ValueError, match="existing local model directory"):
            wb.LocalWhisper(path, model_factory=lambda *a, **kw: pytest.fail("must not download"))


@pytest.mark.parametrize("text,compression", [("エンデルヒール、エンゼルヒール。", 1.1), ("エンデルヒール" * 30, 5.0)])
def test_name_hints_and_repetitions_cannot_become_a_spoken_utterance(tmp_path, text, compression):
    decoder, _ = make_decoder(tmp_path, [SimpleNamespace(text=text, no_speech_prob=.1,
        avg_logprob=-.2, compression_ratio=compression)], prompt="エンデルヒール、エンゼルヒール")
    assert decoder.recognize(np.zeros(1600, dtype=np.float32)) == wb.Recognition("", 1)


@pytest.mark.parametrize("text,prompt", [
    ("えんでるひーる", "エンデルヒール"),
    ("エンゼルヒール", "エンデルヒール エンゼルヒール"),
    ("エンゼルヒール", "エンデルヒール，エンゼルヒール；エアレンディル"),
])
def test_name_hint_filter_handles_kana_and_vocabulary_separators(tmp_path, text, prompt):
    decoder, _ = make_decoder(tmp_path, [SimpleNamespace(text=text, no_speech_prob=.1,
        avg_logprob=-.2)], prompt=prompt)
    assert decoder.recognize(np.zeros(1600, dtype=np.float32)) == wb.Recognition("", 1)


def test_genuine_question_repeated_twice_is_retained_as_one_recognition(tmp_path):
    text = "エンデルヒールなら休むのは大事？" * 2
    decoder, _ = make_decoder(tmp_path, [SimpleNamespace(text=text, no_speech_prob=.1,
        avg_logprob=-.2, compression_ratio=1.8)], prompt="エンデルヒール")
    assert decoder.recognize(np.zeros(1600, dtype=np.float32)).text == text


def test_delayed_result_keeps_capture_timestamp_and_monotonic_caption_expiry(monkeypatch):
    now = [15.0]
    phrases = srv.Phrases()
    monkeypatch.setattr(srv, "STATE", {**srv.STATE, "phrases": [], "text": ""})
    monkeypatch.setattr(srv.time, "monotonic", lambda: now[0])

    class Decoder:
        def recognize(self, audio):
            now[0] = 19.0  # inference completes 9 s after the captured speech
            return wb.Recognition("エンデルヒールなら休もう")

    backend = wb.WhisperBackend(Decoder(), clock=lambda: now[0],
        on_result=lambda text, at, mono, start: srv.publish_phrases(phrases, text, at, mono, start))
    backend.submit(wb.Utterance(pcm(), ended_at=1000.0, ended_mono=10.0, started_at=998.0))
    assert backend.run_once(block=False)
    assert srv.STATE["phrases"][0]["at"] == 1000.0
    assert srv.STATE["phrases"][0]["started_at"] == 998.0
    assert phrases.items[0][2] == 10.0
    assert srv.STATE["text"] == "エンデルヒールなら休もう"
    now[0] = 36.0
    srv.publish_phrases(phrases)
    assert srv.STATE["text"] == ""  # expires from capture, not completion


def test_pending_queue_keeps_only_latest_and_explicitly_drops_stale_jobs():
    results, decodes = [], []
    now = [5.0]
    decoder = SimpleNamespace(recognize=lambda audio: decodes.append(len(audio)) or wb.Recognition("ok"))
    backend = wb.WhisperBackend(decoder, queue_size=1, max_age_seconds=10,
                               clock=lambda: now[0], on_result=lambda *args: results.append(args))
    for i in range(3):
        backend.submit(wb.Utterance(pcm(), 1000.0 + i, float(i)))
    assert backend.diagnostics()["utterances_dropped"] == 2
    assert backend.run_once(block=False)
    assert results == [("ok", 1002.0, 2.0, None)]
    assert not backend.run_once(block=False)
    backend.submit(wb.Utterance(pcm(), 1000.0, 0.0))
    now[0] = 11.0
    assert backend.run_once(block=False)
    assert backend.diagnostics()["utterances_stale"] == 1
    assert decodes == [1600]


def test_result_that_becomes_stale_during_decode_is_not_published():
    now = [2.0]
    results, counts = [], []

    class Decoder:
        def recognize(self, audio):
            now[0] = 25.0
            return wb.Recognition("old result")

    backend = wb.WhisperBackend(Decoder(), max_age_seconds=20, clock=lambda: now[0],
                               on_result=lambda *args: results.append(args), on_decode=counts.append)
    backend.submit(wb.Utterance(pcm(), 1000.0, 0.0))
    backend.run_once(block=False)
    assert not results
    assert counts == [1600]
    assert backend.diagnostics()["utterances_stale"] == 1


def test_segmentation_keeps_draining_while_inference_is_busy():
    started, release, completed = threading.Event(), threading.Event(), threading.Event()
    results, decodes = [], []

    class Decoder:
        def recognize(self, audio):
            decodes.append(len(audio))
            if len(decodes) == 1:
                started.set()
                assert release.wait(2)
            return wb.Recognition("ok")

    def result(*args):
        results.append(args)
        if len(results) == 2:
            completed.set()

    backend = wb.WhisperBackend(Decoder(),
        segmenter=wb.Segmenter(pre_roll_seconds=0, min_speech_seconds=.1,
                               silence_seconds=.1, max_seconds=1),
        clock=lambda: 105.0, on_result=result)
    backend.submit(wb.Utterance(pcm(), 1000.0, 100.0))
    thread = backend.start()
    try:
        assert started.wait(2)
        for i, value in enumerate([10000, 0] * 3, 1):
            backend.feed(pcm(value), 1000 + i / 10, 100 + i / 10)
        stats = backend.diagnostics()
        assert stats["segmented_samples"] == 9600
        assert stats["queued_utterances"] == 1
        assert stats["utterances_dropped"] == 2
        assert len(decodes) == 1
        release.set()
        assert completed.wait(2)
    finally:
        release.set()
        backend.stop()
        thread.join(2)
    assert not thread.is_alive()
    assert len(results) == 2
    assert results[-1][1:] == (1000.5, 100.5, pytest.approx(1000.4))


def test_decode_failures_never_log_transcript_or_exception_text():
    logs = []

    class Decoder:
        def recognize(self, audio):
            raise RuntimeError("秘密の書き起こしや音声")

    backend = wb.WhisperBackend(Decoder(), clock=lambda: 1.0, log=logs.append)
    backend.submit(wb.Utterance(pcm(), 1000.0, 0.0))
    backend.run_once(block=False)
    assert logs == ["speech: whisper decode failed: RuntimeError"]
    assert backend.diagnostics()["whisper_decode_failures"] == 1


def test_default_backend_import_needs_neither_native_model_library():
    result = subprocess.run([sys.executable, "-c", "import speech_server; print(speech_server.BACKEND)"],
                            cwd="speech", capture_output=True, text=True, check=True,
                            env={"PATH": "/usr/bin:/bin"})
    assert result.stdout.strip() == "vosk"


@pytest.mark.parametrize("name", ["vosk", "whisper"])
def test_selected_model_loads_before_any_capture_starts(monkeypatch, name):
    events = []
    monkeypatch.setattr(srv, "BACKEND", name)
    monkeypatch.setattr(srv, "load_vosk", lambda: events.append("model-ready") or object())
    fake_backend = SimpleNamespace(start=lambda: events.append("inference-started"))
    monkeypatch.setattr(srv, "load_whisper", lambda: events.append("model-ready") or (fake_backend, srv.Phrases()))

    class Thread:
        def __init__(self, target, **kwargs):
            self.target = target

        def start(self):
            events.append(self.target.__name__)

    class Finished(BaseException):
        pass

    class Server:
        def __init__(self, *args):
            pass

        def serve_forever(self):
            raise Finished

    monkeypatch.setattr(srv.threading, "Thread", Thread)
    monkeypatch.setattr(srv, "ThreadingHTTPServer", Server)
    monkeypatch.setattr(srv, "WHISPER", None)
    with pytest.raises(Finished):
        srv.main()
    assert events[0] == "model-ready"
    assert events.index("model-ready") < events.index("capture")
