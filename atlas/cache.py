"""Disk cache with a minimum age: fresh entries are never re-fetched, stale ones
are served when the network fails."""
import os
import tempfile
import time


def is_fresh(mtime, now, max_age):
    return now - mtime < max_age


class DiskCache:
    def __init__(self, root, clock=time.time):
        self.root = os.path.abspath(str(root))
        self.clock = clock

    def path(self, key):
        path = os.path.abspath(os.path.join(self.root, key))
        if not path.startswith(self.root + os.sep):
            raise ValueError("cache key outside the cache: %r" % key)
        return path

    def get(self, key):
        path = self.path(key)
        try:
            with open(path, "rb") as f:
                return f.read(), os.fstat(f.fileno()).st_mtime
        except FileNotFoundError:
            return None

    def put(self, key, data):
        path = self.path(key)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), prefix=".tmp-")
        try:
            with os.fdopen(fd, "wb") as f:
                f.write(data)
            os.replace(tmp, path)
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise

    def fetch(self, key, max_age, download, validate=None):
        """Cached bytes when younger than max_age, else download (stale on failure)."""
        hit = self.get(key)
        if hit is not None and is_fresh(hit[1], self.clock(), max_age):
            return hit[0]
        try:
            data = download()
            if validate is not None:
                validate(data)
        except Exception:
            if hit is not None:
                return hit[0]
            raise
        self.put(key, data)
        return data
