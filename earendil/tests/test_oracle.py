import json
import os
import socket
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from types import SimpleNamespace

import openai
import pytest

import oracle
import values
from fakes import FakeClient, stream_chunk

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
    question = "締め切り前に徹夜するのってどう思う"
    o.ask(question, history)
    (call,) = client.calls
    assert call == {
        "model": "deepseek-flash",
        "max_tokens": 4000,
        "reasoning_effort": "none",
        "stream": True,
        "messages": [{"role": "system", "content": o.system_prompt()}] + history + [{"role": "user", "content": question}],
    }
    assert "Our Values" in call["messages"][0]["content"]
    assert client.last_stream.closed


@pytest.mark.parametrize("effort", ["none", "low", "high", "max"])
def test_model_and_effort_are_configurable(effort):
    client = FakeClient(CHUNKS)
    o, _ = make(client, model="deepseek-v4-pro", effort=effort)
    o.ask("q")
    (call,) = client.calls
    assert call["model"] == "deepseek-v4-pro" and call["reasoning_effort"] == effort


def test_callbacks_follow_the_stream():
    client = FakeClient(CHUNKS)
    o, _ = make(client)
    updates, sentences = [], []
    result = o.ask("q", on_update=lambda a, t: updates.append((a, list(t))), on_sentence=sentences.append)
    assert result == oracle.Answer(ANSWER, ["We not I", "Family First"])
    assert sentences == ["エンデルヒールなら、まず徹夜しなくて済む方法をチームで探すよ。", "今日は30分だけ早く相談してみよう！"]
    assert updates[0] == ("", ["We not I", "Family First"])
    assert updates[-1] == (ANSWER, ["We not I", "Family First"])


def test_reasoning_role_and_usage_chunks_never_reach_the_screen_or_voice():
    chunks = [
        stream_chunk(reasoning_content="秘密の思考。"),
        stream_chunk(),
        SimpleNamespace(choices=[]),
        stream_chunk(content="別候補。", index=1),
        *CHUNKS,
    ]
    o, logs = make(FakeClient(chunks))
    updates, sentences = [], []
    result = o.ask("秘密の質問", on_update=lambda a, t: updates.append(a), on_sentence=sentences.append)
    assert result.text == ANSWER
    assert len(updates) == len(CHUNKS) + 1
    assert "".join(sentences) == ANSWER
    assert not any("秘密" in line or "別候補" in line for line in updates + sentences + logs)


def test_a_refusal_does_not_speak_the_sentence_it_cut_off():
    chunks = ["[We not I]\n休もう。", "エンデルヒールなら、まず"]
    client = FakeClient(chunks, finish_reason="content_filter")
    o, logs = make(client)
    sentences = []
    with pytest.raises(oracle.OracleError) as info:
        o.ask("秘密の質問", on_sentence=sentences.append)
    assert info.value.code == "llm_refused"
    assert sentences == ["休もう。"]
    assert client.last_stream.closed
    assert all("秘密" not in line and "エンデルヒールなら" not in line for line in logs)


def test_a_refusal_delta_is_an_error():
    o, _ = make(FakeClient([stream_chunk(refusal="Cannot help")]))
    with pytest.raises(oracle.OracleError) as info:
        o.ask("q")
    assert info.value.code == "llm_refused"


@pytest.mark.parametrize("finish_reason", [None, "length", "aborted", "insufficient_system_resource", "tool_calls"])
def test_an_interrupted_stream_does_not_speak_the_unfinished_tail(finish_reason):
    o, _ = make(FakeClient(["[]\n休もう。まずは"], finish_reason=finish_reason))
    sentences = []
    with pytest.raises(oracle.OracleError) as info:
        o.ask("q", on_sentence=sentences.append)
    assert info.value.code == "llm_failed"
    assert sentences == ["休もう。"]


def test_an_empty_answer_is_a_failure():
    o, _ = make(FakeClient(["[We not I]\n"]))
    with pytest.raises(oracle.OracleError) as info:
        o.ask("q")
    assert info.value.code == "llm_failed"


def test_no_client_means_no_api_key():
    def factory():
        raise RuntimeError("no credentials: test-private-sdk-key")

    o, logs = make(factory=factory)
    with pytest.raises(oracle.OracleError) as info:
        o.ask("q")
    assert info.value.code == "no_api_key"
    assert logs
    assert all("test-private-sdk-key" not in line for line in logs)


def test_type_errors_are_bugs_not_missing_keys():
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


def test_default_client_always_selects_deepseek(monkeypatch):
    calls = []
    monkeypatch.setenv("DEEPSEEK_API_KEY", "test-deepseek")
    monkeypatch.setenv("OPENAI_API_KEY", "test-other-provider")
    monkeypatch.setenv("OPENAI_BASE_URL", "http://wrong-provider.invalid")
    monkeypatch.setattr(openai, "OpenAI", lambda **kw: calls.append(kw) or object())
    oracle.default_client(timeout=12)
    assert calls == [{"api_key": "test-deepseek", "base_url": "https://api.deepseek.com", "timeout": 12, "max_retries": 2}]


# -- the real SDK against a local fake API -------------------------------------------------


def chunk_event(content=None, reasoning_content=None, finish_reason=None):
    delta = {}
    if content is not None:
        delta["content"] = content
    if reasoning_content is not None:
        delta["reasoning_content"] = reasoning_content
    return {
        "id": "chatcmpl_test", "object": "chat.completion.chunk", "created": 1800000000,
        "model": "deepseek-flash",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
    }


def sse(*events):
    return ("".join("data: %s\n\n" % json.dumps(event, ensure_ascii=False) for event in events) + "data: [DONE]\n\n").encode()


class FakeAPI:
    """POST /chat/completions: records requests, answers with canned DeepSeek SSE."""

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
    return openai.OpenAI(base_url=url, api_key="test", max_retries=0, timeout=10)


def test_the_real_sdk_streams_a_tagged_answer_without_reasoning(no_proxy):
    pieces = ["[We not I, Fami", "ly First]\nエンデルヒールなら、", "まず徹夜しなくて済む方法をチームで探すよ。", "今日は30分だけ早く相談してみよう！"]
    events = [chunk_event(), chunk_event(reasoning_content="秘密の思考。")]
    events += [chunk_event(content=p) for p in pieces]
    events.append(chunk_event(finish_reason="stop"))
    api = FakeAPI(sse(*events))
    try:
        o, logs = make(real_client(api.url))
        sentences = []
        result = o.ask("締め切り前に徹夜するのってどう思う", on_sentence=sentences.append)
    finally:
        api.close()
    assert result == oracle.Answer(ANSWER, ["We not I", "Family First"])
    assert sentences == ["エンデルヒールなら、まず徹夜しなくて済む方法をチームで探すよ。", "今日は30分だけ早く相談してみよう！"]
    assert not any("秘密" in line for line in logs + sentences)
    (req,) = api.requests
    assert req["path"] == "/chat/completions"
    headers = {k.lower(): v for k, v in req["headers"].items()}
    assert headers["authorization"] == "Bearer test"
    assert "anthropic-beta" not in headers
    body = req["body"]
    assert body == o.request("締め切り前に徹夜するのってどう思う")


@pytest.mark.parametrize("finish_reason,code", [("content_filter", "llm_refused"), ("aborted", "llm_failed"), (None, "llm_failed")])
def test_the_real_sdk_rejects_filtered_or_incomplete_answers(no_proxy, finish_reason, code):
    api = FakeAPI(sse(chunk_event(content="[]\n途中"), chunk_event(finish_reason=finish_reason)))
    try:
        o, _ = make(real_client(api.url))
        sentences = []
        with pytest.raises(oracle.OracleError) as info:
            o.ask("q", on_sentence=sentences.append)
    finally:
        api.close()
    assert info.value.code == code
    assert sentences == []


@pytest.mark.parametrize("status", [400, 401, 402, 403, 429, 500, 503])
def test_the_real_sdk_api_errors_are_llm_failed_and_private(no_proxy, status):
    body = json.dumps({"error": {"type": "api_error", "message": "秘密の質問を含むboom Authorization: Bearer test"}}).encode()
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
    assert all("Bearer test" not in line and "authorization" not in line.lower() for line in logs)


def test_the_real_sdk_stream_error_does_not_speak_the_tail(no_proxy):
    api = FakeAPI(sse(chunk_event(content="[]\n途中"), {"error": {"message": "秘密のエラー", "type": "server_error"}}))
    try:
        o, logs = make(real_client(api.url))
        sentences = []
        with pytest.raises(oracle.OracleError) as info:
            o.ask("q", on_sentence=sentences.append)
    finally:
        api.close()
    assert info.value.code == "llm_failed"
    assert sentences == []
    assert all("秘密" not in line for line in logs)


def test_the_real_sdk_connection_errors_are_llm_failed(no_proxy):
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    o, _ = make(real_client("http://127.0.0.1:%d" % port))
    with pytest.raises(oracle.OracleError) as info:
        o.ask("q")
    assert info.value.code == "llm_failed"


@pytest.mark.parametrize("key", [None, "", "   "])
def test_without_deepseek_credentials_is_no_api_key_even_with_openai_key(monkeypatch, key):
    monkeypatch.setenv("OPENAI_API_KEY", "test-other-provider")
    if key is None:
        monkeypatch.delenv("DEEPSEEK_API_KEY", raising=False)
    else:
        monkeypatch.setenv("DEEPSEEK_API_KEY", key)
    o, _ = make()
    with pytest.raises(oracle.OracleError) as info:
        o.ask("q")
    assert info.value.code == "no_api_key"
