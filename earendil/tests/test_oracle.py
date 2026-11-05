import json
import os
import socket
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from types import SimpleNamespace

import anthropic
import pytest

import oracle
import values
from fakes import FakeClient, final

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TITLES = [v["title"] for v in values.load(os.path.join(HERE, "values.json"))["values"]]
with open(os.path.join(HERE, "persona.md"), encoding="utf-8") as f:
    TEMPLATE = f.read()

ANSWER = "エンデルヒールなら、まず徹夜しなくて済む方法をチームで探すよ。今日は30分だけ早く相談してみよう！"


# -- the streamed answer ---------------------------------------------------------------


def feed_all(stream, chunks):
    sentences = []
    for chunk in chunks:
        sentences += stream.feed(chunk)
    sentences += stream.finish()
    return sentences


def test_tags_then_sentences_as_soon_as_they_complete():
    s = oracle.AnswerStream(TITLES)
    assert s.feed("[We not I, Fam") == []
    assert s.tags == [] and s.answer == ""
    assert s.feed("ily First]\nエンデルヒールなら、まず徹夜") == []
    assert s.tags == ["We not I", "Family First"]
    assert s.answer == "エンデルヒールなら、まず徹夜"
    assert s.feed("しなくて済む方法をチームで探すよ。今日は") == ["エンデルヒールなら、まず徹夜しなくて済む方法をチームで探すよ。"]
    assert s.feed("30分だけ早く相談してみよう！") == ["今日は30分だけ早く相談してみよう！"]
    assert s.finish() == []
    assert s.answer == ANSWER


def test_the_remainder_is_spoken_at_the_end():
    s = oracle.AnswerStream(TITLES)
    assert feed_all(s, ["[]\n", "いいね。", "やってみよう"]) == ["いいね。", "やってみよう"]
    assert s.tags == []


def test_a_missing_tag_line_is_tolerated():
    s = oracle.AnswerStream(TITLES)
    assert feed_all(s, ["エンデルヒールなら", "、休むよ。"]) == ["エンデルヒールなら、休むよ。"]
    assert s.tags == [] and s.answer == "エンデルヒールなら、休むよ。"


def test_tags_and_answer_on_one_line():
    s = oracle.AnswerStream(TITLES)
    feed_all(s, ["［be charming］エンデルヒールなら笑うよ。"])
    assert s.tags == ["Be Charming"] and s.answer == "エンデルヒールなら笑うよ。"


def test_tags_are_matched_case_insensitively_unknown_dropped_at_most_three():
    s = oracle.AnswerStream(TITLES)
    feed_all(
        s,
        [
            "[technology is the means, not the end, Moonshots, Ambitious Goals, Credible Steps, "
            "We not I, We not I, Be Charming]\n答え。"
        ],
    )
    assert s.tags == ["Technology is the Means, not the End", "Ambitious Goals, Credible Steps", "We not I"]


def test_an_unterminated_tag_line_ends_at_the_newline():
    s = oracle.AnswerStream(TITLES)
    feed_all(s, ["[Craft Beauty\n", "丁寧に作ろう。"])
    assert s.tags == ["Craft Beauty"] and s.answer == "丁寧に作ろう。"


def test_newlines_end_sentences_and_lines_are_joined_for_display():
    s = oracle.AnswerStream(TITLES)
    assert feed_all(s, ["[]\n", "まず休もう\n\nそれから話そう！？"]) == ["まず休もう", "それから話そう！"]
    assert s.answer == "まず休もうそれから話そう！？"


def test_the_answer_is_cut_to_400_characters():
    s = oracle.AnswerStream(TITLES)
    sentences = feed_all(s, ["[]\n"] + ["あいうえおかきくけ。"] * 50)
    assert len(s.answer) == oracle.MAX_ANSWER
    assert len(sentences) == 40 and "".join(sentences) == s.answer


def test_tag_line_only():
    s = oracle.AnswerStream(TITLES)
    assert feed_all(s, ["[We not I"]) == []
    assert s.tags == ["We not I"] and s.answer == ""


# -- the system prompt -----------------------------------------------------------------


def test_render_system_fills_the_template():
    text = oracle.render_system(TEMPLATE, "エンデルヒール", "Values:\n- We not I（…）: …", "Our Values\nBe Kind")
    assert "「エンデルヒール」と呼ばれています" in text
    assert "Values:\n- We not I（…）: …" in text
    assert text.rstrip().endswith("Our Values\nBe Kind")
    assert "{persona}" not in text and "{values}" not in text and "{source}" not in text


def test_render_system_without_the_page_says_so():
    text = oracle.render_system(TEMPLATE, "エンデルヒール", "V", "")
    assert text.rstrip().endswith(oracle.SOURCE_MISSING)


def test_the_page_text_is_not_templated_again():
    text = oracle.render_system(TEMPLATE, "P", "V", "a {persona} b")
    assert text.rstrip().endswith("a {persona} b")


# -- history -----------------------------------------------------------------------------


class Clock:
    def __init__(self, t=1_800_000_000.0):
        self.t = t

    def __call__(self):
        return self.t


def test_history_keeps_the_last_three_exchanges():
    clock = Clock()
    h = oracle.History(clock=clock)
    assert h.messages() == []
    for i in range(4):
        h.add("q%d" % i, "a%d" % i, ["We not I"] if i else [])
    msgs = h.messages()
    assert [m["role"] for m in msgs] == ["user", "assistant"] * 3
    assert msgs[0] == {"role": "user", "content": "q1"}
    assert msgs[1] == {"role": "assistant", "content": "[We not I]\na1"}


def test_history_is_forgotten_three_minutes_after_the_last_exchange():
    clock = Clock()
    h = oracle.History(clock=clock)
    h.add("q", "a", [])
    clock.t += 179
    assert len(h.messages()) == 2
    clock.t += 1
    assert h.messages() == []
    h.add("q2", "a2", [])
    assert h.messages()[0]["content"] == "q2"


def test_history_forgets_old_exchanges_without_being_asked_again():
    clock = Clock()
    h = oracle.History(clock=clock)
    h.add("締め切り前の徹夜ってどう思う", "まず徹夜しない方法を探すよ。", [])
    clock.t += 179
    h.forget_old()
    assert len(h.exchanges) == 1
    clock.t += 1
    h.forget_old()
    assert h.exchanges == [] and h.last is None


# -- the call, with a fake client ------------------------------------------------------------


def make(client=None, factory=None, **kw):
    logs = []
    o = oracle.Oracle(
        TEMPLATE,
        "エンデルヒール",
        "Values:\n- We not I（「私」ではなく「私たち」）: …",
        TITLES,
        source=lambda: "Our Values",
        client=client,
        client_factory=factory,
        log=logs.append,
        **kw,
    )
    return o, logs


CHUNKS = ["[We not I, Family First]\n", "エンデルヒールなら、まず徹夜しなくて済む方法を", "チームで探すよ。今日は30分だけ早く相談してみよう！"]


def test_the_request_shape():
    client = FakeClient(CHUNKS)
    o, _ = make(client)
    history = [{"role": "user", "content": "前の質問"}, {"role": "assistant", "content": "[]\n前の答え"}]
    o.ask("締め切り前に徹夜するのってどう思う", history)
    (call,) = client.calls
    assert call["model"] == "claude-opus-5-5"
    assert call["max_tokens"] == 4000
    assert call["output_config"] == {"effort": "low"}
    assert call["betas"] == ["server-side-fallback-2026-07-01"]
    assert call["fallbacks"] == "default"
    (system,) = call["system"]
    assert system["type"] == "text" and system["cache_control"] == {"type": "ephemeral"}
    assert system["text"] == o.system_prompt()
    assert "Our Values" in system["text"]
    assert call["messages"] == history + [{"role": "user", "content": "締め切り前に徹夜するのってどう思う"}]
    for banned in ("temperature", "top_p", "top_k", "thinking"):
        assert banned not in call


def test_model_effort_and_fallbacks_are_configurable():
    client = FakeClient(CHUNKS)
    o, _ = make(client, model="claude-sonnet-5-5", effort="medium", fallbacks=False)
    o.ask("q")
    (call,) = client.calls
    assert call["model"] == "claude-sonnet-5-5" and call["output_config"] == {"effort": "medium"}
    assert "betas" not in call and "fallbacks" not in call


def test_callbacks_follow_the_stream():
    client = FakeClient(CHUNKS)
    o, _ = make(client)
    updates, sentences = [], []
    result = o.ask("q", on_update=lambda a, t: updates.append((a, list(t))), on_sentence=sentences.append)
    assert result == oracle.Answer(ANSWER, ["We not I", "Family First"])
    assert sentences == ["エンデルヒールなら、まず徹夜しなくて済む方法をチームで探すよ。", "今日は30分だけ早く相談してみよう！"]
    assert updates[0] == ("", ["We not I", "Family First"])
    assert updates[-1] == (ANSWER, ["We not I", "Family First"])


def test_a_refusal_is_an_error():
    client = FakeClient(["[]\n途中まで"], message=final("[]\n途中まで", stop_reason="refusal"))
    o, logs = make(client)
    with pytest.raises(oracle.OracleError) as info:
        o.ask("q")
    assert info.value.code == "llm_refused"
    assert all("途中" not in line and "q" != line for line in logs)


def test_a_refusal_does_not_speak_the_sentence_it_cut_off():
    # sentences finished while streaming are already spoken; the unfinished tail is not
    chunks = ["[We not I]\n休もう。", "エンデルヒールなら、まず"]
    client = FakeClient(chunks, message=final("".join(chunks), stop_reason="refusal"))
    o, _ = make(client)
    sentences = []
    with pytest.raises(oracle.OracleError) as info:
        o.ask("q", on_sentence=sentences.append)
    assert info.value.code == "llm_refused"
    assert sentences == ["休もう。"]


FALLBACK = SimpleNamespace(type="fallback")
CONTINUED = "エンデルヒールなら、まず徹夜しなくて済む方法をみんなで探すよ。"


def test_a_mid_stream_fallback_continues_the_partial_answer():
    # the fallback model continues from the partial text (only its text blocks are
    # passed on), and text_stream carries both halves: together they are the answer
    content = [
        SimpleNamespace(type="thinking", thinking="", signature="s"),
        SimpleNamespace(type="text", text="[We not I]\nエンデルヒールなら、まず徹夜"),
        FALLBACK,
        SimpleNamespace(type="text", text="しなくて済む方法をみんなで探すよ。"),
    ]
    streamed = ["[We not I]\nエンデルヒールなら、まず徹夜", "しなくて済む方法をみんなで探すよ。"]
    o, logs = make(FakeClient(streamed, message=final(content=content)))
    updates, sentences = [], []
    result = o.ask("q", on_update=lambda a, t: updates.append((a, list(t))), on_sentence=sentences.append)
    assert result == oracle.Answer(CONTINUED, ["We not I"])
    assert updates[-1] == (CONTINUED, ["We not I"])
    assert sentences == [CONTINUED]
    assert any("fallback" in line for line in logs)
    assert all("徹夜" not in line for line in logs)


def test_several_fallbacks_still_make_one_answer():
    content = [
        SimpleNamespace(type="text", text="[We not I]\nみんなで"),
        FALLBACK,
        SimpleNamespace(type="text", text="考えて"),
        FALLBACK,
        SimpleNamespace(type="text", text="みよう。"),
    ]
    o, _ = make(FakeClient(["[We not I]\nみんなで", "考えて", "みよう。"], message=final(content=content)))
    assert o.ask("q") == oracle.Answer("みんなで考えてみよう。", ["We not I"])


def test_an_empty_answer_is_a_failure():
    o, _ = make(FakeClient(["[We not I]\n"]))
    with pytest.raises(oracle.OracleError) as info:
        o.ask("q")
    assert info.value.code == "llm_failed"


def test_no_client_means_no_api_key():
    def factory():
        raise anthropic.AnthropicError("no credentials")

    o, logs = make(factory=factory)
    with pytest.raises(oracle.OracleError) as info:
        o.ask("q")
    assert info.value.code == "no_api_key"
    assert logs


def test_unresolved_credentials_mean_no_api_key():
    error = TypeError('"Could not resolve authentication method. Expected one of api_key, auth_token, ..."')
    o, _ = make(FakeClient(error=error))
    with pytest.raises(oracle.OracleError) as info:
        o.ask("q")
    assert info.value.code == "no_api_key"


def test_other_type_errors_are_bugs_not_missing_keys():
    o, _ = make(FakeClient(error=TypeError("unexpected keyword")))
    with pytest.raises(TypeError):
        o.ask("q")


def test_the_client_is_built_once():
    built = []

    def factory():
        built.append(1)
        return FakeClient(CHUNKS)

    o, _ = make(factory=factory)
    o.ask("q")
    o.ask("q")
    assert built == [1]


# -- the real SDK against a local fake API -------------------------------------------------


def sse(*events):
    out = []
    for event in events:
        out.append("event: %s\ndata: %s\n\n" % (event["type"], json.dumps(event, ensure_ascii=False)))
    return "".join(out).encode()


def message_start(model="claude-opus-5-5"):
    return {
        "type": "message_start",
        "message": {
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [],
            "stop_reason": None,
            "stop_sequence": None,
            "usage": {"input_tokens": 1800, "output_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
        },
    }


def text_deltas(index, pieces):
    events = [{"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}}]
    events += [{"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": p}} for p in pieces]
    events.append({"type": "content_block_stop", "index": index})
    return events


THINKING = [
    {"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}},
    {"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "EqQBCkgIBxABGAIiQL"}},
    {"type": "content_block_stop", "index": 0},
]


def ending(stop_reason="end_turn", stop_details=None):
    delta = {"stop_reason": stop_reason, "stop_sequence": None}
    if stop_details is not None:
        delta["stop_details"] = stop_details
    return [{"type": "message_delta", "delta": delta, "usage": {"output_tokens": 61}}, {"type": "message_stop"}]


class FakeAPI:
    """Just enough of POST /v1/messages: records requests, answers with canned bodies."""

    def __init__(self, body, status=200, ctype="text/event-stream"):
        self.body, self.status, self.ctype = body, status, ctype
        self.requests = []
        api = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args):
                pass

            def do_POST(self):
                length = int(self.headers.get("Content-Length", "0"))
                api.requests.append({"path": self.path, "headers": dict(self.headers), "body": json.loads(self.rfile.read(length))})
                self.send_response(api.status)
                self.send_header("Content-Type", api.ctype)
                self.send_header("Content-Length", str(len(api.body)))
                self.send_header("request-id", "req_test")
                self.end_headers()
                self.wfile.write(api.body)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, args=(0.05,), daemon=True)
        self.thread.start()
        self.url = "http://127.0.0.1:%d" % self.server.server_address[1]

    def close(self):
        self.server.shutdown()
        self.server.server_close()


@pytest.fixture
def no_proxy(monkeypatch):
    for key in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"):
        monkeypatch.delenv(key, raising=False)


def real_client(url):
    return anthropic.Anthropic(base_url=url, api_key="test", max_retries=0, timeout=10)


def test_the_real_sdk_streams_a_tagged_answer(no_proxy):
    pieces = ["[We not I, Fami", "ly First]\nエンデルヒールなら、", "まず徹夜しなくて済む方法をチームで探すよ。", "今日は30分だけ早く相談してみよう！"]
    api = FakeAPI(sse(message_start(), *THINKING, *text_deltas(1, pieces), *ending()))
    try:
        o, _ = make(real_client(api.url))
        sentences = []
        result = o.ask("締め切り前に徹夜するのってどう思う", on_sentence=sentences.append)
    finally:
        api.close()
    assert result == oracle.Answer(ANSWER, ["We not I", "Family First"])
    assert sentences == ["エンデルヒールなら、まず徹夜しなくて済む方法をチームで探すよ。", "今日は30分だけ早く相談してみよう！"]
    (req,) = api.requests
    assert req["path"].split("?")[0] == "/v1/messages"
    headers = {k.lower(): v for k, v in req["headers"].items()}
    assert "server-side-fallback-2026-07-01" in headers["anthropic-beta"]
    # this SDK sends the key as a bearer token; older ones used x-api-key
    assert headers.get("authorization") == "Bearer test" or headers.get("x-api-key") == "test"
    body = req["body"]
    assert body["model"] == "claude-opus-5-5"
    assert body["max_tokens"] == 4000
    assert body["output_config"] == {"effort": "low"}
    assert body["fallbacks"] == "default"
    assert body["stream"] is True
    assert body["system"][0]["cache_control"] == {"type": "ephemeral"}
    assert body["messages"] == [{"role": "user", "content": "締め切り前に徹夜するのってどう思う"}]
    for banned in ("temperature", "top_p", "top_k", "thinking"):
        assert banned not in body


def test_the_real_sdk_refusal(no_proxy):
    details = {"type": "refusal", "category": "cyber", "explanation": None}
    api = FakeAPI(sse(message_start(), *THINKING, *text_deltas(1, ["[]\n途中"]), *ending("refusal", details)))
    try:
        o, _ = make(real_client(api.url))
        with pytest.raises(oracle.OracleError) as info:
            o.ask("q")
    finally:
        api.close()
    assert info.value.code == "llm_refused"


def test_the_real_sdk_pre_output_fallback(no_proxy):
    fallback = {
        "type": "content_block_start",
        "index": 0,
        "content_block": {
            "type": "fallback",
            "from": {"model": "claude-opus-5-5"},
            "to": {"model": "claude-opus-5"},
            "trigger": {"type": "refusal", "category": "cyber"},
        },
    }
    events = [message_start("claude-opus-5"), fallback, {"type": "content_block_stop", "index": 0}]
    events += text_deltas(1, ["[Be Charming]\n", "笑顔でいこう。"]) + ending()
    api = FakeAPI(sse(*events))
    try:
        o, _ = make(real_client(api.url))
        result = o.ask("q")
    finally:
        api.close()
    assert result == oracle.Answer("笑顔でいこう。", ["Be Charming"])


def test_the_real_sdk_mid_stream_fallback(no_proxy):
    fallback = {
        "type": "content_block_start",
        "index": 2,
        "content_block": {
            "type": "fallback",
            "from": {"model": "claude-opus-5-5"},
            "to": {"model": "claude-opus-5"},
            "trigger": {"type": "refusal", "category": "cyber"},
        },
    }
    events = [message_start(), *THINKING, *text_deltas(1, ["[We not I]\n", "エンデルヒールなら、まず徹夜"])]
    events += [fallback, {"type": "content_block_stop", "index": 2}]
    events += text_deltas(3, ["しなくて済む方法を", "みんなで探すよ。"]) + ending()
    api = FakeAPI(sse(*events))
    try:
        o, _ = make(real_client(api.url))
        sentences = []
        result = o.ask("q", on_sentence=sentences.append)
    finally:
        api.close()
    assert result == oracle.Answer(CONTINUED, ["We not I"])
    assert sentences == [CONTINUED]


@pytest.mark.parametrize("status", [400, 401, 429, 500, 529])
def test_the_real_sdk_api_errors_are_llm_failed(no_proxy, status):
    body = json.dumps({"type": "error", "error": {"type": "api_error", "message": "boom"}}).encode()
    api = FakeAPI(body, status=status, ctype="application/json")
    try:
        o, logs = make(real_client(api.url))
        with pytest.raises(oracle.OracleError) as info:
            o.ask("秘密の質問")
    finally:
        api.close()
    assert info.value.code == "llm_failed"
    assert any(str(status) in line for line in logs)
    assert not any("秘密" in line or "boom" in line for line in logs)


def test_the_real_sdk_connection_errors_are_llm_failed(no_proxy):
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    o, _ = make(real_client("http://127.0.0.1:%d" % port))
    with pytest.raises(oracle.OracleError) as info:
        o.ask("q")
    assert info.value.code == "llm_failed"


def test_the_real_sdk_without_credentials_is_no_api_key(no_proxy, monkeypatch, tmp_path):
    for key in list(os.environ):
        if key.startswith("ANTHROPIC_"):
            monkeypatch.delenv(key)
    monkeypatch.setenv("ANTHROPIC_CONFIG_DIR", str(tmp_path))
    api = FakeAPI(b"")
    try:
        o, _ = make(factory=lambda: anthropic.Anthropic(base_url=api.url, max_retries=0))
        with pytest.raises(oracle.OracleError) as info:
            o.ask("q")
    finally:
        api.close()
    assert info.value.code == "no_api_key"
    assert api.requests == []
