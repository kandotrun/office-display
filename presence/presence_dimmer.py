#!/usr/bin/env python3
"""Presence-reactive brightness for the office photo frame.

Two independent, privacy-preserving signals keep the screen awake:

* a local face-count endpoint (how many faces, nothing else), and
* a coarse motion estimate from the local camera snapshot, because a face
  detector reliably misses people standing further away.

When neither signal has fired for PRESENCE_IDLE_SECONDS the HDMI output is
faded down; any signal fades it straight back to full brightness.

Fails safe: if both signals are unavailable the screen stays at full
brightness. No image is stored, logged or compared against anything except the
previous frame in memory.
"""
import io
import json
import os
import subprocess
import time
import urllib.request

from PIL import Image

STATUS_URL = os.environ.get("PRESENCE_STATUS_URL", "http://127.0.0.1:8093/status")
SNAPSHOT_URL = os.environ.get("PRESENCE_SNAPSHOT_URL", "http://127.0.0.1:8082/snapshot.jpg")
OUTPUT = os.environ.get("PRESENCE_OUTPUT", "HDMI-2")
IDLE_SECONDS = float(os.environ.get("PRESENCE_IDLE_SECONDS", "900"))
DIM_BRIGHTNESS = float(os.environ.get("PRESENCE_DIM", "0.5"))
MOTION_DIFF = float(os.environ.get("PRESENCE_MOTION_DIFF", "2.0"))
FULL_BRIGHTNESS = 1.0
POLL_SECONDS = 2.0
STALE_MS = 5000
SETTLE_SECONDS = 5.0
TILE = (128, 72)


def log(msg):
    print("[%s] %s" % (time.strftime("%H:%M:%S"), msg), flush=True)


def read_faces():
    """Face count from the local detector, or None when it is not trustworthy."""
    try:
        with urllib.request.urlopen(STATUS_URL, timeout=3) as response:
            status = json.load(response)
    except Exception:
        return None
    age = status.get("source_age_ms") or 10**9
    if not status.get("healthy") or age >= STALE_MS:
        return None
    return int(status.get("detected_faces") or 0)


def read_tile():
    """Small grayscale thumbnail of the current camera frame, or None."""
    try:
        with urllib.request.urlopen(SNAPSHOT_URL, timeout=3) as response:
            data = response.read()
        return Image.open(io.BytesIO(data)).convert("L").resize(TILE).tobytes()
    except Exception:
        return None


def mean_abs_diff(a, b):
    return sum(abs(x - y) for x, y in zip(a, b)) / len(a)


def set_brightness(value):
    result = subprocess.run(
        ["xrandr", "--output", OUTPUT, "--brightness", "%.3f" % value],
        check=False,
        capture_output=True,
    )
    return result.returncode == 0


def fade(start, end):
    steps = max(2, int(abs(end - start) / 0.05))
    ok = True
    for step in range(1, steps + 1):
        ok = set_brightness(start + (end - start) * step / steps)
        time.sleep(0.04)
    return ok


def main():
    current = FULL_BRIGHTNESS
    set_brightness(current)
    previous_tile = None
    settle_until = 0.0
    last_seen = time.monotonic()
    log(
        "start output=%s idle=%.0fs dim=%.2f motion=%.1f"
        % (OUTPUT, IDLE_SECONDS, DIM_BRIGHTNESS, MOTION_DIFF)
    )
    while True:
        now = time.monotonic()
        faces = read_faces()
        tile = read_tile()

        motion = None
        if previous_tile is not None and tile is not None and now >= settle_until:
            motion = mean_abs_diff(previous_tile, tile)
        if tile is not None:
            previous_tile = tile

        face_seen = faces is not None and faces > 0
        moved = motion is not None and motion > MOTION_DIFF
        known = faces is not None or motion is not None

        if face_seen or moved:
            if now - last_seen >= IDLE_SECONDS:
                log(
                    "presence -> waking (faces=%s motion=%s)"
                    % (faces, "%.1f" % motion if motion is not None else "-")
                )
            last_seen = now

        idle_for = now - last_seen
        if not known:
            target, state = FULL_BRIGHTNESS, "unknown"
        elif idle_for >= IDLE_SECONDS:
            target, state = DIM_BRIGHTNESS, "dim"
        else:
            target, state = FULL_BRIGHTNESS, "awake"

        if abs(target - current) > 0.02:
            if fade(current, target):
                current = target
                settle_until = time.monotonic() + SETTLE_SECONDS
                log("%s: brightness %.2f (idle %.0fs)" % (state, target, idle_for))
            else:
                log("warn: xrandr failed, retrying")

        time.sleep(POLL_SECONDS)


if __name__ == "__main__":
    main()
