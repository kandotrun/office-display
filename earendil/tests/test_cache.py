import os

import pytest

import cache

DAY = 86400


class Clock:
    def __init__(self, t):
        self.t = t

    def __call__(self):
        return self.t


def test_put_then_get(tmp_path):
    c = cache.DiskCache(tmp_path)
    assert c.get("emblem.svg") is None
    c.put("emblem.svg", b"<svg/>")
    data, mtime = c.get("emblem.svg")
    assert data == b"<svg/>" and mtime > 0


def test_keys_cannot_leave_the_root(tmp_path):
    c = cache.DiskCache(tmp_path)
    for key in ("../x", "/etc/passwd", "a/../../x"):
        with pytest.raises(ValueError):
            c.path(key)


def test_fresh_entry_is_served_without_fetching(tmp_path):
    c = cache.DiskCache(tmp_path)
    c.put("k", b"old")
    c.clock = Clock(os.stat(c.path("k")).st_mtime + DAY - 1)
    calls = []
    assert c.fetch("k", DAY, lambda: calls.append(1) or b"new") == b"old"
    assert calls == []


def test_stale_entry_is_refetched(tmp_path):
    c = cache.DiskCache(tmp_path)
    c.put("k", b"old")
    c.clock = Clock(os.stat(c.path("k")).st_mtime + DAY)
    assert c.fetch("k", DAY, lambda: b"new") == b"new"
    assert c.get("k")[0] == b"new"


def test_stale_entry_is_served_when_the_fetch_fails(tmp_path):
    c = cache.DiskCache(tmp_path)
    c.put("k", b"old")
    c.clock = Clock(os.stat(c.path("k")).st_mtime + 30 * DAY)

    def broken():
        raise OSError("offline")

    assert c.fetch("k", DAY, broken) == b"old"


def test_invalid_download_is_not_stored(tmp_path):
    c = cache.DiskCache(tmp_path)

    def validate(data):
        raise ValueError("not an emblem")

    with pytest.raises(ValueError):
        c.fetch("k", DAY, lambda: b"<html>", validate=validate)
    assert c.get("k") is None


def test_missing_entry_and_failed_fetch_raises(tmp_path):
    c = cache.DiskCache(tmp_path)

    def broken():
        raise OSError("offline")

    with pytest.raises(OSError):
        c.fetch("k", DAY, broken)
