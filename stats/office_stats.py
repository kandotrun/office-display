#!/usr/bin/env python3
"""Office statistics for the 2F display.

Collects a one-minute aggregate of the signals the display already measures:
activity (frame-to-frame motion) and loudness (microphone level), plus the
number of faces the detector reported. Nothing else is stored: no images, no
identity, no per-person records. Rows are aggregate numbers keyed by minute.

Serves the aggregates as JSON for the drawing service:

    GET /stats/today            per-hour profile for today
    GET /stats/week             7x24 activity grid
    GET /stats/recent?minutes=N last N one-minute buckets
    GET /status                 collector state
"""
import json
import os
import sqlite3
import threading
import time
import urllib.request
from datetime import datetime, timedelta
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SURVEY_STATUS = os.environ.get("STATS_SURVEY_URL", "http://127.0.0.1:8094/status")
FACES_STATUS = os.environ.get("STATS_FACES_URL", "http://127.0.0.1:8093/status")
PORT = int(os.environ.get("STATS_PORT", "8095"))
DB_PATH = os.environ.get("STATS_DB", os.path.expanduser("~/office-stats/stats.db"))
POLL_SECONDS = float(os.environ.get("STATS_POLL_SECONDS", "10"))
RETENTION_DAYS = int(os.environ.get("STATS_RETENTION_DAYS", "120"))
ACTIVE_MOTION = float(os.environ.get("STATS_ACTIVE_MOTION", "1.5"))

SCHEMA = """
CREATE TABLE IF NOT EXISTS samples (
    ts          INTEGER PRIMARY KEY,   -- unix seconds at the start of the minute
    motion      REAL NOT NULL,         -- running mean activity
    motion_peak REAL NOT NULL,
    db          REAL NOT NULL,         -- running mean loudness
    db_peak     REAL NOT NULL,
    faces       INTEGER NOT NULL,      -- highest face count seen in the minute
    n           INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS samples_ts ON samples(ts);
"""

LOCK = threading.Lock()
CONN = None
STATE = {"last_poll": 0.0, "last_error": None, "rows": 0}


def connect():
    os.makedirs(os.path.dirname(DB_PATH), exist_ok=True)
    conn = sqlite3.connect(DB_PATH, check_same_thread=False)
    conn.executescript(SCHEMA)
    conn.commit()
    return conn


def fetch_json(url):
    try:
        with urllib.request.urlopen(url, timeout=3) as response:
            return json.load(response)
    except Exception:
        return None


def poll_once():
    survey = fetch_json(SURVEY_STATUS)
    if survey is None:
        return None
    faces = fetch_json(FACES_STATUS) or {}
    return {
        "motion": float(survey.get("motion") or 0.0),
        "db": float(survey.get("db") or -90.0),
        "faces": int(faces.get("detected_faces") or 0) if faces.get("healthy") else 0,
    }


def record(sample):
    minute = int(time.time()) // 60 * 60
    with LOCK:
        CONN.execute(
            """
            INSERT INTO samples (ts, motion, motion_peak, db, db_peak, faces, n)
            VALUES (?, ?, ?, ?, ?, ?, 1)
            ON CONFLICT(ts) DO UPDATE SET
                motion      = (samples.motion * samples.n + excluded.motion) / (samples.n + 1),
                motion_peak = MAX(samples.motion_peak, excluded.motion_peak),
                db          = (samples.db * samples.n + excluded.db) / (samples.n + 1),
                db_peak     = MAX(samples.db_peak, excluded.db_peak),
                faces       = MAX(samples.faces, excluded.faces),
                n           = samples.n + 1
            """,
            (minute, sample["motion"], sample["motion"], sample["db"], sample["db"], sample["faces"]),
        )
        CONN.execute("DELETE FROM samples WHERE ts < ?", (int(time.time()) - RETENTION_DAYS * 86400,))
        CONN.commit()


def collect():
    while True:
        sample = poll_once()
        if sample is None:
            STATE["last_error"] = "survey_unreachable"
        else:
            STATE["last_error"] = None
            record(sample)
            STATE["last_poll"] = time.time()
        time.sleep(POLL_SECONDS)


def rows(sql, args=()):
    with LOCK:
        cur = CONN.execute(sql, args)
        return [dict(zip([d[0] for d in cur.description], r)) for r in cur.fetchall()]


def week_grid():
    """7x24 activity grid: weekday (0=Sunday) x hour, in local time."""
    since = int(time.time()) - 28 * 86400
    data = rows(
        """
        SELECT CAST(strftime('%w', ts, 'unixepoch', 'localtime') AS INTEGER) AS weekday,
               CAST(strftime('%H', ts, 'unixepoch', 'localtime') AS INTEGER) AS hour,
               AVG(motion) AS motion,
               AVG(db) AS db,
               MAX(faces) AS faces,
               COUNT(*) AS minutes
        FROM samples WHERE ts >= ?
        GROUP BY weekday, hour
        """,
        (since,),
    )
    grid = [[None for _ in range(24)] for _ in range(7)]
    for r in data:
        grid[r["weekday"]][r["hour"]] = {
            "motion": round(r["motion"], 3),
            "db": round(r["db"], 1),
            "faces": r["faces"],
            "minutes": r["minutes"],
        }
    return grid


def today_profile():
    start = int(datetime.now().replace(hour=0, minute=0, second=0, microsecond=0).timestamp())
    data = rows(
        """
        SELECT CAST(strftime('%H', ts, 'unixepoch', 'localtime') AS INTEGER) AS hour,
               AVG(motion) AS motion, AVG(db) AS db, MAX(faces) AS faces, COUNT(*) AS minutes
        FROM samples WHERE ts >= ?
        GROUP BY hour
        """,
        (start,),
    )
    profile = [None for _ in range(24)]
    for r in data:
        profile[r["hour"]] = {
            "motion": round(r["motion"], 3),
            "db": round(r["db"], 1),
            "faces": r["faces"],
            "minutes": r["minutes"],
        }
    return profile


def day_report():
    """Everything the daily report card shows, for today."""
    now = datetime.now()
    start = int(now.replace(hour=0, minute=0, second=0, microsecond=0).timestamp())
    end = start + 86400
    hourly = rows(
        """
        SELECT CAST(strftime('%H', ts, 'unixepoch', 'localtime') AS INTEGER) AS hour,
               AVG(motion) AS motion, AVG(db) AS db, MAX(db_peak) AS db_peak,
               MAX(faces) AS faces, COUNT(*) AS minutes
        FROM samples WHERE ts >= ? AND ts < ? GROUP BY hour ORDER BY hour
        """,
        (start, end),
    )
    hours = [None for _ in range(24)]
    for r in hourly:
        hours[r["hour"]] = {
            "motion": round(r["motion"], 3),
            "db": round(r["db"], 1),
            "db_peak": round(r["db_peak"], 1),
            "faces": r["faces"],
            "minutes": r["minutes"],
        }
    used = [(h, v) for h, v in enumerate(hours) if v and v["minutes"] >= 2]
    # Comparing hours needs more than one hour of data to mean anything.
    comparable = len(used) >= 3
    busiest = max(used, key=lambda hv: hv[1]["motion"]) if comparable else None
    quietest = min(used, key=lambda hv: hv[1]["motion"]) if comparable else None

    minutes = rows(
        "SELECT motion FROM samples WHERE ts >= ? AND ts < ? ORDER BY ts",
        (start, end),
    )
    active_minutes = sum(1 for m in minutes if m["motion"] >= ACTIVE_MOTION)
    total_minutes = len(minutes)

    last_week_day = now - timedelta(days=7)
    lw_start = int(last_week_day.replace(hour=0, minute=0, second=0, microsecond=0).timestamp())
    lw = rows(
        "SELECT AVG(motion) AS motion, COUNT(*) AS minutes FROM samples WHERE ts >= ? AND ts < ?",
        (lw_start, lw_start + 86400),
    )
    last_week = None
    if lw and lw[0]["minutes"] >= 30 and total_minutes >= 30:
        today_avg = sum(m["motion"] for m in minutes) / total_minutes
        if lw[0]["motion"]:
            last_week = round(today_avg / lw[0]["motion"], 2)

    return {
        "date": now.strftime("%Y-%m-%d"),
        "weekday": now.strftime("%a"),
        "hours": hours,
        "busiest": None if not busiest else {"hour": busiest[0], "motion": busiest[1]["motion"]},
        "quietest": None if not quietest else {"hour": quietest[0], "motion": quietest[1]["motion"]},
        "first_hour": used[0][0] if used else None,
        "last_hour": used[-1][0] if used else None,
        "active_minutes": active_minutes,
        "total_minutes": total_minutes,
        "peak_db": max((v["db_peak"] for h, v in used), default=None),
        "last_week": last_week,
    }


def recent(minutes):
    since = int(time.time()) - minutes * 60
    return rows(
        """
        SELECT ts, ROUND(motion,3) AS motion, ROUND(db,1) AS db, faces
        FROM samples WHERE ts >= ? ORDER BY ts
        """,
        (since,),
    )


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, *args):
        pass

    def send_json(self, payload):
        body = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/stats/week":
            self.send_json({"grid": week_grid(), "now": datetime.now().isoformat(timespec="seconds")})
        elif path == "/stats/today":
            self.send_json({"hours": today_profile(), "now": datetime.now().isoformat(timespec="seconds")})
        elif path == "/stats/report":
            self.send_json(day_report())
        elif path == "/stats/recent":
            minutes = 60
            if "minutes=" in self.path:
                try:
                    minutes = max(1, min(1440, int(self.path.split("minutes=")[1].split("&")[0])))
                except ValueError:
                    pass
            self.send_json({"minutes": minutes, "points": recent(minutes)})
        elif path == "/status":
            with LOCK:
                total = CONN.execute("SELECT COUNT(*) FROM samples").fetchone()[0]
            STATE["rows"] = total
            self.send_json(STATE)
        else:
            self.send_error(404)


def main():
    global CONN
    CONN = connect()
    threading.Thread(target=collect, name="collect", daemon=True).start()
    print("office-stats on 127.0.0.1:%d db=%s" % (PORT, DB_PATH), flush=True)
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()


if __name__ == "__main__":
    main()
