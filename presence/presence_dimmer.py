#!/usr/bin/env python3
"""Presence-reactive brightness for the 2F photo frame.

Polls the local camera-gaze-rs face detector (no images, no identity, no
history). When nobody has been seen for PRESENCE_IDLE_SECONDS the HDMI output
is faded down; when a face appears it fades straight back to full brightness.

Fails safe: if the detector is unreachable, unhealthy or stale, the screen
stays at full brightness.
"""
import json
import os
import subprocess
import time
import urllib.request

STATUS_URL = os.environ.get("PRESENCE_STATUS_URL", "http://127.0.0.1:8093/status")
OUTPUT = os.environ.get("PRESENCE_OUTPUT", "HDMI-2")
IDLE_SECONDS = float(os.environ.get("PRESENCE_IDLE_SECONDS", "300"))
DIM_BRIGHTNESS = float(os.environ.get("PRESENCE_DIM", "0.12"))
FULL_BRIGHTNESS = 1.0
POLL_SECONDS = 2.0
STALE_MS = 5000


def log(msg):
    print("[%s] %s" % (time.strftime("%H:%M:%S"), msg), flush=True)


def read_status():
    try:
        with urllib.request.urlopen(STATUS_URL, timeout=3) as response:
            return json.load(response)
    except Exception:
        return None


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
    last_seen = time.monotonic()
    log("start output=%s idle=%.0fs dim=%.2f" % (OUTPUT, IDLE_SECONDS, DIM_BRIGHTNESS))
    while True:
        status = read_status()
        now = time.monotonic()
        age = (status or {}).get("source_age_ms") or 10**9
        fresh = bool(status and status.get("healthy") and age < STALE_MS)

        if fresh and (status.get("detected_faces") or 0) > 0:
            if now - last_seen >= IDLE_SECONDS:
                log("person detected -> waking")
            last_seen = now

        idle_for = now - last_seen
        if not fresh:
            target, state = FULL_BRIGHTNESS, "unknown"
        elif idle_for >= IDLE_SECONDS:
            target, state = DIM_BRIGHTNESS, "dim"
        else:
            target, state = FULL_BRIGHTNESS, "awake"

        if abs(target - current) > 0.02:
            if fade(current, target):
                current = target
                log("%s: brightness %.2f (idle %.0fs)" % (state, target, idle_for))
            else:
                log("warn: xrandr failed, retrying")

        time.sleep(POLL_SECONDS)


if __name__ == "__main__":
    main()
