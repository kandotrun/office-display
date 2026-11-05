"""The conversation state machine behind earendil's /status.

    idle ──wake──▶ listening ──question──▶ thinking ──text──▶ speaking ──▶ answered ──hold──▶ idle
      └──wake + question──────────────────────▲                    └──failure──▶ error ──hold──▶ idle

* A wake with a question goes straight to thinking; a bare wake listens for
  the next phrase (EARENDIL_LISTEN_SECONDS). Silence returns to idle quietly;
  only a mumble too short to be a question ends in error not_understood.
* While thinking or speaking new wakes are ignored; in answered or error a
  wake starts a new conversation (the counter goes up on every wake).
* Phrases finalised while our voice plays, or within 2 s after, are the TV
  hearing itself and are dropped.
* answered and error are held for EARENDIL_HOLD_SECONDS after the voice ends.

All state sits behind one lock; the clock is injectable. Nothing here logs
what was asked or answered: only state names and error codes.
"""
import threading
import time

import wake

IDLE, LISTENING, THINKING, SPEAKING, ANSWERED, ERROR = "idle", "listening", "thinking", "speaking", "answered", "error"
BUSY = (THINKING, SPEAKING)
DONE = (ANSWERED, ERROR)

MAX_QUESTION = 200
MAX_HEARD = 200
MAX_ANSWER = 400
MAX_VALUES = 3
MAX_LEVELS = 1200
MAX_CREDIT = 64
PHRASE_MAX_AGE = 10.0  # older phrases (a restart, a stalled poller) are not acted on
MAX_WINDOWS = 16


class PhraseCursor:
    """New final phrases from successive speech /status documents, each once."""

    def __init__(self, max_age=PHRASE_MAX_AGE):
        self.max_age = max_age
        self.seq = 0

    def new(self, phrases, now):
        if not isinstance(phrases, list):
            return []
        valid = []
        for p in phrases:
            if not isinstance(p, dict):
                continue
            seq, at, text = p.get("seq"), p.get("at"), p.get("text")
            if isinstance(seq, int) and isinstance(at, (int, float)) and isinstance(text, str):
                valid.append((seq, float(at), text))
        if not valid:
            return []
        valid.sort()
        if valid[-1][0] < self.seq:
            self.seq = 0  # speech restarted and counts from 1 again
        out = [p for p in valid if p[0] > self.seq and now - p[1] <= self.max_age]
        self.seq = max(self.seq, valid[-1][0])
        return out


class Conversation:
    def __init__(
        self,
        persona,
        hint,
        start_answer=None,
        voice=None,
        credit=lambda: "",
        clock=time.time,
        listen_seconds=8.0,
        hold_seconds=25.0,
        echo_seconds=2.0,
        busy_seconds=180.0,
        require_wake_cue=False,
        log=print,
    ):
        self.persona = persona
        self.hint = hint
        self.start_answer = start_answer  # (conversation, question), called without the lock
        self.voice = voice  # say(conv, text) / clear() -> dropped, or None
        self.credit = credit
        self.clock = clock
        self.listen_seconds = listen_seconds
        self.hold_seconds = hold_seconds
        self.echo_seconds = echo_seconds
        self.busy_seconds = busy_seconds
        self.require_wake_cue = require_wake_cue
        self.log = log
        self.lock = threading.Lock()

        self.state = IDLE
        self.conversation = 0
        self.question = ""
        self.heard_text = ""
        self.answer = ""
        self.values = []
        self.voice_doc = None
        self.error = None
        self.deadline = None  # listening: give up; answered/error: back to idle
        self.mumbled = False  # listening heard something too short to be a question
        self.busy_since = None
        self.model_done = False
        self.pending = 0  # sentences of this conversation queued or playing
        self.windows = []  # [start, end or None] of our own voice, for the echo rule

    # -- transitions (lock held) ---------------------------------------------------

    def _enter(self, state):
        if state != self.state:
            self.state = state
            extra = " %s" % self.error if state == ERROR else ""
            self.log("earendil: %s%s (conversation %d)" % (state, extra, self.conversation))

    def _clear(self):
        self.question = self.heard_text = self.answer = ""
        self.values = []
        self.voice_doc = None
        self.error = None
        self.deadline = None

    def _idle(self):
        self._clear()
        self._enter(IDLE)

    def _wake(self, question, now):
        """A new conversation; the actions to run once the lock is released."""
        self.conversation += 1
        self._clear()
        self.pending = 0  # nothing of ours is queued outside thinking/speaking
        if question:
            return self._ask(question, now)
        self.mumbled = False
        self.deadline = now + self.listen_seconds
        self._enter(LISTENING)
        return []

    def _ask(self, question, now):
        self.question = question[:MAX_QUESTION]
        self.heard_text = ""
        self.deadline = None
        self.busy_since = now
        self.model_done = False
        self._enter(THINKING)
        if self.start_answer is None:
            return []
        return [(self.start_answer, (self.conversation, self.question))]

    def _hold(self, now):
        self.deadline = now + self.hold_seconds

    def _fail(self, code, now):
        self.error = code
        self.answer = ""
        self.values = []
        self.model_done = True
        if self.voice is not None:
            self.pending = max(0, self.pending - self.voice.clear())
        self.deadline = None
        if self.pending == 0:
            self._hold(now)
        self._enter(ERROR)

    def _maybe_answered(self, now):
        if self.state in BUSY and self.model_done and self.pending == 0:
            self._hold(now)
            self._enter(ANSWERED)

    def _echo(self, at, started_at=None):
        captured_start = at if started_at is None else min(started_at, at)
        return any(start <= at and (end is None or captured_start <= end + self.echo_seconds)
                   for start, end in self.windows)

    @staticmethod
    def _run(actions):
        for fn, args in actions:
            fn(*args)

    # -- inputs ------------------------------------------------------------------------

    def phrase(self, at, text, started_at=None):
        """A phrase captured by `at`, optionally spanning from `started_at`."""
        actions = []
        with self.lock:
            now = self.clock()
            if self._echo(at, started_at) or self.state in BUSY:
                return
            hit = wake.detect(text, require_cue=self.require_wake_cue)
            if self.state == LISTENING:
                if hit is not None and hit.question:
                    actions = self._ask(hit.question, now)
                elif hit is not None:
                    self.deadline = now + self.listen_seconds  # named again: keep listening
                elif len(wake.normalise(text)) < wake.MIN_QUESTION:
                    self.mumbled = True
                else:
                    actions = self._ask(wake.tidy(text), now)
            elif hit is not None:
                actions = self._wake(hit.question, now)
        self._run(actions)

    def heard(self, partial):
        with self.lock:
            if self.state == LISTENING:
                self.heard_text = wake.tidy(partial)[:MAX_HEARD]

    def ask(self, text):
        """POST /ask: (HTTP status, body)."""
        with self.lock:
            if self.state in BUSY:
                return 409, {"conversation": self.conversation}
            actions = self._wake(wake.tidy(text) or text.strip(), self.clock())
            conversation = self.conversation
        self._run(actions)
        return 202, {"conversation": conversation}

    def tick(self):
        with self.lock:
            now = self.clock()
            if self.state == LISTENING and now >= self.deadline:
                if self.mumbled:
                    self._fail("not_understood", now)
                else:
                    self._idle()
            elif self.state in BUSY and now - self.busy_since >= self.busy_seconds:
                self._fail("llm_failed", now)
            elif self.state in DONE and self.deadline is not None and now >= self.deadline:
                self._idle()
            self.windows = [w for w in self.windows if w[1] is None or now - w[1] <= 60][-MAX_WINDOWS:]

    # -- from the answer thread ---------------------------------------------------------

    def _current(self, conv):
        return conv == self.conversation and self.state in BUSY

    def answer_text(self, conv, answer, values):
        with self.lock:
            if not self._current(conv):
                return
            self.answer = answer[:MAX_ANSWER]
            self.values = list(values[:MAX_VALUES])
            if self.answer:
                self._enter(SPEAKING)

    def sentence(self, conv, text):
        with self.lock:
            if not self._current(conv) or self.voice is None:
                return
            self.pending += 1
            self.voice.say(conv, text)

    def answer_done(self, conv, answer, values):
        with self.lock:
            if not self._current(conv):
                return
            self.answer = answer[:MAX_ANSWER]
            self.values = list(values[:MAX_VALUES])
            self.model_done = True
            if self.answer:
                self._enter(SPEAKING)
            self._maybe_answered(self.clock())

    def answer_failed(self, conv, code):
        with self.lock:
            if self._current(conv):
                self._fail(code, self.clock())

    # -- from the voice --------------------------------------------------------------------

    def voice_started(self, conv, started, step, levels):
        with self.lock:
            self.windows.append([started, None])
            if conv != self.conversation:
                return
            self.voice_doc = {"started": started, "step": step, "levels": list(levels[:MAX_LEVELS])}
            if self.state == THINKING:
                self._enter(SPEAKING)

    def voice_finished(self, conv):
        with self.lock:
            now = self.clock()
            for window in self.windows:
                if window[1] is None:
                    window[1] = now
            if conv != self.conversation:
                return
            self.voice_doc = None
            self.pending = max(0, self.pending - 1)
            if self.state in BUSY:
                self._maybe_answered(now)
            elif self.state in DONE and self.pending == 0 and self.deadline is None:
                self._hold(now)

    # -- output ------------------------------------------------------------------------------

    def status(self):
        with self.lock:
            return {
                "state": self.state,
                "conversation": self.conversation,
                "persona": self.persona,
                "hint": self.hint,
                "question": self.question[:MAX_QUESTION],
                "heard": self.heard_text[:MAX_HEARD] if self.state == LISTENING else "",
                "answer": self.answer[:MAX_ANSWER],
                "values": list(self.values[:MAX_VALUES]),
                "voice": dict(self.voice_doc) if self.voice_doc else None,
                "voice_credit": (self.credit() or "")[:MAX_CREDIT],
                "error": self.error,
            }
