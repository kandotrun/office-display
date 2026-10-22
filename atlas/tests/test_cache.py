import os

import pytest

import cache

DAY = 86400


class Clock:
    def __init__(self, t):
        self.t = t

    def __call__(self):
        return self.t


def test_is_fresh_boundaries():
    assert cache.is_fresh(1000.0, 1000.0 + 59, 60)
    assert not cache.is_fresh(1000.0, 1000.0 + 60, 60)


def test_put_then_get(tmp_path):
    c = cache.DiskCache(tmp_path)
    assert c.get("osm/6/56/25.png") is None
    c.put("osm/6/56/25.png", b"tile")
    data, mtime = c.get("osm/6/56/25.png")
    assert data == b"tile" and mtime > 0
    assert (tmp_path / "osm" / "6" / "56" / "25.png").read_bytes() == b"tile"


def test_keys_cannot_leave_the_root(tmp_path):
    c = cache.DiskCache(tmp_path)
    for key in ("../x", "/etc/passwd", "a/../../x"):
        with pytest.raises(ValueError):
            c.path(key)


def test_fresh_entry_is_served_without_fetching(tmp_path):
    c = cache.DiskCache(tmp_path)
    c.put("k", b"old")
    clock = Clock(os.stat(c.path("k")).st_mtime + 6 * DAY)
    c.clock = clock
    calls = []
    assert c.fetch("k", 7 * DAY, lambda: calls.append(1) or b"new") == b"old"
    assert calls == []


def test_stale_entry_is_refetched(tmp_path):
    c = cache.DiskCache(tmp_path)
    c.put("k", b"old")
    c.clock = Clock(os.stat(c.path("k")).st_mtime + 8 * DAY)
    assert c.fetch("k", 7 * DAY, lambda: b"new") == b"new"
    assert c.get("k")[0] == b"new"


def test_stale_entry_is_served_when_the_fetch_fails(tmp_path):
    c = cache.DiskCache(tmp_path)
    c.put("k", b"old")
    c.clock = Clock(os.stat(c.path("k")).st_mtime + 30 * DAY)

    def broken():
        raise OSError("offline")

    assert c.fetch("k", 7 * DAY, broken) == b"old"


def test_invalid_download_is_not_stored(tmp_path):
    c = cache.DiskCache(tmp_path)

    def validate(data):
        raise ValueError("not an image")

    with pytest.raises(ValueError):
        c.fetch("k", DAY, lambda: b"<html>", validate=validate)
    assert c.get("k") is None


def test_missing_entry_and_failed_fetch_raises(tmp_path):
    c = cache.DiskCache(tmp_path)

    def broken():
        raise OSError("offline")

    with pytest.raises(OSError):
        c.fetch("k", DAY, broken)
