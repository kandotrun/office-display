"""The only place atlas talks HTTP: bounded reads, an identifying User-Agent."""
import os
import threading
import time
import urllib.error
import urllib.request

USER_AGENT = os.environ.get(
    "ATLAS_USER_AGENT",
    "office-display-atlas/0.1 (+https://github.com/kandotrun/office-display)",
)
MAX_BYTES = 32 << 20
TIMEOUT = 20.0

# Loopback requests (the photo manager) never go through a proxy.
_LOCAL = urllib.request.build_opener(urllib.request.ProxyHandler({}))


class FetchError(Exception):
    def __init__(self, url, status=None, reason=""):
        super().__init__("%s: %s %s" % (url, status if status is not None else "error", reason))
        self.url = url
        self.status = status


def _read(open_fn, url, timeout, max_bytes):
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with open_fn(req, timeout=timeout) as resp:
            data = resp.read(max_bytes + 1)
    except urllib.error.HTTPError as exc:
        raise FetchError(url, exc.code, exc.reason) from None
    except (urllib.error.URLError, OSError, ValueError) as exc:
        raise FetchError(url, None, str(exc)) from None
    if len(data) > max_bytes:
        raise FetchError(url, None, "response larger than %d bytes" % max_bytes)
    return data


def fetch(url, timeout=TIMEOUT, max_bytes=MAX_BYTES):
    return _read(urllib.request.urlopen, url, timeout, max_bytes)


def fetch_local(url, timeout=TIMEOUT, max_bytes=64 << 20):
    return _read(_LOCAL.open, url, timeout, max_bytes)


class Spacer:
    """Keeps at least `interval` seconds between the starts of successive calls."""

    def __init__(self, interval, clock=time.monotonic, sleep=time.sleep):
        self.interval = interval
        self.clock = clock
        self.sleep = sleep
        self.lock = threading.Lock()
        self.next = None

    def wait(self):
        with self.lock:
            now = self.clock()
            if self.next is not None and self.next > now:
                self.sleep(self.next - now)
                now = self.clock()
            self.next = now + self.interval
