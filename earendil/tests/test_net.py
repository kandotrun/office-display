import http.client
import io
import urllib.error

import pytest

import net


class FakeResponse(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


def test_default_user_agent_identifies_the_project():
    assert net.USER_AGENT.startswith("office-display-earendil/")
    assert "github.com/kandotrun/office-display" in net.USER_AGENT


def test_fetch_sends_the_user_agent(monkeypatch):
    seen = {}

    def urlopen(req, timeout):
        seen.update(ua=req.get_header("User-agent"), url=req.full_url, timeout=timeout)
        return FakeResponse(b"body")

    monkeypatch.setattr(net.urllib.request, "urlopen", urlopen)
    assert net.fetch("https://example.invalid/x", timeout=3) == b"body"
    assert seen == {"ua": net.USER_AGENT, "url": "https://example.invalid/x", "timeout": 3}


def test_fetch_rejects_oversized_bodies(monkeypatch):
    monkeypatch.setattr(net.urllib.request, "urlopen", lambda req, timeout: FakeResponse(b"x" * 11))
    with pytest.raises(net.FetchError):
        net.fetch("https://example.invalid/x", max_bytes=10)


def test_http_errors_keep_their_status(monkeypatch):
    def urlopen(req, timeout):
        raise urllib.error.HTTPError(req.full_url, 404, "Not Found", {}, None)

    monkeypatch.setattr(net.urllib.request, "urlopen", urlopen)
    with pytest.raises(net.FetchError) as info:
        net.fetch("https://example.invalid/x")
    assert info.value.status == 404


@pytest.mark.parametrize(
    "exc",
    [urllib.error.URLError("offline"), http.client.RemoteDisconnected("closed"), TimeoutError("slow")],
)
def test_network_errors_have_no_status(monkeypatch, exc):
    def urlopen(req, timeout):
        raise exc

    monkeypatch.setattr(net.urllib.request, "urlopen", urlopen)
    with pytest.raises(net.FetchError) as info:
        net.fetch("https://example.invalid/x")
    assert info.value.status is None


def test_errors_never_quote_the_query_string(monkeypatch):
    # VOICEVOX takes the sentence as a query parameter; it must not reach a log
    def urlopen(req, timeout):
        raise urllib.error.HTTPError(req.full_url, 500, "boom", {}, None)

    monkeypatch.setattr(net.urllib.request, "urlopen", urlopen)
    with pytest.raises(net.FetchError) as info:
        net.fetch("http://127.0.0.1:50021/audio_query?text=%E7%A7%98%E5%AF%86&speaker=13")
    assert "秘密" not in str(info.value) and "%E7" not in str(info.value)
    assert "/audio_query" in str(info.value)


def test_fetch_local_posts_without_a_proxy(monkeypatch):
    seen = {}

    def open_(req, timeout):
        seen.update(method=req.get_method(), body=req.data, ctype=req.get_header("Content-type"))
        return FakeResponse(b"ok")

    monkeypatch.setattr(net._LOCAL, "open", open_)
    assert net.fetch_local("http://127.0.0.1:1/x", data=b"{}", content_type="application/json") == b"ok"
    assert seen == {"method": "POST", "body": b"{}", "ctype": "application/json"}


@pytest.mark.parametrize("url", ["http://192.168.1.20:50021/version", "http://gpu-box.lan:50021/x", "http://8.8.8.8/"])
def test_fetch_local_refuses_other_machines(monkeypatch, url):
    opened = []
    monkeypatch.setattr(net._LOCAL, "open", lambda req, timeout: opened.append(req) or FakeResponse(b"ok"))
    with pytest.raises(net.FetchError):
        net.fetch_local(url + "?text=%E7%A7%98%E5%AF%86")
    assert opened == []


def test_loopback_names():
    for url in ("http://127.0.0.1:8098/status", "http://localhost:50021", "http://[::1]:1/", "http://127.1.2.3/"):
        assert net.is_loopback(url), url
    for url in ("http://10.0.0.1/", "http://localhost.example.com/", "http://[::2]/", "not a url", "http://[bad/"):
        assert not net.is_loopback(url), url
