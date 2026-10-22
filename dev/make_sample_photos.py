"""Generate sample photos for a local run (dev only; nothing here ships).

    python dev/make_sample_photos.py <dir> [count]

Some images carry EXIF GPS and DateTimeOriginal so the photo-place view and
card labels can be checked without real photos.
"""
import math
import os
import random
import sys

from PIL import Image, ImageDraw, ImageFilter

PLACES = [
    (35.0116, 135.7681, "2025:08:14 10:21:00"),  # Kyoto
    (43.0621, 141.3544, "2025:02:08 15:02:00"),  # Sapporo
    (48.8566, 2.3522, "2024:05:03 18:40:00"),  # Paris
    (26.2124, 127.6809, "2025:07:01 12:00:00"),  # Naha
]
SIZES = [(1600, 1067), (1067, 1600), (1600, 900), (1200, 1200)]


def rational(value):
    deg = int(value)
    minutes_float = (value - deg) * 60
    minutes = int(minutes_float)
    seconds = round((minutes_float - minutes) * 60 * 100)
    return (float(deg), float(minutes), seconds / 100)


def scene(w, h, seed):
    rnd = random.Random(seed)
    top = tuple(rnd.randint(40, 200) for _ in range(3))
    bottom = tuple(rnd.randint(10, 120) for _ in range(3))
    img = Image.new("RGB", (w, h))
    draw = ImageDraw.Draw(img)
    for y in range(h):
        t = y / h
        draw.line([(0, y), (w, y)], fill=tuple(int(a + (b - a) * t) for a, b in zip(top, bottom)))
    for layer in range(3):
        base = h * (0.55 + 0.12 * layer)
        pts = [(0, h)]
        for x in range(0, w + 40, 40):
            pts.append((x, base - 80 * math.sin(x / (120 + 60 * layer) + seed) - rnd.randint(0, 30)))
        pts.append((w, h))
        shade = tuple(max(0, c - 40 * (layer + 1)) for c in bottom)
        draw.polygon(pts, fill=shade)
    sun = (rnd.randint(w // 5, 4 * w // 5), rnd.randint(h // 8, h // 3))
    draw.ellipse([sun[0] - 50, sun[1] - 50, sun[0] + 50, sun[1] + 50], fill=(255, 230, 180))
    return img.filter(ImageFilter.GaussianBlur(1.2))


def main():
    out = sys.argv[1]
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 12
    os.makedirs(out, exist_ok=True)
    for i in range(count):
        w, h = SIZES[i % len(SIZES)]
        img = scene(w, h, i)
        exif = Image.Exif()
        if i % 3 != 2:
            lat, lon, taken = PLACES[i % len(PLACES)]
            gps = exif.get_ifd(0x8825)
            gps[1] = "N" if lat >= 0 else "S"
            gps[2] = rational(abs(lat))
            gps[3] = "E" if lon >= 0 else "W"
            gps[4] = rational(abs(lon))
            exif.get_ifd(0x8769)[36867] = taken
        img.save(os.path.join(out, "sample-%02d.jpg" % i), "JPEG", quality=88, exif=exif)
    print("wrote %d photos to %s" % (count, out))


if __name__ == "__main__":
    main()
