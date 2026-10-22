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
    assert net.USER_AGENT.startswith("office-display-atlas/")
    assert "github.com/kandotrun/office-display" in net.USER_AGENT


def test_fetch_sends_the_user_agent(monkeypatch):
    seen = {}

    def urlopen(req, timeout):
        seen["ua"] = req.get_header("User-agent")
        seen["url"] = req.full_url
        return FakeResponse(b"body")

    monkeypatch.setattr(net.urllib.request, "urlopen", urlopen)
    assert net.fetch("https://example.invalid/x") == b"body"
    assert seen == {"ua": net.USER_AGENT, "url": "https://example.invalid/x"}


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


def test_network_errors_have_no_status(monkeypatch):
    def urlopen(req, timeout):
        raise urllib.error.URLError("offline")

    monkeypatch.setattr(net.urllib.request, "urlopen", urlopen)
    with pytest.raises(net.FetchError) as info:
        net.fetch("https://example.invalid/x")
    assert info.value.status is None


def test_spacer_keeps_the_minimum_interval():
    now = [100.0]
    slept = []

    def sleep(s):
        slept.append(s)
        now[0] += s

    spacer = net.Spacer(1.0, clock=lambda: now[0], sleep=sleep)
    spacer.wait()
    spacer.wait()
    now[0] += 0.4
    spacer.wait()
    now[0] += 5
    spacer.wait()
    assert slept == [pytest.approx(1.0), pytest.approx(0.6)]
