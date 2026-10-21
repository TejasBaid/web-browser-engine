#!/usr/bin/env python3
"""Generate the small test images used by tests/reftests/ (standard library only).

Writes PNGs directly (zlib + struct) and converts to JPEG/GIF with macOS `sips`.

Usage: python3 scripts/gen-reftest-images.py tests/reftests/images
"""
import math
import os
import struct
import subprocess
import sys
import zlib


def write_png(path, width, height, pixels, alpha):
    """pixels: function (x, y) -> (r, g, b) or (r, g, b, a)."""
    channels = 4 if alpha else 3
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        for x in range(width):
            raw.extend(pixels(x, y)[:channels])

    def chunk(kind, data):
        c = kind + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6 if alpha else 2, 0, 0, 0)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", ihdr))
        f.write(chunk(b"IDAT", zlib.compress(bytes(raw), 9)))
        f.write(chunk(b"IEND", b""))


def quadrants(x, y):
    # 64x48: red / green / blue / yellow quadrants with a 2px black frame.
    if x < 2 or y < 2 or x >= 62 or y >= 46:
        return (0, 0, 0)
    left, top = x < 32, y < 24
    if top and left:
        return (230, 40, 40)
    if top:
        return (40, 180, 70)
    if left:
        return (40, 90, 220)
    return (250, 210, 30)


def photo(x, y):
    # 100x100 smooth "photo-like" pattern: sky, sun and hills.
    hill = 62 + 10 * math.sin(x / 14.0)
    if y > hill:
        g = 120 + int(40 * math.sin((x + y) / 9.0))
        return (40, g, 50)
    d = math.hypot(x - 72, y - 26)
    if d < 13:
        return (255, 220, 60)
    t = y / 70.0
    return (int(90 + 100 * t), int(160 + 60 * t), 245)


def alpha_disc(x, y):
    # 80x80 magenta disc whose opacity falls off towards the edge; transparent corners.
    d = math.hypot(x - 39.5, y - 39.5)
    if d > 38:
        return (0, 0, 0, 0)
    a = int(255 * (1 - d / 38) ** 0.5)
    return (200, 30, 160, a)


def checker(x, y):
    # 40x40 checkerboard (8px squares) for the GIF.
    return (20, 20, 20) if ((x // 8) + (y // 8)) % 2 == 0 else (255, 140, 0)


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "tests/reftests/images"
    os.makedirs(out, exist_ok=True)
    write_png(os.path.join(out, "quadrants.png"), 64, 48, quadrants, False)
    write_png(os.path.join(out, "alpha.png"), 80, 80, alpha_disc, True)
    tmp_photo = os.path.join(out, "_photo.png")
    tmp_checker = os.path.join(out, "_checker.png")
    write_png(tmp_photo, 100, 100, photo, False)
    write_png(tmp_checker, 40, 40, checker, False)
    subprocess.run(["sips", "-s", "format", "jpeg", "-s", "formatOptions", "90", tmp_photo,
                    "--out", os.path.join(out, "photo.jpg")], check=True, stdout=subprocess.DEVNULL)
    subprocess.run(["sips", "-s", "format", "gif", tmp_checker, "--out", os.path.join(out, "checker.gif")],
                   check=True, stdout=subprocess.DEVNULL)
    os.remove(tmp_photo)
    os.remove(tmp_checker)


if __name__ == "__main__":
    main()
