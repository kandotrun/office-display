import http.client
import json
import threading

import pytest

import speech_server as srv

T0 = 1793782800.0  # unix time of the first phrase
M0 = 5000.0  # the monotonic clock at the same moment


def add(phrases, text, dt):
    return phrases.add(text, T0 + dt, M0 + dt)


def test_seq_starts_at_one_and_counts_every_final_phrase():
    phrases = srv.Phrases(keep=6)
    assert phrases.public() == []
    assert add(phrases, "おはよう", 0) == 1
    assert add(phrases, "エンゼル ヒール なら どう 思う", 2) == 2
    assert [p["seq"] for p in phrases.public()] == [1, 2]


def test_empty_results_are_not_phrases():
    phrases = srv.Phrases(keep=6)
    assert add(phrases, "", 0) is None
    assert add(phrases, "   ", 1) is None
    assert add(phrases, "はい", 2) == 1


def test_public_phrases_are_oldest_first_with_finalisation_time():
    phrases = srv.Phrases(keep=6)
    add(phrases, "一つ目", 0.123)
    add(phrases, "二つ目", 1.5)
    assert phrases.public() == [
        {"seq": 1, "at": 1793782800.12, "text": "一つ目"},
        {"seq": 2, "at": 1793782801.5, "text": "二つ目"},
    ]


def test_only_the_last_phrases_are_kept_and_seq_keeps_counting():
    phrases = srv.Phrases(keep=3)
    for i in range(5):
        add(phrases, "p%d" % i, i)
    assert [(p["seq"], p["text"]) for p in phrases.public()] == [(3, "p2"), (4, "p3"), (5, "p4")]


def test_public_list_is_a_snapshot():
    phrases = srv.Phrases(keep=6)
    add(phrases, "a", 0)
    snap = phrases.public()
    add(phrases, "b", 1)
    assert [p["text"] for p in snap] == ["a"]


def test_caption_is_the_last_two_fresh_phrases():
    phrases = srv.Phrases(keep=6)
    add(phrases, "a", 0)
    add(phrases, "b", 1)
    add(phrases, "c", 2)
    assert phrases.caption(M0 + 3, hold=25) == "b / c"


def test_caption_drops_phrases_older_than_the_hold_but_phrases_keep_them():
    phrases = srv.Phrases(keep=6)
    add(phrases, "a", 0)
    add(phrases, "b", 20)
    assert phrases.caption(M0 + 30, hold=25) == "b"
    assert phrases.caption(M0 + 50, hold=25) == ""
    # independent of the caption hold
    assert [p["text"] for p in phrases.public()] == ["a", "b"]


def test_quiet_for_measures_from_the_newest_fresh_phrase():
    phrases = srv.Phrases(keep=6)
    assert phrases.quiet_for(M0, hold=25) == float("inf")
    add(phrases, "a", 0)
    assert phrases.quiet_for(M0 + 9, hold=25) == pytest.approx(9)
    assert phrases.quiet_for(M0 + 26, hold=25) == float("inf")


def test_initial_state_has_no_phrases():
    assert srv.STATE["phrases"] == []


@pytest.fixture
def served(monkeypatch):
    server = srv.ThreadingHTTPServer(("127.0.0.1", 0), srv.Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield server.server_address[1]
    server.shutdown()
    server.server_close()


def test_status_serves_phrases_next_to_the_existing_fields(served, monkeypatch):
    phrases = srv.Phrases(keep=6)
    add(phrases, "エンゼル ヒール なら どう 思う", 0)
    monkeypatch.setitem(srv.STATE, "text", phrases.caption(M0, 25))
    monkeypatch.setitem(srv.STATE, "phrases", phrases.public())
    conn = http.client.HTTPConnection("127.0.0.1", served, timeout=5)
    conn.request("GET", "/status")
    resp = conn.getresponse()
    doc = json.loads(resp.read())
    assert resp.status == 200
    assert {"db", "bands", "text", "partial", "speaking", "updated", "phrases"} <= set(doc)
    assert doc["text"] == "エンゼル ヒール なら どう 思う"
    assert doc["phrases"] == [{"seq": 1, "at": 1793782800.0, "text": "エンゼル ヒール なら どう 思う"}]
    for name in ("sample_rate", "capture_samples", "decoded_samples", "dropped_chunks", "dropped_samples",
                 "discarded_capture_bytes", "last_decode_at", "queued_audio_ms"):
        assert isinstance(doc[name], (int, float))
