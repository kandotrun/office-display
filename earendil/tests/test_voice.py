import json
import os
import threading
from urllib.parse import parse_qs, urlparse

import pytest

import net
import voice
from fakes import FakeBackend, Recorder, tone, wav_bytes, write_wav


# -- the loudness envelope -------------------------------------------------------------


def test_envelope_is_rms_per_50_ms_normalised(tmp_path):
    path = tmp_path / "s.wav"
    write_wav(path, [0] * 800 + tone(0.1, 16000) + tone(0.05, 8000))
    step, levels = voice.envelope(str(path))
    assert step == 0.05
    assert levels == pytest.approx([0.0, 0.0, 1.0, 1.0, 0.5], abs=0.01)


def test_envelope_counts_every_channel(tmp_path):
    path = tmp_path / "s.wav"
    left = tone(0.1, 10000)
    write_wav(path, [v for s in left for v in (s, 0)], channels=2)
    step, levels = voice.envelope(str(path))
    assert step == 0.05 and levels == pytest.approx([1.0, 1.0], abs=0.01)


def test_envelope_of_silence_is_flat(tmp_path):
    path = tmp_path / "s.wav"
    write_wav(path, [0] * 1000)
    assert voice.envelope(str(path)) == (0.05, [0.0, 0.0, 0.0])


def test_long_speech_stretches_the_step_to_stay_within_1200_levels(tmp_path):
    path = tmp_path / "s.wav"
    write_wav(path, tone(70, 1000, rate=1000, freq=100), rate=1000)
    step, levels = voice.envelope(str(path))
    assert len(levels) <= voice.MAX_LEVELS
    assert step == pytest.approx(0.059)
    assert len(levels) * step == pytest.approx(70, abs=0.1)


def test_unreadable_audio_has_no_envelope(tmp_path):
    path = tmp_path / "s.wav"
    path.write_bytes(b"RIFF....not a wave")
    assert voice.envelope(str(path)) == (0.05, [])


# -- backends ------------------------------------------------------------------------------


class FakeVoicevox:
    def __init__(self, wav=b"RIFFwav", up=True):
        self.calls = []
        self.wav = wav
        self.up = up
        self.speakers_fail = False

    def __call__(self, url, timeout=None, max_bytes=None, data=None, content_type=None):
        self.calls.append({"url": url, "timeout": timeout, "data": data, "ctype": content_type})
        if not self.up:
            raise net.FetchError(url, None, "ConnectionRefusedError")
        parsed = urlparse(url)
        q = parse_qs(parsed.query)
        if parsed.path == "/version":
            return b'"0.21.1"'
        if parsed.path == "/speakers":
            if self.speakers_fail:
                raise net.FetchError(url, 500, "boom")
            body = [
                {"name": "四国めたん", "styles": [{"name": "ノーマル", "id": 2}]},
                {"name": "青山龍星", "styles": [{"name": "ノーマル", "id": 13}, {"name": "熱血", "id": 81}]},
            ]
            return json.dumps(body, ensure_ascii=False).encode()
        if parsed.path == "/audio_query" and data == b"":
            return json.dumps({"accent_phrases": [], "speedScale": 1.0, "text": q["text"][0]}, ensure_ascii=False).encode()
        if parsed.path == "/synthesis":
            return self.wav
        raise net.FetchError(url, 404, "not found")


def test_voicevox_queries_then_synthesises(tmp_path):
    fake = FakeVoicevox()
    backend = voice.Voicevox("http://127.0.0.1:50021", 13, fetch=fake)
    out = tmp_path / "out.wav"
    backend.synth("こんにちは。", str(out))
    assert out.read_bytes() == b"RIFFwav"
    query, synthesis = fake.calls
    q = parse_qs(urlparse(query["url"]).query)
    assert urlparse(query["url"]).path == "/audio_query" and q == {"text": ["こんにちは。"], "speaker": ["13"]}
    assert urlparse(synthesis["url"]).path == "/synthesis"
    assert parse_qs(urlparse(synthesis["url"]).query) == {"speaker": ["13"]}
    assert synthesis["ctype"] == "application/json"
    assert json.loads(synthesis["data"])["text"] == "こんにちは。"


def test_voicevox_credit_names_the_speaker():
    fake = FakeVoicevox()
    assert voice.Voicevox("http://127.0.0.1:50021", 13, fetch=fake).credit() == "VOICEVOX:青山龍星"
    assert voice.Voicevox("http://127.0.0.1:50021", 81, fetch=fake).credit() == "VOICEVOX:青山龍星"
    fake.speakers_fail = True
    assert voice.Voicevox("http://127.0.0.1:50021", 13, fetch=fake).credit() == "VOICEVOX"


def test_voicevox_credit_is_looked_up_once():
    fake = FakeVoicevox()
    backend = voice.Voicevox("http://127.0.0.1:50021", 13, fetch=fake)
    backend.credit()
    backend.credit()
    assert sum(urlparse(c["url"]).path == "/speakers" for c in fake.calls) == 1


def test_voicevox_probe_has_a_one_second_budget():
    fake = FakeVoicevox()
    assert voice.Voicevox("http://127.0.0.1:50021", 13, fetch=fake).available()
    assert fake.calls[0]["timeout"] == 1.0 and urlparse(fake.calls[0]["url"]).path == "/version"
    fake.up = False
    assert not voice.Voicevox("http://127.0.0.1:50021", 13, fetch=fake).available()


class FakeRunner:
    """Records commands; writes a WAV where a synthesiser would."""

    def __init__(self, tmp_path, code=0):
        self.calls = []
        self.code = code
        self.wav = wav_bytes(tmp_path, tone(0.1, 9000))

    def __call__(self, args, input=None, timeout=None):
        self.calls.append({"args": list(args), "input": input, "timeout": timeout})
        for flag in ("-ow", "-o"):
            if flag in args:
                with open(args[args.index(flag) + 1], "wb") as f:
                    f.write(self.wav)
        return self.code


def test_open_jtalk_reads_the_sentence_from_stdin(tmp_path):
    run = FakeRunner(tmp_path)
    backend = voice.OpenJTalk("/dic", "/voice.htsvoice", runner=run)
    out = tmp_path / "o.wav"
    backend.synth("こんにちは。", str(out))
    (call,) = run.calls
    assert call["args"] == ["open_jtalk", "-x", "/dic", "-m", "/voice.htsvoice", "-ow", str(out)]
    assert call["input"] == "こんにちは。".encode()
    assert out.read_bytes().startswith(b"RIFF")


def test_say_writes_16_bit_wave(tmp_path):
    run = FakeRunner(tmp_path)
    out = tmp_path / "o.wav"
    voice.Say(runner=run).synth("テスト", str(out))
    (call,) = run.calls
    assert call["args"] == ["say", "-v", "Kyoko", "--file-format=WAVE", "--data-format=LEI16@22050", "-o", str(out)]
    assert call["input"] == "テスト".encode()


def test_a_failing_synthesiser_raises(tmp_path):
    with pytest.raises(voice.VoiceError):
        voice.OpenJTalk("/dic", "/v", runner=FakeRunner(tmp_path, code=1)).synth("x", str(tmp_path / "o.wav"))


def test_no_voice_says_nothing(tmp_path):
    assert voice.NoVoice().synth("x", str(tmp_path / "o.wav")) is False
    assert voice.NoVoice().credit() == ""


def choose(mode, voicevox_up=True, which=(), platform="linux", tmp_path=None):
    return voice.choose_backend(
        mode,
        voicevox_url="http://127.0.0.1:50021",
        speaker=13,
        dic="/dic",
        htsvoice="/v",
        fetch=FakeVoicevox(up=voicevox_up),
        runner=lambda *a, **k: 0,
        which=lambda name: "/usr/bin/" + name if name in which else None,
        platform=platform,
    )


def test_auto_prefers_voicevox_then_open_jtalk_then_say_then_nothing():
    assert choose("auto").name == "voicevox"
    assert choose("auto", voicevox_up=False, which=("open_jtalk", "say")).name == "openjtalk"
    assert choose("auto", voicevox_up=False, which=("say",), platform="darwin").name == "say"
    assert choose("auto", voicevox_up=False, which=("say",), platform="linux").name == "none"
    assert choose("auto", voicevox_up=False).name == "none"


def test_voicevox_must_be_on_this_machine():
    # the sentence travels in the URL: never across the network in cleartext
    def pick(mode, url):
        return voice.choose_backend(mode, url, 13, fetch=FakeVoicevox(), which=lambda name: None, platform="linux")

    for url in ("http://127.0.0.1:50021", "http://localhost:50021", "http://[::1]:50021", "http://127.0.0.2:50021"):
        assert pick("voicevox", url).name == "voicevox", url
        assert pick("auto", url).name == "voicevox", url
    for url in ("http://gpu-box.lan:50021", "http://192.168.1.20:50021", "http://127.0.0.1.example.com:50021", "50021"):
        with pytest.raises(ValueError):
            pick("voicevox", url)
        assert pick("auto", url).name == "none", url


def test_explicit_backends():
    assert choose("voicevox", voicevox_up=False).name == "voicevox"
    assert choose("openjtalk").name == "openjtalk"
    assert choose("say").name == "say"
    assert choose("none").name == "none"
    with pytest.raises(ValueError):
        choose("festival")


def test_auto_keeps_looking_for_voicevox():
    up = {"voicevox": False}
    now = [1000.0]

    def pick():
        return voice.Voicevox("http://v", 13, fetch=FakeVoicevox(up=up["voicevox"])) if up["voicevox"] else voice.NoVoice()

    auto = voice.Auto(pick, clock=lambda: now[0], every=300)
    assert auto.current().name == "none"
    up["voicevox"] = True
    now[0] += 299
    assert auto.current().name == "none"
    now[0] += 1
    assert auto.current().name == "voicevox"
    up["voicevox"] = False
    now[0] += 3600
    assert auto.current().name == "voicevox"  # once found, kept
    assert auto.name == "voicevox"


def test_player_commands():
    assert voice.player_command("/run/earendil/a.wav", "linux", "hdmi:CARD=vc4hdmi0") == [
        "aplay",
        "-q",
        "-D",
        "hdmi:CARD=vc4hdmi0",
        "/run/earendil/a.wav",
    ]
    assert voice.player_command("/tmp/a.wav", "darwin", "default") == ["afplay", "/tmp/a.wav"]


def test_runtime_dir_defaults(monkeypatch, tmp_path):
    monkeypatch.delenv("EARENDIL_RUNTIME_DIR", raising=False)
    monkeypatch.setenv("RUNTIME_DIRECTORY", str(tmp_path))
    assert voice.runtime_dir() == str(tmp_path)
    monkeypatch.setenv("EARENDIL_RUNTIME_DIR", str(tmp_path / "x"))
    assert voice.runtime_dir() == str(tmp_path / "x")
    monkeypatch.delenv("EARENDIL_RUNTIME_DIR")
    monkeypatch.delenv("RUNTIME_DIRECTORY")
    assert voice.runtime_dir() == voice.tempfile.gettempdir()


# -- the sentence queue --------------------------------------------------------------------


def make_speaker(tmp_path, backend=None, player=None):
    events = []
    logs = []
    backend = backend or FakeBackend(wav_bytes(tmp_path, tone(0.1, 9000)))
    player = player or Recorder()
    spk = voice.Speaker(
        backend,
        player,
        str(tmp_path / "run"),
        on_start=lambda conv, started, step, levels: events.append(("start", conv, started, step, len(levels))),
        on_finish=lambda conv: events.append(("finish", conv)),
        clock=lambda: 1234.5,
        log=logs.append,
    )
    return spk, events, backend, player, logs


def test_sentences_are_spoken_in_order_and_reported(tmp_path):
    spk, events, backend, player, _ = make_speaker(tmp_path)
    spk.say(3, "一つ目。")
    spk.say(3, "二つ目。")
    assert spk.run_once(block=False) and spk.run_once(block=False)
    assert not spk.run_once(block=False)
    assert backend.texts == ["一つ目。", "二つ目。"]
    assert events == [("start", 3, 1234.5, 0.05, 2), ("finish", 3), ("start", 3, 1234.5, 0.05, 2), ("finish", 3)]
    assert player.modes == [0o600, 0o600]


def test_audio_files_are_deleted_after_playing(tmp_path):
    spk, _, _, player, _ = make_speaker(tmp_path)
    spk.say(1, "a")
    spk.run_once(block=False)
    (played,) = player.played
    assert os.path.dirname(played) == str(tmp_path / "run")
    assert not os.path.exists(played)
    assert os.listdir(tmp_path / "run") == []


def test_audio_files_are_deleted_when_playing_fails(tmp_path):
    spk, events, _, player, logs = make_speaker(tmp_path, player=Recorder(fail=True))
    spk.say(1, "a")
    spk.run_once(block=False)
    assert os.listdir(tmp_path / "run") == []
    assert events[-1] == ("finish", 1)
    assert logs == ["earendil: voice fake failed: VoiceError"]


def test_a_failed_synthesis_still_finishes_the_sentence_and_logs_no_text(tmp_path):
    backend = FakeBackend(b"", fail={"秘密の文。"})
    spk, events, _, player, logs = make_speaker(tmp_path, backend=backend)
    spk.say(1, "秘密の文。")
    spk.run_once(block=False)
    assert events == [("finish", 1)]
    assert player.played == []
    assert os.listdir(tmp_path / "run") == []
    assert all("秘密" not in line for line in logs)


def test_no_voice_finishes_without_playing(tmp_path):
    spk, events, _, player, _ = make_speaker(tmp_path, backend=voice.NoVoice())
    spk.say(1, "a")
    spk.run_once(block=False)
    assert events == [("finish", 1)] and player.played == []


def test_clear_drops_what_is_queued(tmp_path):
    spk, events, _, _, _ = make_speaker(tmp_path)
    for text in "abc":
        spk.say(1, text)
    assert spk.clear() == 3
    assert not spk.run_once(block=False)
    assert events == []


def test_the_worker_thread_plays_everything(tmp_path):
    spk, events, _, player, _ = make_speaker(tmp_path)
    done = threading.Event()
    spk.on_finish = lambda conv: (events.append(("finish", conv)), len(player.played) == 2 and done.set())
    spk.start()
    spk.say(1, "a")
    spk.say(1, "b")
    assert done.wait(5)
    assert len(player.played) == 2


def test_credit_and_name_come_from_the_backend(tmp_path):
    spk, *_ = make_speaker(tmp_path)
    assert spk.backend_name() == "fake"
    # looked up by the worker (VOICEVOX answers over HTTP), never by /status
    assert spk.credit() == ""
    spk.refresh_credit()
    assert spk.credit() == "FAKE"
    spk.say(1, "a")
    spk.run_once(block=False)
    assert spk.credit() == "FAKE"


def test_leftover_audio_is_swept_at_start(tmp_path):
    # a Ctrl-C or SIGTERM mid-sentence stops the daemon worker before its finally
    run = tmp_path / "run"
    run.mkdir()
    (run / "earendil-x1y2.wav").write_bytes(b"RIFF an answer")
    (run / "other.wav").write_bytes(b"RIFF")
    spk, *_ = make_speaker(tmp_path)
    spk.start()
    assert sorted(os.listdir(run)) == ["other.wav"]


def test_stop_deletes_the_sentence_being_spoken(tmp_path):
    seen = []

    def play(path):
        spk.stop()  # the process is being stopped while this plays
        seen.append(os.path.exists(path))

    spk, events, *_ = make_speaker(tmp_path, player=play)
    spk.say(1, "a")
    spk.say(1, "b")
    assert spk.run_once(block=False)
    assert seen == [False] and os.listdir(tmp_path / "run") == []
    # nothing new is synthesised once stopped
    spk.say(1, "c")
    assert not spk.run_once(block=False)
    assert os.listdir(tmp_path / "run") == []
    spk.stop()  # again, and with nothing in flight: no error
