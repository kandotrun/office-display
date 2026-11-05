"""Scripted stand-in for the Earendil agent (dev only).

    python dev/fake_earendil.py [--state idle|listening|thinking|speaking|answered|error]
                                [--emblem-json PATH | --no-emblem]

Serves the frame's side of the contract on 127.0.0.1:8101 (GET /status,
/values.json, /emblem.json; POST /ask) without a key, a microphone or any
audio: a loop of sample conversations with synthesised voice levels, for
screen work. --state pins one state for screenshots. Nothing is logged.
"""
import argparse
import json
import math
import os
import random
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("FAKE_EARENDIL_PORT", "8101"))
HERE = os.path.dirname(os.path.abspath(__file__))
VALUES_PATH = os.path.join(HERE, "..", "earendil", "values.json")

PERSONA = "エンデルヒール"
HINT = "「エンデルヒールなら？」と話しかけてみてください"
CREDIT = "VOICEVOX:青山龍星"
STATES = ("idle", "listening", "thinking", "speaking", "answered", "error")

# Seconds per phase of the loop; speaking lasts as long as its sentences.
IDLE, LISTEN, THINK, HOLD = 12.0, 3.0, 2.5, 8.0
STEP = 0.05  # voice sample step
MORA = 0.13  # seconds per character spoken
GAP = 0.3  # silence between sentences
STREAM = 24.0  # answer characters per second while it streams in

SAMPLES = [
    {
        "question": "締め切り前に徹夜するのってどう思う",
        "answer": "エンデルヒールなら、まず徹夜しなくて済む方法をチームで探すよ。"
        "ひとりで抱え込まずに、早めに相談してほしいな。"
        "大切な人との時間も、仕事と同じくらい大事にしよう。",
        "values": ["We not I", "Family First"],
    },
    {
        "question": "新しい技術を導入するべきかな",
        "answer": "目的が先で、技術はその手段だよ。"
        "スプレッドシートで足りるなら、それで十分。"
        "導入するなら小さく試して、確かめながら一段ずつ進めよう。",
        "values": ["Technology is the Means, not the End", "Ambitious Goals, Credible Steps"],
    },
    {"question": "この企画ってありかな", "error": "llm_failed"},
]

FALLBACK_VALUES = {
    "source": "https://earendil.com/values/",
    "values": [
        {"title": "We not I", "kind": "value", "ja": "「私」ではなく「私たち」",
         "gloss": "最高のチームは個の総和を超える。ひとりでは謙虚に、チームとしては自信をもって。"},
        {"title": "Family First", "kind": "value", "ja": "家族が第一",
         "gloss": "人生を形づくるのは大切な人との関係。高い基準で働くけれど、ぶつかったら家族を優先する。"},
    ],
}


def star_emblem():
    """A tall four-point star with a diamond hole (exercises even-odd)."""
    def bend(v):
        return math.copysign(abs(v) ** 2.2, v)

    outer = []
    for i in range(128):
        a = i / 128 * 2 * math.pi
        outer.append([round(0.5 + 0.5 * bend(math.cos(a)), 4), round(0.5 - 0.5 * bend(math.sin(a)), 4)])
    hole = [[0.5, 0.42], [0.56, 0.5], [0.5, 0.58], [0.44, 0.5]]
    return {"aspect": 0.549, "rings": [outer, hole]}


def sentences(text):
    out, cur = [], ""
    for ch in text:
        cur += ch
        if ch in "。！？!?\n":
            if cur.strip():
                out.append(cur.strip())
            cur = ""
    if cur.strip():
        out.append(cur.strip())
    return out


def levels(sentence, seed):
    """Loudness per STEP for one sentence: a syllable envelope with dips at commas."""
    rng = random.Random(seed)
    n = max(1, int(len(sentence) * MORA / STEP))
    out = []
    for i in range(n):
        t = i * STEP
        k = int(t / MORA)
        ch = sentence[min(k, len(sentence) - 1)]
        phase = (t % MORA) / MORA
        peak = 0.0 if ch in "、。！？" else 0.55 + 0.4 * rng.random()
        env = math.sin(math.pi * min(1.0, phase * 1.3)) ** 0.7
        out.append(round(max(0.0, min(1.0, peak * env + 0.04 * rng.random())), 3))
    return out


def plan(sample):
    """Sentence timings (start, duration, levels) relative to the start of speaking."""
    spoken, t = [], 0.0
    for i, s in enumerate(sentences(sample.get("answer", ""))):
        lv = levels(s, len(sample["question"]) * 31 + i)
        spoken.append((t, len(lv) * STEP, lv))
        t += len(lv) * STEP + GAP
    return spoken, t


def length(sample):
    if "error" in sample:
        return IDLE + LISTEN + THINK + HOLD
    return IDLE + LISTEN + THINK + plan(sample)[1] + HOLD


def base(state, conversation):
    return {
        "state": state, "conversation": conversation, "persona": PERSONA, "hint": HINT,
        "question": "", "heard": "", "answer": "", "values": [], "voice": None,
        "voice_credit": CREDIT, "error": None,
    }


def phase(sample, e, conversation, now):
    """The status `e` seconds into one sample's loop (idle first)."""
    if e < IDLE:
        return base("idle", conversation)
    conversation += 1
    q, e = sample["question"], e - IDLE
    if e < LISTEN:
        st = base("listening", conversation)
        st["heard"] = q[: int(len(q) * min(1.0, e / (LISTEN * 0.8)))]
        return st
    e -= LISTEN
    if e < THINK:
        st = base("thinking", conversation)
        st["question"] = q
        return st
    e -= THINK
    if "error" in sample:
        st = base("error", conversation)
        st.update(question=q, error=sample["error"])
        return st
    spoken, d = plan(sample)
    st = base("speaking" if e < d else "answered", conversation)
    st.update(question=q, values=sample["values"])
    a = sample["answer"]
    st["answer"] = a[: int(e * STREAM)] if e < d else a
    for start, dur, lv in spoken:
        if start <= e < start + dur:
            st["voice"] = {"started": now - (e - start), "step": STEP, "levels": lv}
    return st


class Script:
    def __init__(self, pinned):
        self.lock = threading.Lock()
        self.pinned = pinned
        self.t0 = time.time()
        self.conv_base = 0
        self.ask = None  # (started, sample) while a POST /ask conversation runs

    def status(self):
        with self.lock:
            return self._status(time.time())

    def _status(self, now):
        if self.pinned:
            return self.pinned_status(now)
        if self.ask:
            started, sample = self.ask
            e = IDLE + LISTEN + (now - started)
            if e < length(sample):
                return phase(sample, e, self.conv_base - 1, now)
            self.ask, self.t0 = None, now
        total = sum(length(s) for s in SAMPLES)
        loops, e = divmod(now - self.t0, total)
        n = self.conv_base + int(loops) * len(SAMPLES)
        for s in SAMPLES:
            if e < length(s):
                return phase(s, e, n, now)
            e -= length(s)
            n += 1
        return base("idle", n)

    def pinned_status(self, now):
        sample = SAMPLES[0]
        q, a = sample["question"], sample["answer"]
        st = base(self.pinned, 1 if self.pinned != "idle" else 0)
        if self.pinned == "listening":
            st["heard"] = q[: len(q) * 2 // 3]
        if self.pinned in ("thinking", "speaking", "answered", "error"):
            st["question"] = q
        if self.pinned in ("speaking", "answered"):
            st.update(answer=a, values=sample["values"])
        if self.pinned == "speaking":
            # Loop the first sentence so there is always a voice to follow.
            _, dur, lv = plan(sample)[0][0]
            into = (now - self.t0) % (dur + GAP)
            if into < dur:
                st["voice"] = {"started": now - into, "step": STEP, "levels": lv}
        if self.pinned == "error":
            st["error"] = "llm_failed"
        return st

    def start_ask(self, text):
        """Like the real agent's POST /ask: thinking, speaking, answered."""
        with self.lock:
            now = time.time()
            st = self._status(now)
            if self.pinned or st["state"] in ("thinking", "speaking"):
                return None
            self.conv_base = st["conversation"] + 1
            self.ask = (now, {**SAMPLES[0], "question": text[:200]})
            return self.conv_base


def load_values():
    try:
        with open(VALUES_PATH, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return FALLBACK_VALUES


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--state", choices=STATES, help="pin one state for screenshots")
    g = p.add_mutually_exclusive_group()
    g.add_argument("--emblem-json", metavar="PATH", help="serve this /emblem.json instead of the built-in star")
    g.add_argument("--no-emblem", action="store_true", help="answer /emblem.json with 404 (the frame draws its own star)")
    args = p.parse_args()
    values = json.dumps(load_values(), ensure_ascii=False).encode()
    if args.no_emblem:
        emblem = None
    elif args.emblem_json:
        with open(args.emblem_json, encoding="utf-8") as f:
            doc = json.load(f)
        if not isinstance(doc, dict) or "aspect" not in doc or "rings" not in doc:
            raise SystemExit("--emblem-json: need {\"aspect\": ..., \"rings\": [...]}")
        emblem = json.dumps(doc).encode()
    else:
        emblem = json.dumps(star_emblem()).encode()
    script = Script(args.state)

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def send(self, code, body):
            self.send_response(code)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path == "/status":
                self.send(200, json.dumps(script.status(), ensure_ascii=False).encode())
            elif self.path == "/values.json":
                self.send(200, values)
            elif self.path == "/emblem.json" and emblem is not None:
                self.send(200, emblem)
            else:
                self.send(404, b'{"error":"not_found"}')

        def do_POST(self):
            if self.path != "/ask":
                self.send(404, b'{"error":"not_found"}')
                return
            try:
                n = int(self.headers.get("Content-Length", "0"))
                text = json.loads(self.rfile.read(min(n, 4096)) or b"{}").get("text", "")
            except (ValueError, AttributeError):
                self.send(400, b'{"error":"bad_request"}')
                return
            if not isinstance(text, str) or not text.strip():
                self.send(400, b'{"error":"bad_request"}')
                return
            conversation = script.start_ask(text.strip())
            if conversation is None:
                self.send(409, b'{"error":"busy"}')
            else:
                self.send(202, json.dumps({"conversation": conversation}).encode())

    mode = "pinned to %s" % args.state if args.state else "scripted loop"
    print("fake earendil on 127.0.0.1:%d (%s)" % (PORT, mode), flush=True)
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()


if __name__ == "__main__":
    main()
