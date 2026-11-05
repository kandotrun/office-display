#!/usr/bin/env python3
"""earendil: the values band and the voice consultation agent of the 2F display.

Listens to speech's captions for the wake phrase (エンデルヒール, as Vosk hears
it), asks DeepSeek what Earendil would do, speaks the answer and publishes the
state for photo-frame-rs on loopback:

    GET  /status        the conversation (state, question, answer, voice levels, ...)
    GET  /values.json   the bundled values (titles and our Japanese summaries)
    GET  /emblem.json   the emblem as polygons; 404 until it has been loaded
    GET  /health        which parts are up
    POST /ask           {"text": "..."} as if the wake phrase and this question
                        had been heard: 202 {"conversation": n}, 409 while busy.
                        application/json only, Host 127.0.0.1 or localhost and
                        no Origin: browser pages (cross-site forms, DNS
                        rebinding) cannot start a conversation

/status and /values.json answer at once, offline and without an API key. The
emblem and the original values page load in the background and retry.

Privacy: audio never reaches this process. Only the transcribed text of an
utterance that holds the wake phrase is sent to the DeepSeek API; questions
and answers live in memory (the last 3 exchanges, for 3 minutes) and are never
logged or written to disk. Logs carry state names, error codes and backend
names only.

Environment:

    EARENDIL_PORT               8101
    EARENDIL_SPEECH_URL         http://127.0.0.1:8098/status (polled 4 times a second)
    EARENDIL_VALUES             values.json next to this file
    EARENDIL_PERSONA            エンデルヒール (the name the team calls it)
    EARENDIL_HINT               「<persona>なら？」と話しかけてみてください
    EARENDIL_VALUES_URL         https://earendil.com/values/ (page text for the model, daily)
    EARENDIL_EMBLEM_URL         https://earendil.com/static/earendil-emblem.svg, or a local path
    EARENDIL_CACHE_DIR          ~/.cache/earendil (the emblem SVG and the page text)
    EARENDIL_MODEL              deepseek-flash
    EARENDIL_EFFORT             none (or low, high, max)
    EARENDIL_LLM_TIMEOUT        60 (seconds per request, retried twice by the SDK)
    DEEPSEEK_API_KEY            read from the environment;
                                without credentials questions end in no_api_key
    EARENDIL_TTS                auto | voicevox | openjtalk | say | none
    EARENDIL_VOICEVOX_URL       http://127.0.0.1:50021 (must be on this machine: the sentence is in the URL)
    EARENDIL_VOICEVOX_SPEAKER   13 (青山龍星)
    EARENDIL_OPENJTALK_DIC      /var/lib/mecab/dic/open-jtalk/naist-jdic
    EARENDIL_OPENJTALK_VOICE    /usr/share/hts-voice/nitech-jp-atr503-m001/nitech_jp_atr503_m001.htsvoice
    EARENDIL_AUDIO_OUT          default (the ALSA device for aplay; afplay on macOS)
    EARENDIL_RUNTIME_DIR        $RUNTIME_DIRECTORY, else the temp dir (audio while it plays;
                                earendil-*.wav left there by an abrupt exit is swept at start)
    EARENDIL_LISTEN_SECONDS     8 (how long a bare wake waits for the question)
    EARENDIL_HOLD_SECONDS       25 (how long an answer stays after the voice ends)
    EARENDIL_USER_AGENT         office-display-earendil/0.1 (+https://github.com/kandotrun/office-display)
"""
import json
import os
import signal
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import cache
import conversation
import emblem
import net
import oracle
import values
import voice

HERE = os.path.dirname(os.path.abspath(__file__))
PORT = int(os.environ.get("EARENDIL_PORT", "8101"))
SPEECH_URL = os.environ.get("EARENDIL_SPEECH_URL", "http://127.0.0.1:8098/status")
VALUES_PATH = os.environ.get("EARENDIL_VALUES", os.path.join(HERE, "values.json"))
PERSONA_PROMPT = os.path.join(HERE, "persona.md")
PERSONA = os.environ.get("EARENDIL_PERSONA", "エンデルヒール")
HINT = os.environ.get("EARENDIL_HINT", "「%sなら？」と話しかけてみてください" % PERSONA)
VALUES_URL = os.environ.get("EARENDIL_VALUES_URL", "https://earendil.com/values/")
EMBLEM_URL = os.environ.get("EARENDIL_EMBLEM_URL", "https://earendil.com/static/earendil-emblem.svg")
CACHE_DIR = os.path.expanduser(os.environ.get("EARENDIL_CACHE_DIR", "~/.cache/earendil"))
MODEL = os.environ.get("EARENDIL_MODEL", oracle.MODEL)
EFFORT = os.environ.get("EARENDIL_EFFORT", oracle.EFFORT)
LLM_TIMEOUT = float(os.environ.get("EARENDIL_LLM_TIMEOUT", "60"))
TTS = os.environ.get("EARENDIL_TTS", "auto").strip().lower()
VOICEVOX_URL = os.environ.get("EARENDIL_VOICEVOX_URL", voice.VOICEVOX_URL)
VOICEVOX_SPEAKER = int(os.environ.get("EARENDIL_VOICEVOX_SPEAKER", str(voice.VOICEVOX_SPEAKER)))
OPENJTALK_DIC = os.environ.get("EARENDIL_OPENJTALK_DIC", voice.OPENJTALK_DIC)
OPENJTALK_VOICE = os.environ.get("EARENDIL_OPENJTALK_VOICE", voice.OPENJTALK_VOICE)
AUDIO_OUT = os.environ.get("EARENDIL_AUDIO_OUT", "default")
LISTEN_SECONDS = float(os.environ.get("EARENDIL_LISTEN_SECONDS", "8"))
HOLD_SECONDS = float(os.environ.get("EARENDIL_HOLD_SECONDS", "25"))

POLL_EVERY = 0.25
SPEECH_TIMEOUT = 0.5
SPEECH_MAX_BYTES = 256 << 10
MAX_BODY = 4096
MAX_ASK = 200
REQUEST_TIMEOUT = 5.0  # per socket operation: slow or short requests are dropped
LOCAL_HOSTS = ("127.0.0.1", "localhost")
# (check again after a success, first retry after a failure, longest retry) in
# seconds; the disk cache only re-downloads once a day
REFRESH_EVERY = (3600, 30, 1800)


def spawn_thread(fn, *args):
    threading.Thread(target=fn, args=args, name="answer", daemon=True).start()


class Earendil:
    def __init__(
        self,
        values_doc,
        template,
        persona=PERSONA,
        hint=HINT,
        cache_dir=CACHE_DIR,
        values_url=VALUES_URL,
        emblem_location=EMBLEM_URL,
        speech_url=SPEECH_URL,
        fetch=net.fetch,
        fetch_local=net.fetch_local,
        client=None,
        client_factory=None,
        model=MODEL,
        effort=EFFORT,
        speaker=None,
        spawn=spawn_thread,
        listen_seconds=LISTEN_SECONDS,
        hold_seconds=HOLD_SECONDS,
        clock=time.time,
        log=print,
    ):
        self.values_doc = values_doc
        self.speech_url = speech_url
        self.fetch_local = fetch_local
        self.speaker = speaker
        self.spawn = spawn
        self.clock = clock
        self.log = log
        self.model = model
        self.lock = threading.Lock()
        self.source_text = ""
        self.emblem_doc = None
        self.speech_ok = None  # None until the first poll

        disk = cache.DiskCache(cache_dir)
        self.source = values.SourceText(values_url, disk, fetch)
        self.emblem_source = emblem.EmblemSource(emblem_location, disk, fetch)
        self.cursor = conversation.PhraseCursor()
        self.history = oracle.History(clock=clock)
        self.oracle = oracle.Oracle(
            template,
            persona,
            values.render_block(values_doc["values"]),
            [v["title"] for v in values_doc["values"]],
            source=self._source,
            client=client,
            client_factory=client_factory or (lambda: oracle.default_client(LLM_TIMEOUT)),
            model=model,
            effort=effort,
            log=log,
        )
        self.conversation = conversation.Conversation(
            persona,
            hint,
            start_answer=self._start_answer,
            voice=speaker,
            credit=speaker.credit if speaker is not None else (lambda: ""),
            clock=clock,
            listen_seconds=listen_seconds,
            hold_seconds=hold_seconds,
            log=log,
        )
        if speaker is not None:
            speaker.on_start = self.conversation.voice_started
            speaker.on_finish = self.conversation.voice_finished

    # -- the answer -------------------------------------------------------------------

    def _source(self):
        with self.lock:
            return self.source_text

    def _start_answer(self, conv, question):
        self.spawn(self._answer, conv, question)

    def _answer(self, conv, question):
        talk = self.conversation
        try:
            result = self.oracle.ask(
                question,
                self.history.messages(),
                on_update=lambda answer, tags: talk.answer_text(conv, answer, tags),
                on_sentence=lambda sentence: talk.sentence(conv, sentence),
            )
        except oracle.OracleError as exc:
            talk.answer_failed(conv, exc.code)
            return
        except Exception as exc:
            self.log("earendil: answer failed: %s" % type(exc).__name__)
            talk.answer_failed(conv, "llm_failed")
            return
        self.history.add(question, result.text, result.tags)
        talk.answer_done(conv, result.text, result.tags)

    # -- inputs and refreshers ----------------------------------------------------------

    def poll_speech(self):
        try:
            doc = json.loads(self.fetch_local(self.speech_url, timeout=SPEECH_TIMEOUT, max_bytes=SPEECH_MAX_BYTES))
            if not isinstance(doc, dict):
                raise ValueError("speech status is not an object")
        except Exception as exc:
            if self._speech(False) is not False:
                self.log("earendil: speech unavailable (%s)" % type(exc).__name__)
            return
        if self._speech(True) is False:
            self.log("earendil: speech is back")
        for _, at, text in self.cursor.new(doc.get("phrases"), self.clock()):
            self.conversation.phrase(at, text)
        partial = doc.get("partial")
        if isinstance(partial, str):
            self.conversation.heard(partial)

    def _speech(self, ok):
        """Record whether speech answered; the previous value."""
        with self.lock:
            before, self.speech_ok = self.speech_ok, ok
        return before

    def refresh_emblem(self):
        doc = self.emblem_source.refresh()
        with self.lock:
            self.emblem_doc = doc

    def refresh_source(self):
        text = self.source.refresh()
        if not text:
            raise RuntimeError("no text of %s (offline and nothing cached)" % self.source.url)
        with self.lock:
            self.source_text = text

    def tick(self):
        """One poller round: new captions, timeouts, and forgetting old exchanges on time."""
        self.poll_speech()
        self.conversation.tick()
        self.history.forget_old()

    def run_poller(self):
        while True:
            started = time.monotonic()
            try:
                self.tick()
            except Exception as exc:  # never let the poller die
                self.log("earendil: poller failed: %s" % type(exc).__name__)
            time.sleep(max(0.0, POLL_EVERY - (time.monotonic() - started)))

    # -- documents ----------------------------------------------------------------------

    def emblem(self):
        with self.lock:
            return self.emblem_doc

    def health(self):
        with self.lock:
            speech, emblem_ready, source_ready = self.speech_ok, self.emblem_doc is not None, bool(self.source_text)
        return {
            "ok": True,
            "speech": speech,
            "emblem": emblem_ready,
            "source": source_ready,
            "tts": self.speaker.backend_name() if self.speaker is not None else "none",
            "model": self.model,
        }


def parse_ask(body):
    """The question of a POST /ask body, or None when it is not acceptable."""
    try:
        doc = json.loads(body)
    except ValueError:
        return None
    text = doc.get("text") if isinstance(doc, dict) else None
    if not isinstance(text, str) or not text.strip() or len(text) > MAX_ASK:
        return None
    try:
        text.encode("utf-8")  # lone surrogates are valid JSON escapes, but no text
    except UnicodeEncodeError:
        return None
    return text


def _local_host(value):
    host = (value or "").strip().lower()
    name, _, port = host.rpartition(":")
    if name and port.isdigit():
        host = name
    return host in LOCAL_HOSTS


def refuse_ask(headers):
    """None when a POST /ask may go on, else the HTTP status that refuses it.

    Browsers reach loopback too: a cross-site form posts text/plain without a
    preflight, and DNS rebinding arrives with a foreign Host. Only a JSON body
    addressed to this host and sent without an Origin (curl, tests) is taken.
    """
    if not _local_host(headers.get("Host")) or headers.get("Origin") is not None:
        return 403
    if (headers.get("Content-Type") or "").split(";", 1)[0].strip().lower() != "application/json":
        return 415
    return None


def make_handler(app, timeout=REQUEST_TIMEOUT):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.0"

        def log_message(self, *args):
            pass

        def _json(self, code, doc):
            body = json.dumps(doc, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
            self.send_response(code)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            path = self.path.split("?", 1)[0]
            try:
                if path == "/status":
                    return self._json(200, app.conversation.status())
                if path == "/values.json":
                    return self._json(200, app.values_doc)
                if path == "/emblem.json":
                    doc = app.emblem()
                    return self._json(200, doc) if doc is not None else self._json(404, {})
                if path == "/health":
                    return self._json(200, app.health())
                return self._json(404, {})
            except (BrokenPipeError, ConnectionResetError):
                return
            except Exception as exc:
                print("earendil: %s failed: %s" % (path[:32], type(exc).__name__), flush=True)
                return self._json(500, {})

        def do_POST(self):
            path = self.path.split("?", 1)[0]
            try:
                if path != "/ask":
                    return self._json(404, {})
                refused = refuse_ask(self.headers)
                if refused is not None:
                    return self._json(refused, {})
                try:
                    length = int(self.headers.get("Content-Length", ""))
                except ValueError:
                    return self._json(411, {})
                if length < 0:
                    return self._json(400, {})
                if length > MAX_BODY:
                    return self._json(413, {})
                text = parse_ask(self.rfile.read(length))
                if text is None:
                    return self._json(400, {})
                code, doc = app.conversation.ask(text)
                return self._json(code, doc)
            except (BrokenPipeError, ConnectionResetError, TimeoutError):
                return  # TimeoutError: the body did not arrive in time; just close
            except Exception as exc:
                print("earendil: /ask failed: %s" % type(exc).__name__, flush=True)
                return self._json(500, {})

    Handler.timeout = timeout  # StreamRequestHandler sets it on the socket
    return Handler


def backoff(failures, first, longest):
    return min(longest, first * 2 ** (failures - 1))


def start_loop(name, refresh, every=REFRESH_EVERY, log=print):
    """Run refresh forever in its own thread; failures back off and are logged once per streak."""
    interval, first, longest = every

    def loop():
        failures = 0
        while True:
            try:
                refresh()
                if failures:
                    log("earendil: %s recovered" % name)
                failures = 0
                wait = interval
            except Exception as exc:
                if not failures:
                    log("earendil: %s failed: %s" % (name, exc))
                failures += 1
                wait = backoff(failures, first, longest)
            time.sleep(wait)

    thread = threading.Thread(target=loop, name=name, daemon=True)
    thread.start()
    return thread


def make_backend(mode, log=print):
    def choose():
        return voice.choose_backend(mode, VOICEVOX_URL, VOICEVOX_SPEAKER, OPENJTALK_DIC, OPENJTALK_VOICE)

    if mode == "auto":
        if not net.is_loopback(VOICEVOX_URL):
            log("earendil: EARENDIL_VOICEVOX_URL is not on this machine; auto does not use VOICEVOX")
        return voice.Auto(choose)
    try:
        return choose()
    except ValueError as exc:
        log("earendil: %s; speaking with none" % exc)
        return voice.NoVoice()


def log_line(line):
    print(line, flush=True)


def _terminate(signum, frame):
    raise SystemExit(0)


def serve(server, speaker):
    """Serve until SIGTERM or Ctrl-C; the audio in flight is deleted on the way out.

    (The voice worker is a daemon thread: at exit it stops without running
    its finally blocks, so the main thread cleans up for it.)
    """
    signal.signal(signal.SIGTERM, _terminate)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        speaker.stop()
        server.server_close()


def main():
    values_doc = values.load(VALUES_PATH)
    with open(PERSONA_PROMPT, encoding="utf-8") as f:
        template = f.read()
    os.makedirs(CACHE_DIR, exist_ok=True)
    speaker = voice.Speaker(make_backend(TTS, log_line), voice.Player(AUDIO_OUT), voice.runtime_dir(), log=log_line)
    app = Earendil(values_doc, template, speaker=speaker, log=log_line)
    # bind first: a second instance stops here, before sweeping the first one's audio
    server = ThreadingHTTPServer(("127.0.0.1", PORT), make_handler(app))
    speaker.start()
    start_loop("emblem", app.refresh_emblem, log=log_line)
    start_loop("values page", app.refresh_source, log=log_line)
    threading.Thread(target=app.run_poller, name="speech", daemon=True).start()
    log_line("earendil on 127.0.0.1:%d (model %s, effort %s, tts %s, cache %s)" % (PORT, MODEL, EFFORT, TTS, CACHE_DIR))
    serve(server, speaker)


if __name__ == "__main__":
    main()
