import http.client
import importlib
import json
import os
import signal
import socket
import threading
import time

import pytest

import earendil_server as srv
import net
import oracle
import values
from fakes import EARENDIL_SVG, FakeBackend, FakeClient, Recorder, tone, wav_bytes

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VALUES_DOC = values.load(os.path.join(HERE, "values.json"))
with open(os.path.join(HERE, "persona.md"), encoding="utf-8") as f:
    TEMPLATE = f.read()

T0 = 1_800_000_000.0
QUESTION = "締め切り前に徹夜するのってどう思う"
ANSWER = "エンデルヒールなら、まず徹夜しなくて済む方法をチームで探すよ。"
CHUNKS = ["[We not I, Family First]\n", "エンデルヒールなら、まず徹夜しなくて", "済む方法をチームで探すよ。"]


class Clock:
    def __init__(self, t=T0):
        self.t = t

    def __call__(self):
        return self.t


class Offline:
    """The internet is down: every fetch fails."""

    def __init__(self):
        self.calls = []

    def __call__(self, url, **kw):
        self.calls.append(url)
        raise net.FetchError(url, None, "offline")


class Web:
    def __init__(self, pages):
        self.pages = pages
        self.calls = []

    def __call__(self, url, **kw):
        self.calls.append(url)
        if url in self.pages:
            return self.pages[url]
        raise net.FetchError(url, 404, "not found")


class Speech:
    """speech's /status as earendil polls it."""

    def __init__(self):
        self.doc = {"db": -60.0, "text": "", "partial": "", "speaking": False, "phrases": []}
        self.down = False
        self.seq = 0
        self.calls = []

    def say(self, text, at):
        self.seq += 1
        self.doc["phrases"] = (self.doc["phrases"] + [{"seq": self.seq, "at": at, "text": text}])[-6:]

    def __call__(self, url, **kw):
        self.calls.append((url, kw))
        if self.down:
            raise net.FetchError(url, None, "ConnectionRefusedError")
        return json.dumps(self.doc, ensure_ascii=False).encode()


def no_key():
    raise RuntimeError("no credentials")


def make(tmp_path, fetch=None, speech=None, client=None, factory=no_key, spawn=None, **kw):
    logs = []
    clock = kw.pop("clock", Clock())
    app = srv.Earendil(
        VALUES_DOC,
        TEMPLATE,
        cache_dir=str(tmp_path),
        fetch=fetch or Offline(),
        fetch_local=speech or Speech(),
        client=client,
        client_factory=factory,
        spawn=spawn or (lambda fn, *args: fn(*args)),
        clock=clock,
        log=logs.append,
        **kw,
    )
    return app, logs, clock


@pytest.fixture
def served(tmp_path):
    app, logs, clock = make(tmp_path)
    server = srv.ThreadingHTTPServer(("127.0.0.1", 0), srv.make_handler(app))
    thread = threading.Thread(target=server.serve_forever, args=(0.05,), daemon=True)
    thread.start()
    yield app, logs, server.server_address[1]
    server.shutdown()
    server.server_close()


def request(port, method, path, body=None, headers=None):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    conn.request(method, path, body=body, headers=headers or {})
    resp = conn.getresponse()
    data = resp.read()
    conn.close()
    return resp.status, resp.getheader("Content-Type"), json.loads(data) if data else None


def test_values_and_status_answer_at_once_even_offline(served):
    _, _, port = served
    status, ctype, doc = request(port, "GET", "/values.json")
    assert status == 200 and ctype.startswith("application/json")
    assert doc == VALUES_DOC and len(doc["values"]) == 14
    status, _, doc = request(port, "GET", "/status")
    assert status == 200
    assert doc["state"] == "idle" and doc["conversation"] == 0 and doc["persona"] == "エンデルヒール"
    assert doc["hint"] == "「エンデルヒールなら？」と話しかけてみてください"


def test_emblem_is_404_until_loaded(served):
    app, _, port = served
    assert request(port, "GET", "/emblem.json")[0] == 404
    with pytest.raises(net.FetchError):
        app.refresh_emblem()
    assert request(port, "GET", "/emblem.json")[0] == 404
    app.emblem_source.fetch = Web({srv.EMBLEM_URL: EARENDIL_SVG})
    app.refresh_emblem()
    status, _, doc = request(port, "GET", "/emblem.json")
    assert status == 200 and doc["aspect"] == 0.549 and len(doc["rings"]) == 1


def test_health(served):
    _, _, port = served
    status, _, doc = request(port, "GET", "/health")
    assert status == 200
    assert doc == {"ok": True, "speech": None, "emblem": False, "source": False, "tts": "none", "model": "deepseek-flash"}


def test_unknown_paths(served):
    _, _, port = served
    assert request(port, "GET", "/nope")[0] == 404
    assert request(port, "POST", "/status", body=b"{}")[0] == 404


def ask(port, body, **headers):
    raw = body if isinstance(body, bytes) else json.dumps(body, ensure_ascii=False).encode()
    return request(port, "POST", "/ask", raw, {"Content-Type": "application/json", **headers})


def test_ask_without_an_api_key(served):
    app, logs, port = served
    status, _, doc = ask(port, {"text": QUESTION})
    assert (status, doc) == (202, {"conversation": 1})
    status, _, doc = request(port, "GET", "/status")
    assert (doc["state"], doc["error"], doc["question"]) == ("error", "no_api_key", QUESTION)
    # an error is held like an answer, and a new question may follow
    assert ask(port, {"text": "もう一度"})[:3:2] == (202, {"conversation": 2})
    assert all(QUESTION not in line and "もう一度" not in line for line in logs)


@pytest.mark.parametrize(
    "body",
    [b"not json", b"[]", b'{"text": 3}', b'{"text": ""}', b'{"text": "   "}', {"text": "あ" * 201}, {"question": "x"}],
)
def test_ask_rejects_bad_bodies(served, body):
    _, _, port = served
    assert ask(port, body)[0] == 400


def test_ask_rejects_large_bodies(served):
    _, _, port = served
    assert ask(port, {"text": "あ", "pad": "x" * 5000})[0] == 413


def raw_post(port, head, body=b"", timeout=3.0):
    """Send a hand-written request; (status line, whether the server closed the socket)."""
    with socket.create_connection(("127.0.0.1", port), timeout=timeout) as s:
        s.sendall(head.encode("latin-1") + b"\r\n\r\n" + body)
        data = b""
        while True:
            try:
                chunk = s.recv(4096)
            except socket.timeout:
                return data.split(b"\r\n", 1)[0].decode(), False
            if not chunk:
                return data.split(b"\r\n", 1)[0].decode(), True
            data += chunk


def test_ask_rejects_a_negative_content_length(served):
    app, _, port = served
    head = "POST /ask HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: -1"
    status, closed = raw_post(port, head, b'{"text":"hello"}')
    assert status.split()[1] == "400" and closed
    assert app.conversation.status()["conversation"] == 0


def test_a_body_shorter_than_its_length_times_out(tmp_path):
    app, _, _ = make(tmp_path)
    server = srv.ThreadingHTTPServer(("127.0.0.1", 0), srv.make_handler(app, timeout=0.3))
    threading.Thread(target=server.serve_forever, args=(0.05,), daemon=True).start()
    try:
        head = "POST /ask HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: 4000"
        started = time.monotonic()
        _, closed = raw_post(server.server_address[1], head, b'{"text":"hello"}')
        assert closed and time.monotonic() - started < 2.5
    finally:
        server.shutdown()
        server.server_close()
    assert srv.REQUEST_TIMEOUT <= 10
    assert app.conversation.status()["conversation"] == 0


def test_ask_takes_only_json(served):
    # a cross-site <form enctype=text/plain> can post a JSON-looking body without CORS
    app, _, port = served
    form = b'{"text":"\xe3\x81\x82\xe3\x81\x84\xe3\x81\x86","a":"="}\r\n'
    assert request(port, "POST", "/ask", form, {"Content-Type": "text/plain"})[0] == 415
    assert request(port, "POST", "/ask", form)[0] == 415
    assert app.conversation.status()["conversation"] == 0
    status = request(port, "POST", "/ask", b'{"text":"x y z"}', {"Content-Type": "application/json; charset=utf-8"})[0]
    assert status == 202


@pytest.mark.parametrize(
    "headers",
    [{"Host": "evil.example:8101"}, {"Host": "192.168.1.20:8101"}, {"Origin": "https://evil.example"}],
)
def test_ask_is_for_this_host_only(served, headers):
    # DNS rebinding sends a foreign Host; a page in a browser sends an Origin
    app, _, port = served
    assert ask(port, {"text": QUESTION}, **headers)[0] == 403
    assert app.conversation.status()["conversation"] == 0


def test_ask_accepts_localhost(served):
    _, _, port = served
    assert ask(port, {"text": QUESTION}, Host="localhost:8101")[0] == 202


def test_ask_rejects_text_that_is_not_unicode(served):
    # a lone surrogate is valid JSON but cannot be encoded: /status would fail on it
    app, _, port = served
    body = b'{"text":"\\ud800\\u3069\\u3046"}'
    assert srv.parse_ask(body) is None
    assert ask(port, body)[0] == 400
    assert request(port, "GET", "/status")[0] == 200


def test_ask_while_busy_is_409(tmp_path):
    app, _, _ = make(tmp_path, spawn=lambda fn, *args: None)  # the model never answers
    assert app.conversation.ask(QUESTION) == (202, {"conversation": 1})
    assert app.conversation.ask(QUESTION) == (409, {"conversation": 1})


def test_a_whole_answer_with_history(tmp_path):
    client = FakeClient(CHUNKS)
    app, logs, clock = make(tmp_path, client=client)
    app.conversation.ask(QUESTION)
    s = app.conversation.status()
    assert (s["state"], s["answer"], s["values"]) == ("answered", ANSWER, ["We not I", "Family First"])
    clock.t += 30
    app.conversation.tick()
    app.conversation.ask("じゃあ明日は？")
    second = client.calls[1]["messages"]
    assert second[0]["role"] == "system"
    assert second[1] == {"role": "user", "content": QUESTION}
    assert second[2] == {"role": "assistant", "content": "[We not I, Family First]\n" + ANSWER}
    assert second[-1] == {"role": "user", "content": "じゃあ明日は?"}  # NFKC, like the captions
    assert all("徹夜" not in line and "明日" not in line for line in logs)


@pytest.mark.parametrize("max_age,expected", [(10, "idle"), (30, "thinking")])
def test_local_stt_latency_can_be_allowed_without_changing_capture_timestamp(tmp_path, max_age, expected):
    speech = Speech()
    app, _, clock = make(tmp_path, speech=speech, phrase_max_age=max_age, spawn=lambda *args: None)
    speech.say("エンデルヒールなら休むのは大事？", clock() - 16)
    app.poll_speech()
    assert app.conversation.status()["state"] == expected


def test_delayed_local_stt_still_suppresses_audio_captured_during_tv_speech(tmp_path):
    speech = Speech()
    app, _, clock = make(tmp_path, speech=speech, phrase_max_age=30, spawn=lambda *args: None)
    clock.t = T0 - 20
    app.conversation.voice_started(0, clock(), 0.05, [0.5])
    clock.t = T0 - 14
    app.conversation.voice_finished(0)
    clock.t = T0
    speech.say("エンデルヒールなら休むのは大事？", T0 - 17)
    app.poll_speech()
    assert app.conversation.status()["state"] == "idle"
    assert app.conversation.status()["conversation"] == 0


@pytest.mark.parametrize("started_at,expected", [
    (T0 - 21, "idle"), (T0 - 16.2, "idle"), (T0 - 16, "idle"),
    (T0 - 15.9, "thinking"), (T0 - 15, "thinking"),
])
def test_whisper_interval_rejects_mixed_tv_echo_even_when_room_sound_ends_later(tmp_path, started_at, expected):
    speech = Speech()
    app, _, clock = make(tmp_path, speech=speech, phrase_max_age=30, spawn=lambda *args: None)
    clock.t = T0 - 20
    app.conversation.voice_started(0, clock(), 0.05, [0.5])
    clock.t = T0 - 18
    app.conversation.voice_finished(0)
    clock.t = T0
    speech.say("エンデルヒールなら休むのは大事？", T0 - 12)
    speech.doc["phrases"][-1]["started_at"] = started_at
    app.poll_speech()
    assert app.conversation.status()["state"] == expected


@pytest.mark.parametrize("text,expected", [
    ("エンデルヒール、エンゼルヒール、エアレンディル", "idle"),
    ("エンデルヒール", "idle"),
    ("エンデルヒールなら休むのは大事？", "thinking"),
])
def test_optional_wake_cue_requires_a_deliberate_call_after_the_name(tmp_path, text, expected):
    speech = Speech()
    app, _, clock = make(tmp_path, speech=speech, require_wake_cue=True, spawn=lambda *args: None)
    speech.say(text, clock())
    app.poll_speech()
    assert app.conversation.status()["state"] == expected


@pytest.mark.parametrize("max_age", [0, -1, float("nan"), float("inf"), 61])
def test_invalid_phrase_age_fails_before_the_agent_can_listen(tmp_path, max_age):
    with pytest.raises(ValueError, match="between 0 and 60"):
        make(tmp_path, phrase_max_age=max_age)


def test_phrase_with_recent_end_but_stale_capture_start_is_not_a_new_call(tmp_path):
    speech = Speech()
    app, _, clock = make(tmp_path, speech=speech, phrase_max_age=30, spawn=lambda *args: None)
    speech.say("エンデルヒールなら休むのは大事？", T0 - 5)
    speech.doc["phrases"][-1]["started_at"] = T0 - 35
    app.poll_speech()
    assert app.conversation.status()["conversation"] == 0


def test_echo_window_is_retained_for_the_maximum_phrase_age_and_grace(tmp_path):
    speech = Speech()
    app, _, clock = make(tmp_path, speech=speech, phrase_max_age=60, spawn=lambda *args: None)
    clock.t = T0 - 62
    app.conversation.voice_started(0, clock(), .05, [.5])
    clock.t = T0 - 61
    app.conversation.voice_finished(0)
    clock.t = T0
    app.conversation.tick()
    speech.say("エンデルヒールなら休むのは大事？", T0 - 59)
    speech.doc["phrases"][-1]["started_at"] = T0 - 59.5
    app.poll_speech()
    assert app.conversation.status()["conversation"] == 0


def test_old_exchanges_are_forgotten_on_the_poller_tick(tmp_path):
    app, _, clock = make(tmp_path, client=FakeClient(CHUNKS))
    app.conversation.ask(QUESTION)
    assert len(app.history.exchanges) == 1
    clock.t += 179
    app.tick()
    assert len(app.history.exchanges) == 1
    clock.t += 1
    app.tick()
    assert app.history.exchanges == []


def test_an_unexpected_failure_still_ends_the_conversation(tmp_path):
    client = FakeClient(error=RuntimeError("bug"))
    app, logs, _ = make(tmp_path, client=client)
    app.conversation.ask(QUESTION)
    assert (app.conversation.status()["state"], app.conversation.status()["error"]) == ("error", "llm_failed")
    assert any("RuntimeError" in line for line in logs)


def test_the_page_text_reaches_the_prompt(tmp_path):
    page = b"<main><h1>Values</h1><ul><li>Be kind to each other</li></ul></main>"
    app, _, _ = make(tmp_path, fetch=Web({srv.VALUES_URL: page}))
    assert oracle.SOURCE_MISSING in app.oracle.system_prompt()
    app.refresh_source()
    prompt = app.oracle.system_prompt()
    assert prompt.rstrip().endswith("Values\n- Be kind to each other")
    assert "{" not in prompt


def test_no_page_text_is_a_failure_to_retry(tmp_path):
    app, _, _ = make(tmp_path)
    with pytest.raises(RuntimeError):
        app.refresh_source()
    assert app.health()["source"] is False


def test_the_speech_poller_hears_the_wake_phrase(tmp_path):
    speech = Speech()
    app, logs, clock = make(tmp_path, speech=speech, spawn=lambda fn, *args: None)
    app.poll_speech()
    assert app.health()["speech"] is True
    url, kw = speech.calls[0]
    assert url == srv.SPEECH_URL and kw["timeout"] <= 1.0
    speech.say("エンゼル ヒール", clock.t)
    app.poll_speech()
    assert app.conversation.status()["state"] == "listening"
    speech.doc["partial"] = "締切 前 に"
    app.poll_speech()
    assert app.conversation.status()["heard"] == "締切前に"
    speech.say("締切 前 に 徹夜 する の って どう 思う", clock.t + 1)
    clock.t += 2
    app.poll_speech()
    s = app.conversation.status()
    assert (s["state"], s["question"]) == ("thinking", "締切前に徹夜するのってどう思う")


def test_the_speech_poller_tolerates_speech_being_down(tmp_path):
    speech = Speech()
    speech.down = True
    app, logs, clock = make(tmp_path, speech=speech)
    app.poll_speech()
    app.poll_speech()
    assert app.health()["speech"] is False
    assert len([line for line in logs if "speech" in line]) == 1
    speech.down = False
    speech.say("エンゼル ヒール", clock.t)
    app.poll_speech()
    assert app.health()["speech"] is True
    assert app.conversation.status()["state"] == "listening"


def test_backoff_doubles_up_to_the_limit():
    assert [srv.backoff(n, 30, 1800) for n in range(1, 9)] == [30, 60, 120, 240, 480, 960, 1800, 1800]


def test_defaults(monkeypatch):
    for key in list(os.environ):
        if key.startswith("EARENDIL_"):
            monkeypatch.delenv(key)
    try:
        mod = importlib.reload(srv)
        assert mod.PORT == 8101
        assert mod.SPEECH_URL == "http://127.0.0.1:8098/status"
        assert mod.VALUES_URL == "https://earendil.com/values/"
        assert mod.EMBLEM_URL == "https://earendil.com/static/earendil-emblem.svg"
        assert (mod.MODEL, mod.EFFORT) == ("deepseek-flash", "none")
        assert (mod.LISTEN_SECONDS, mod.HOLD_SECONDS) == (8.0, 25.0)
        assert (mod.TTS, mod.VOICEVOX_SPEAKER, mod.AUDIO_OUT) == ("auto", 13, "default")
        assert mod.PERSONA == "エンデルヒール"
        monkeypatch.setenv("EARENDIL_MODEL", "deepseek-v4-pro")
        monkeypatch.setenv("EARENDIL_EFFORT", "high")
        configured = importlib.reload(srv)
        assert (configured.MODEL, configured.EFFORT) == ("deepseek-v4-pro", "high")
    finally:
        monkeypatch.delenv("EARENDIL_MODEL", raising=False)
        monkeypatch.delenv("EARENDIL_EFFORT", raising=False)
        importlib.reload(srv)


def test_an_unknown_tts_falls_back_to_none():
    logs = []
    backend = srv.make_backend("festival", log=logs.append)
    assert backend.name == "none" and logs


def test_voicevox_elsewhere_is_not_used(monkeypatch):
    monkeypatch.setattr(srv, "VOICEVOX_URL", "http://gpu-box.lan:50021")
    logs = []
    assert srv.make_backend("voicevox", log=logs.append).name == "none"
    assert len(logs) == 1 and "EARENDIL_VOICEVOX_URL" in logs[0]
    logs.clear()
    srv.make_backend("auto", log=logs.append)
    assert len(logs) == 1 and "EARENDIL_VOICEVOX_URL" in logs[0]
    logs.clear()
    monkeypatch.setattr(srv, "VOICEVOX_URL", "http://127.0.0.1:50021")
    srv.make_backend("auto", log=logs.append)
    assert logs == []


class StopRecorder:
    def __init__(self):
        self.stopped = 0

    def stop(self):
        self.stopped += 1


class FakeServer:
    def __init__(self, run):
        self.run = run
        self.closed = False

    def serve_forever(self):
        self.run()

    def server_close(self):
        self.closed = True


def test_ctrl_c_stops_the_voice_on_the_way_out():
    def interrupted():
        raise KeyboardInterrupt

    speaker, server = StopRecorder(), FakeServer(interrupted)
    before = signal.getsignal(signal.SIGTERM)
    try:
        srv.serve(server, speaker)
    finally:
        signal.signal(signal.SIGTERM, before)
    assert speaker.stopped == 1 and server.closed


def test_sigterm_stops_the_voice_and_exits():
    def terminated():
        os.kill(os.getpid(), signal.SIGTERM)
        time.sleep(5)  # the handler interrupts this

    speaker, server = StopRecorder(), FakeServer(terminated)
    before = signal.getsignal(signal.SIGTERM)
    try:
        with pytest.raises(SystemExit) as info:
            srv.serve(server, speaker)
    finally:
        signal.signal(signal.SIGTERM, before)
    assert info.value.code == 0
    assert speaker.stopped == 1 and server.closed


def test_the_answer_is_spoken_sentence_by_sentence(tmp_path):
    backend = FakeBackend(wav_bytes(tmp_path, tone(0.2, 9000)))
    player = Recorder()
    clock = Clock()
    speaker = srv.voice.Speaker(backend, player, str(tmp_path / "run"), clock=clock)
    chunks = ["[We not I]\n", "エンデルヒールなら、休むよ。", "今日は早く帰ろう！"]
    app, logs, _ = make(tmp_path / "app", client=FakeClient(chunks), speaker=speaker, clock=clock)
    speaker.log = logs.append
    app.conversation.ask(QUESTION)
    # the model is done; both sentences wait for the voice
    assert backend.texts == [] and app.conversation.status()["state"] == "speaking"
    assert speaker.run_once(block=False)
    assert app.conversation.status()["state"] == "speaking"
    assert speaker.run_once(block=False)
    s = app.conversation.status()
    assert (s["state"], s["answer"], s["voice_credit"]) == ("answered", "エンデルヒールなら、休むよ。今日は早く帰ろう！", "FAKE")
    assert backend.texts == ["エンデルヒールなら、休むよ。", "今日は早く帰ろう！"]
    assert len(player.played) == 2 and os.listdir(tmp_path / "run") == []
    # the TV hearing itself right after does not wake it
    app.conversation.phrase(clock.t + 1.0, "エンゼル ヒール なら 休む よ")
    assert app.conversation.status()["conversation"] == 1
    assert all("休む" not in line and "徹夜" not in line for line in logs)
    assert app.health()["tts"] == "fake"
