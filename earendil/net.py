"""The only place earendil talks HTTP itself: bounded reads, an identifying User-Agent.

(The Anthropic API is reached through the official SDK in oracle.py.)
"""
import http.client
import ipaddress
import os
import urllib.error
import urllib.parse
import urllib.request

USER_AGENT = os.environ.get(
    "EARENDIL_USER_AGENT",
    "office-display-earendil/0.1 (+https://github.com/kandotrun/office-display)",
)
MAX_BYTES = 4 << 20
TIMEOUT = 20.0

# Loopback requests (speech, VOICEVOX) never go through a proxy.
_LOCAL = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def is_loopback(url):
    """Whether a URL names this machine: localhost, 127.0.0.0/8 or ::1."""
    try:
        host = urllib.parse.urlsplit(url).hostname
    except ValueError:
        return False
    if host == "localhost":
        return True
    try:
        return ipaddress.ip_address(host or "").is_loopback
    except ValueError:
        return False


class FetchError(Exception):
    def __init__(self, url, status=None, reason=""):
        # never the query string: VOICEVOX takes the sentence to speak there
        super().__init__("%s: %s %s" % (url.split("?", 1)[0], status if status is not None else "error", reason))
        self.status = status


def _read(open_fn, url, timeout, max_bytes, data=None, content_type=None):
    headers = {"User-Agent": USER_AGENT}
    if content_type:
        headers["Content-Type"] = content_type
    req = urllib.request.Request(url, data=data, headers=headers, method="POST" if data is not None else "GET")
    try:
        with open_fn(req, timeout=timeout) as resp:
            body = resp.read(max_bytes + 1)
    except urllib.error.HTTPError as exc:
        raise FetchError(url, exc.code, exc.reason) from None
    except (urllib.error.URLError, OSError, ValueError, http.client.HTTPException) as exc:
        # HTTPException: IncompleteRead, BadStatusLine, ... are network failures too.
        # The reason is the exception type only: messages may quote the URL.
        raise FetchError(url, None, type(exc).__name__) from None
    if len(body) > max_bytes:
        raise FetchError(url, None, "response larger than %d bytes" % max_bytes)
    return body


def fetch(url, timeout=TIMEOUT, max_bytes=MAX_BYTES):
    return _read(urllib.request.urlopen, url, timeout, max_bytes)


def fetch_local(url, timeout=TIMEOUT, max_bytes=MAX_BYTES, data=None, content_type=None):
    """GET (or POST when data is given) a loopback URL; any other host is refused.

    What goes this way is private (captions, the sentences to speak in the
    VOICEVOX query string), so it never leaves the machine.
    """
    if not is_loopback(url):
        raise FetchError(url, None, "not a loopback address")
    return _read(_LOCAL.open, url, timeout, max_bytes, data, content_type)
