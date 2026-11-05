"""Asking DeepSeek what Earendil would do (OpenAI-compatible API, streamed).

One request per question:

    client.chat.completions.create(model="deepseek-flash", max_tokens=4000,
                                   reasoning_effort="none", stream=True,
                                   messages=[system] + history + [question])

Thinking is disabled by default for prompt spoken replies. If enabled through
reasoning_effort, reasoning_content is ignored: only delta.content reaches the
screen and voice. The answer arrives as

    [Title, Title]          line 1: the values it is grounded in (or [])
    エンデルヒールなら、…    the rest: what is spoken, cut to 400 characters

and complete sentences are handed on while the stream is still running, so
speech starts early. A filtered answer is an error (llm_refused), and the
sentence it cut off is not spoken. Interrupted streams are errors too.

Nothing here logs or stores question or answer text; History keeps the last
few exchanges in memory for a few minutes only (forget_old() drops them on
time even when nobody asks again).
"""
import collections
import os
import re
import threading
import time
import unicodedata

try:
    import openai
except ImportError:  # the band works without the SDK; questions get no_api_key
    openai = None

MODEL = "deepseek-flash"
EFFORT = "none"
BASE_URL = "https://api.deepseek.com"
MAX_TOKENS = 4000
TIMEOUT = 60.0
MAX_RETRIES = 2
MAX_ANSWER = 400
MAX_TAGS = 3
MAX_TAG_LINE = 300
TERMINATORS = "。！？!?\n"
SOURCE_MISSING = "（原文はいま取得できていません。上の要約をもとに答えてください。）"

Answer = collections.namedtuple("Answer", "text tags")


class OracleError(Exception):
    """A question that could not be answered; code is the /status error code."""

    def __init__(self, code):
        super().__init__(code)
        self.code = code


def render_system(template, persona, values_block, source):
    """persona.md with its three placeholders filled (the page text last, verbatim)."""
    text = template.replace("{persona}", persona).replace("{values}", values_block)
    return text.replace("{source}", source.strip() or SOURCE_MISSING)


def _key(text):
    return re.sub(r"\s+", " ", unicodedata.normalize("NFKC", text)).strip().casefold()


def match_tags(line, titles, limit=MAX_TAGS):
    """Known titles named in a tag line, in their order there (titles may hold commas)."""
    text = _key(line)
    claimed = []  # (start, end, title)
    for title in sorted(titles, key=len, reverse=True):
        key = _key(title)
        start = text.find(key)
        while start >= 0:
            end = start + len(key)
            bounded = (start == 0 or not text[start - 1].isalnum()) and (end == len(text) or not text[end].isalnum())
            if bounded and not any(start < e and s < end for s, e, _ in claimed):
                claimed.append((start, end, title))
                break
            start = text.find(key, start + 1)
    return [title for _, _, title in sorted(claimed)][:limit]


def _wide(ch):
    return unicodedata.east_asian_width(ch) in ("W", "F")


def join_lines(text):
    """The answer for the screen: lines joined, without a space between Japanese."""
    out = ""
    for line in (part.strip() for part in text.split("\n")):
        if not line:
            continue
        if out and not (_wide(out[-1]) or _wide(line[0])):
            out += " "
        out += line
    return out


def _speakable(sentence):
    return any(ch.isalnum() for ch in sentence)


class AnswerStream:
    """Splits the streamed text into the tag line, the answer and finished sentences."""

    def __init__(self, titles, max_chars=MAX_ANSWER):
        self.titles = titles
        self.max_chars = max_chars
        self.head = ""  # text until we know whether line 1 is a tag line
        self.decided = False
        self.tag_line = None  # None: there was none
        self.raw = ""  # the answer as streamed (capped)
        self.spoken = 0  # raw[:spoken] has been handed out as sentences

    @property
    def tags(self):
        return match_tags(self.tag_line, self.titles) if self.tag_line is not None else []

    @property
    def answer(self):
        return join_lines(self.raw)[: self.max_chars]

    def _decide(self, final=False):
        s = self.head.lstrip()
        if not s:
            return None
        if s[0] in "[［":
            close = min((i for i in (s.find("]"), s.find("］")) if i >= 0), default=-1)
            newline = s.find("\n")
            if close >= 0 and (newline < 0 or close < newline):
                self.tag_line, rest = s[1:close], s[close + 1 :]
            elif newline >= 0:
                self.tag_line, rest = s[1:newline], s[newline + 1 :]
            elif final:
                self.tag_line, rest = s[1:], ""
            elif len(s) > MAX_TAG_LINE:
                rest = s  # no tag line after all
            else:
                return None
        else:
            rest = s
        self.decided = True
        return rest

    def _append(self, text):
        if not self.raw:
            text = text.lstrip()
        self.raw += text[: max(0, self.max_chars - len(self.raw))]

    def _sentences(self, final=False):
        out = []
        start = self.spoken
        for i in range(self.spoken, len(self.raw)):
            ch = self.raw[i]
            if ch in TERMINATORS:
                sentence = self.raw[start : i + (0 if ch == "\n" else 1)].strip()
                if _speakable(sentence):
                    out.append(sentence)
                start = i + 1
        if final:
            rest = self.raw[start:].strip()
            if _speakable(rest):
                out.append(rest)
            start = len(self.raw)
        self.spoken = start
        return out

    def feed(self, delta):
        """Add streamed text; the sentences it completed."""
        if not self.decided:
            self.head += delta
            delta = self._decide()
            if delta is None:
                return []
        self._append(delta)
        return self._sentences()

    def finish(self):
        """The stream ended; the last (unterminated) sentence, if any."""
        if not self.decided:
            rest = self._decide(final=True)
            if rest is not None:
                self._append(rest)
        return self._sentences(final=True)


class History:
    """The last few exchanges, forgotten when the previous one is too old."""

    def __init__(self, keep=3, ttl=180.0, clock=time.time):
        self.keep = keep
        self.ttl = ttl
        self.clock = clock
        self.lock = threading.Lock()
        self.exchanges = []  # (question, assistant text)
        self.last = None

    def _expire(self):
        if self.last is not None and self.clock() - self.last >= self.ttl:
            self.exchanges = []
            self.last = None

    def forget_old(self):
        """Drop the exchanges once they are too old (called on every poller tick)."""
        with self.lock:
            self._expire()

    def add(self, question, answer, tags):
        with self.lock:
            self._expire()
            # the assistant turn as the model wrote it, tag line included
            self.exchanges.append((question, "[%s]\n%s" % (", ".join(tags), answer)))
            del self.exchanges[: -self.keep]
            self.last = self.clock()

    def messages(self):
        with self.lock:
            self._expire()
            out = []
            for question, answer in self.exchanges:
                out.append({"role": "user", "content": question})
                out.append({"role": "assistant", "content": answer})
            return out


def default_client(timeout=TIMEOUT):
    if openai is None:
        raise RuntimeError("the openai SDK is not installed")
    # Always select DeepSeek explicitly; never fall back to OPENAI_API_KEY.
    key = os.environ.get("DEEPSEEK_API_KEY", "").strip()
    if not key:
        raise RuntimeError("DEEPSEEK_API_KEY is not configured")
    return openai.OpenAI(api_key=key, base_url=BASE_URL, timeout=timeout, max_retries=MAX_RETRIES)


class Oracle:
    def __init__(
        self,
        template,
        persona,
        values_block,
        titles,
        source=lambda: "",
        client=None,
        client_factory=None,
        model=MODEL,
        effort=EFFORT,
        max_tokens=MAX_TOKENS,
        log=print,
    ):
        self.template = template
        self.persona = persona
        self.values_block = values_block
        self.titles = list(titles)
        self.source = source
        self.client = client
        self.client_factory = client_factory or default_client
        self.model = model
        self.effort = effort
        self.max_tokens = max_tokens
        self.log = log
        self.lock = threading.Lock()

    def system_prompt(self):
        return render_system(self.template, self.persona, self.values_block, self.source() or "")

    def _client(self):
        with self.lock:
            if self.client is None:
                try:
                    self.client = self.client_factory()
                except Exception as exc:
                    self.log("earendil: no model client (%s)" % type(exc).__name__)
                    raise OracleError("no_api_key") from None
            return self.client

    def request(self, question, history=()):
        return {
            "model": self.model,
            "max_tokens": self.max_tokens,
            "reasoning_effort": self.effort,
            "stream": True,
            "messages": [{"role": "system", "content": self.system_prompt()}]
            + list(history) + [{"role": "user", "content": question}],
        }

    def _failed(self, exc, status=None, hint=""):
        # the class and status only: messages can quote the request
        status = " %d" % status if status else ""
        self.log("earendil: model failed: %s%s%s" % (type(exc).__name__, status, hint))
        return OracleError("llm_failed")

    def ask(self, question, history=(), on_update=None, on_sentence=None):
        """The Answer to a question; OracleError(code) when there is none."""
        client = self._client()
        parsed = AnswerStream(self.titles)

        def publish(sentences):
            if on_update is not None:
                on_update(parsed.answer, parsed.tags)
            if on_sentence is not None:
                for sentence in sentences:
                    on_sentence(sentence)

        finish_reason = None
        refused = False
        try:
            with client.chat.completions.create(**self.request(question, history)) as stream:
                for chunk in stream:
                    for choice in chunk.choices:
                        if choice.index != 0:
                            continue
                        # Thinking is a separate channel, never an answer to speak.
                        delta = choice.delta
                        if getattr(delta, "refusal", None):
                            refused = True
                        if delta.content:
                            publish(parsed.feed(delta.content))
                        if choice.finish_reason is not None:
                            finish_reason = choice.finish_reason
        except openai.AuthenticationError as exc:
            raise self._failed(exc, exc.status_code, " (the API key was rejected)") from None
        except openai.RateLimitError as exc:
            raise self._failed(exc, exc.status_code, " (rate limited)") from None
        except openai.APIStatusError as exc:
            raise self._failed(exc, exc.status_code) from None
        except openai.APIConnectionError as exc:
            raise self._failed(exc) from None
        except openai.APIError as exc:
            raise self._failed(exc) from None
        if refused or finish_reason == "content_filter":
            # before the tail is flushed: a refused partial is incomplete, not to be spoken
            self.log("earendil: the model declined")
            raise OracleError("llm_refused")
        if finish_reason != "stop":
            self.log("earendil: model stream did not finish")
            raise OracleError("llm_failed")
        publish(parsed.finish())

        if not parsed.answer:
            self.log("earendil: the model gave no answer")
            raise OracleError("llm_failed")
        return Answer(parsed.answer, parsed.tags)
