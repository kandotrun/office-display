import pytest

import conversation as cv

T0 = 1_800_000_000.0
WAKE = "エンゼル ヒール なら 締切 前 に 徹夜 する のっ て どう 思う"
QUESTION = "締切前に徹夜するのってどう思う"
ANSWER = "エンデルヒールなら、まず徹夜しなくて済む方法をチームで探すよ。"


class Clock:
    def __init__(self, t=T0):
        self.t = t

    def __call__(self):
        return self.t


class FakeVoice:
    """The Speaker's interface: say() queues, clear() drops what has not started."""

    def __init__(self):
        self.said = []
        self.queued = 0
        self.cleared = 0

    def say(self, conv, text):
        self.said.append((conv, text))
        self.queued += 1

    def clear(self):
        dropped, self.queued = self.queued, 0
        self.cleared += 1
        return dropped


def make(voice=True, **kw):
    clock = Clock()
    started, logs = [], []
    fake = FakeVoice() if voice else None
    c = cv.Conversation(
        persona="エンデルヒール",
        hint="「エンデルヒールなら？」と話しかけてみてください",
        start_answer=lambda conv, question: started.append((conv, question)),
        voice=fake,
        credit=lambda: "VOICEVOX:青山龍星",
        clock=clock,
        log=logs.append,
        **kw,
    )
    return c, clock, fake, started, logs


def play(c, clock, fake, conv, seconds=2.0, levels=(0.0, 0.5, 1.0)):
    """The speaker takes the next sentence, plays it for `seconds` and finishes."""
    fake.queued -= 1
    c.voice_started(conv, clock.t, 0.05, list(levels))
    clock.t += seconds
    c.voice_finished(conv)


def test_initial_status():
    c, *_ = make()
    assert c.status() == {
        "state": "idle",
        "conversation": 0,
        "persona": "エンデルヒール",
        "hint": "「エンデルヒールなら？」と話しかけてみてください",
        "question": "",
        "heard": "",
        "answer": "",
        "values": [],
        "voice": None,
        "voice_credit": "VOICEVOX:青山龍星",
        "error": None,
    }


def test_a_wake_with_a_question_asks_the_model():
    c, clock, _, started, logs = make()
    c.phrase(clock.t, WAKE)
    s = c.status()
    assert (s["state"], s["conversation"], s["question"]) == ("thinking", 1, QUESTION)
    assert started == [(1, QUESTION)]
    assert logs and all("徹夜" not in line for line in logs)


def test_ordinary_talk_does_nothing():
    c, clock, _, started, _ = make()
    c.phrase(clock.t, "様 の お 昼ごはん どう する")
    assert c.status()["state"] == "idle" and started == []


def test_the_whole_conversation_with_voice():
    c, clock, fake, started, _ = make()
    c.phrase(clock.t, WAKE)
    c.answer_text(1, "", ["We not I"])
    assert c.status()["state"] == "thinking" and c.status()["values"] == ["We not I"]
    c.answer_text(1, "エンデルヒールなら、", ["We not I"])
    assert c.status()["state"] == "speaking" and c.status()["answer"] == "エンデルヒールなら、"
    c.sentence(1, ANSWER)
    assert fake.said == [(1, ANSWER)]
    fake.queued -= 1
    c.voice_started(1, clock.t, 0.05, [0.0, 1.0])
    assert c.status()["voice"] == {"started": clock.t, "step": 0.05, "levels": [0.0, 1.0]}
    c.answer_done(1, ANSWER, ["We not I", "Family First"])
    assert c.status()["state"] == "speaking"  # still talking
    clock.t += 3
    c.voice_finished(1)
    s = c.status()
    assert (s["state"], s["answer"], s["values"], s["voice"]) == ("answered", ANSWER, ["We not I", "Family First"], None)
    clock.t += 24.9
    c.tick()
    assert c.status()["state"] == "answered"
    clock.t += 0.1
    c.tick()
    s = c.status()
    assert (s["state"], s["conversation"], s["question"], s["answer"], s["values"], s["error"]) == ("idle", 1, "", "", [], None)


def test_the_hold_starts_when_the_voice_ends_not_when_the_text_does():
    c, clock, fake, *_ = make()
    c.phrase(clock.t, WAKE)
    c.answer_text(1, ANSWER, [])
    c.sentence(1, ANSWER)
    c.answer_done(1, ANSWER, [])
    clock.t += 10  # synthesis is slow
    play(c, clock, fake, 1, seconds=5)
    assert c.status()["state"] == "answered"
    clock.t += 24
    c.tick()
    assert c.status()["state"] == "answered"
    clock.t += 1
    c.tick()
    assert c.status()["state"] == "idle"


def test_without_a_voice_the_answer_is_held_from_the_end_of_the_text():
    c, clock, _, _, _ = make(voice=False)
    c.phrase(clock.t, WAKE)
    c.sentence(1, ANSWER)  # nowhere to go
    c.answer_done(1, ANSWER, [])
    assert c.status()["state"] == "answered"
    clock.t += 25
    c.tick()
    assert c.status()["state"] == "idle"


def test_wake_then_listen_then_the_question():
    c, clock, _, started, _ = make()
    c.phrase(clock.t, "エンゼル ヒール")
    s = c.status()
    assert (s["state"], s["conversation"], s["question"]) == ("listening", 1, "")
    c.heard("締切 前 に")
    assert c.status()["heard"] == "締切前に"
    clock.t += 5
    c.phrase(clock.t, "締切 前 に 徹夜 する の って どう")
    s = c.status()
    assert (s["state"], s["question"], s["heard"]) == ("thinking", "締切前に徹夜するのってどう", "")
    assert started == [(1, "締切前に徹夜するのってどう")]


def test_heard_is_only_shown_while_listening():
    c, clock, *_ = make()
    c.heard("なにか")
    assert c.status()["heard"] == ""


def test_listening_gives_up_silently():
    c, clock, _, started, _ = make()
    c.phrase(clock.t, "エンゼル ヒール")
    clock.t += 7.9
    c.tick()
    assert c.status()["state"] == "listening"
    clock.t += 0.1
    c.tick()
    s = c.status()
    assert (s["state"], s["error"], s["conversation"]) == ("idle", None, 1)
    assert started == []


def test_only_a_mumble_while_listening_is_not_understood():
    c, clock, _, started, _ = make()
    c.phrase(clock.t, "エンゼル ヒール")
    c.phrase(clock.t + 1, "えー")
    assert c.status()["state"] == "listening"
    clock.t += 8
    c.tick()
    s = c.status()
    assert (s["state"], s["error"]) == ("error", "not_understood")
    assert started == []
    clock.t += 25
    c.tick()
    assert c.status()["state"] == "idle" and c.status()["error"] is None


def test_naming_it_again_while_listening_keeps_listening_or_takes_its_question():
    c, clock, _, started, _ = make()
    c.phrase(clock.t, "エンゼル ヒール")
    clock.t += 6
    c.phrase(clock.t, "ねえ エンゼル ヒール")
    clock.t += 6
    c.tick()
    assert c.status()["state"] == "listening"  # the deadline moved
    c.phrase(clock.t, WAKE)
    assert started == [(1, QUESTION)] and c.status()["conversation"] == 1


def test_wakes_are_ignored_while_thinking_and_speaking():
    c, clock, fake, started, _ = make()
    c.phrase(clock.t, WAKE)
    c.phrase(clock.t + 1, "エンゼル ヒール なら 別 の 質問")
    c.answer_text(1, "エンデルヒールなら", [])
    c.phrase(clock.t + 2, "エンゼル ヒール なら 別 の 質問")
    assert started == [(1, QUESTION)] and c.status()["conversation"] == 1


def test_echo_of_our_own_voice_is_ignored():
    c, clock, fake, started, _ = make()
    c.phrase(clock.t, WAKE)
    c.sentence(1, ANSWER)
    c.answer_done(1, ANSWER, [])
    start = clock.t
    play(c, clock, fake, 1, seconds=4)
    end = clock.t
    assert c.status()["state"] == "answered"
    # the TV saying 「エンデルヒールなら、…」, finalised during playback or just after
    c.phrase(start + 1, "エンゼル ヒール なら まず 徹夜 し なく て")
    c.phrase(end + 1.9, "エンゼル ヒール なら チーム で 探す よ")
    assert c.status()["conversation"] == 1
    # someone asking again once the TV is quiet
    clock.t = end + 2.5
    c.phrase(end + 2.1, "エンゼル ヒール なら 次 は どう する")
    s = c.status()
    assert (s["state"], s["conversation"], s["question"]) == ("thinking", 2, "次はどうする")
    assert started[-1] == (2, "次はどうする")


def test_a_wake_while_answered_starts_a_new_conversation():
    c, clock, _, started, _ = make(voice=False)
    c.phrase(clock.t, WAKE)
    c.answer_done(1, ANSWER, ["We not I"])
    clock.t += 5
    c.phrase(clock.t, "エンゼル ヒール")
    s = c.status()
    assert (s["state"], s["conversation"], s["answer"], s["values"]) == ("listening", 2, "", [])


def test_ask_starts_a_conversation_and_refuses_while_busy():
    c, clock, fake, started, _ = make()
    assert c.ask("締め切り前に 徹夜 する のって どう思う") == (202, {"conversation": 1})
    assert started == [(1, "締め切り前に徹夜するのってどう思う")]
    assert c.status()["state"] == "thinking"
    assert c.ask("もう一つ") == (409, {"conversation": 1})
    c.answer_text(1, "エンデルヒールなら", [])
    assert c.ask("もう一つ")[0] == 409
    c.answer_done(1, ANSWER, [])
    assert c.status()["state"] == "answered"
    assert c.ask("もう一つ") == (202, {"conversation": 2})


def test_ask_while_listening_takes_over():
    c, clock, _, started, _ = make()
    c.phrase(clock.t, "エンゼル ヒール")
    assert c.ask("質問です") == (202, {"conversation": 2})
    assert started == [(2, "質問です")]


def test_late_callbacks_of_an_old_conversation_are_ignored():
    c, clock, fake, started, _ = make(voice=False)
    c.phrase(clock.t, WAKE)
    c.answer_failed(1, "llm_failed")
    clock.t += 5
    c.ask("次")
    c.answer_text(1, "古い答え", ["We not I"])
    c.answer_done(1, "古い答え", ["We not I"])
    s = c.status()
    assert (s["state"], s["conversation"], s["answer"], s["values"]) == ("thinking", 2, "", [])


@pytest.mark.parametrize("code", ["no_api_key", "llm_failed", "llm_refused"])
def test_a_failure_is_held_like_an_answer(code):
    c, clock, fake, _, logs = make()
    c.phrase(clock.t, WAKE)
    c.answer_text(1, "途中まで", ["We not I"])
    c.sentence(1, "途中まで。")
    c.sentence(1, "続き。")
    c.answer_failed(1, code)
    s = c.status()
    assert (s["state"], s["error"], s["answer"], s["values"], s["question"]) == ("error", code, "", [], QUESTION)
    assert fake.cleared == 1 and fake.queued == 0
    assert any(code in line for line in logs)
    clock.t += 25
    c.tick()
    assert c.status()["state"] == "idle"


def test_a_failure_while_a_sentence_plays_is_held_from_its_end():
    c, clock, fake, *_ = make()
    c.phrase(clock.t, WAKE)
    c.sentence(1, "一つ目。")
    c.sentence(1, "二つ目。")
    fake.queued -= 1
    c.voice_started(1, clock.t, 0.05, [1.0])  # 一つ目 is playing, 二つ目 queued
    c.answer_failed(1, "llm_refused")
    clock.t += 30
    c.tick()
    assert c.status()["state"] == "error"  # the hold has not started yet
    c.voice_finished(1)
    clock.t += 24
    c.tick()
    assert c.status()["state"] == "error"
    clock.t += 1
    c.tick()
    assert c.status()["state"] == "idle"


def test_a_stuck_answer_gives_up():
    c, clock, *_ = make(busy_seconds=180)
    c.phrase(clock.t, WAKE)
    clock.t += 179
    c.tick()
    assert c.status()["state"] == "thinking"
    clock.t += 1
    c.tick()
    assert (c.status()["state"], c.status()["error"]) == ("error", "llm_failed")


def test_status_string_limits():
    c, clock, fake, *_ = make()
    c.ask("あ" * 300)
    assert len(c.status()["question"]) == 200
    c.answer_text(1, "い" * 500, ["a", "b", "c", "d"])
    s = c.status()
    assert len(s["answer"]) == 400 and s["values"] == ["a", "b", "c"]
    fake.queued += 1
    c.voice_started(1, clock.t, 0.05, [0.5] * 2000)
    assert len(c.status()["voice"]["levels"]) == 1200
    c2, clock2, *_ = make()
    c2.phrase(clock2.t, "エンゼル ヒール")
    c2.heard("う" * 300)
    assert len(c2.status()["heard"]) == 200


def test_listen_and_hold_seconds_are_configurable():
    c, clock, *_ = make(voice=False, listen_seconds=3, hold_seconds=5)
    c.phrase(clock.t, "エンゼル ヒール")
    clock.t += 3
    c.tick()
    assert c.status()["state"] == "idle"
    c.ask("質問")
    c.answer_done(2, ANSWER, [])
    clock.t += 5
    c.tick()
    assert c.status()["state"] == "idle"


# -- reading speech's phrases -------------------------------------------------------------


def phrases(*items):
    return [{"seq": seq, "at": at, "text": text} for seq, at, text in items]


def test_cursor_returns_each_phrase_once():
    cur = cv.PhraseCursor()
    first = phrases((1, T0, "a"), (2, T0 + 1, "b"))
    assert cur.new(first, T0 + 2) == [(1, T0, "a"), (2, T0 + 1, "b")]
    assert cur.new(first, T0 + 2) == []
    assert cur.new(first[1:] + phrases((3, T0 + 2, "c")), T0 + 3) == [(3, T0 + 2, "c")]


def test_cursor_resets_when_speech_restarts():
    cur = cv.PhraseCursor()
    cur.new(phrases((40, T0, "a"), (41, T0 + 1, "b")), T0 + 2)
    assert cur.new(phrases((1, T0 + 5, "after restart")), T0 + 6) == [(1, T0 + 5, "after restart")]
    assert cur.new(phrases((1, T0 + 5, "after restart"), (2, T0 + 7, "next")), T0 + 8) == [(2, T0 + 7, "next")]


def test_cursor_skips_stale_phrases():
    cur = cv.PhraseCursor(max_age=10)
    got = cur.new(phrases((1, T0, "old"), (2, T0 + 15, "new")), T0 + 20)
    assert got == [(2, T0 + 15, "new")]
    assert cur.new(phrases((1, T0, "old"), (2, T0 + 15, "new")), T0 + 21) == []


def test_cursor_ignores_malformed_entries():
    cur = cv.PhraseCursor()
    junk = [None, {"seq": "1", "at": T0, "text": "a"}, {"seq": 2, "at": "x", "text": "b"}, {"seq": 3, "at": T0}]
    assert cur.new(junk + phrases((4, T0, "ok")), T0 + 1) == [(4, T0, "ok")]
    assert cur.new("not a list", T0 + 1) == []
