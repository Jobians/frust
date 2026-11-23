"""Writes assets/sample.png, the bundled sample image, from nothing but arithmetic.

A shaded ball in front of a block on a tiled floor under a sky: one clear subject for the
mask, a floor that recedes for the depth map, and no person. Standard library only:

    python3 -I assets/make_sample.py assets/sample.png

The output is deterministic, so running it again reproduces the committed file byte for byte.
"""

import math
import struct
import sys
import zlib

WIDTH, HEIGHT = 640, 480
HORIZON = 250


def clamp(value):
    return max(0, min(255, int(round(value))))


def sky(y):
    t = y / HORIZON
    return (90 + 110 * t, 150 + 80 * t, 230 + 15 * t)


def floor(x, y):
    # Perspective tiles: rows get thinner towards the horizon.
    depth = 2400.0 / (y - HORIZON + 8)
    u = (x - WIDTH / 2) * depth / 200.0
    v = depth
    tile = (math.floor(u) + math.floor(v)) % 2
    shade = 0.55 + 0.45 * (y - HORIZON) / (HEIGHT - HORIZON)
    light, dark = (200, 190, 170), (120, 110, 100)
    base = light if tile else dark
    # Far tiles fade to their average, so the horizon does not alias into stripes.
    near = min(1.0, (y - HORIZON) / 70.0)
    mixed = tuple(b * near + (l + d) / 2 * (1 - near) for b, l, d in zip(base, light, dark))
    return tuple(c * shade for c in mixed)


def block(x, y):
    # A flat-faced block behind the ball, left of centre.
    left, right, top, bottom = 90, 250, 150, 300
    if left <= x < right and top <= y < bottom:
        face = 0.75 + 0.25 * (x - left) / (right - left)
        return (40 * face, 140 * face, 150 * face)
    return None


def ball(x, y):
    cx, cy, r = 390, 300, 120
    dx, dy = (x - cx) / r, (y - cy) / r
    d2 = dx * dx + dy * dy
    if d2 > 1.0:
        return None
    dz = math.sqrt(1.0 - d2)
    # Light from the upper left, a little towards the viewer.
    lx, ly, lz = -0.5, -0.6, 0.62
    diffuse = max(0.0, dx * lx + dy * ly + dz * lz)
    spec = max(0.0, dz * 0.9 + (dx * lx + dy * ly) * 0.4) ** 40
    light = 0.25 + 0.75 * diffuse
    return (235 * light + 200 * spec, 120 * light + 200 * spec, 40 * light + 200 * spec)


def shadow(x, y):
    # A soft ellipse under the ball.
    dx, dy = (x - 380) / 135.0, (y - 415) / 28.0
    d = dx * dx + dy * dy
    return 1.0 - 0.45 * max(0.0, 1.0 - d)


def pixel(x, y):
    hit = ball(x, y)
    if hit is not None:
        return hit
    hit = block(x, y)
    if hit is not None:
        return hit
    if y < HORIZON:
        return sky(y)
    s = shadow(x, y)
    return tuple(c * s for c in floor(x, y))


def png(rows):
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    raw = bytearray()
    previous = bytes(WIDTH * 3)
    for row in rows:
        # Filter type 2 (Up): smooth shading compresses far better as row deltas.
        raw.append(2)
        raw.extend((a - b) & 0xFF for a, b in zip(row, previous))
        previous = row
    header = struct.pack(">IIBBBBB", WIDTH, HEIGHT, 8, 2, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "sample.png"
    rows = []
    for y in range(HEIGHT):
        row = bytearray()
        for x in range(WIDTH):
            row.extend(clamp(c) for c in pixel(x, y))
        rows.append(bytes(row))
    with open(out, "wb") as f:
        f.write(png(rows))


if __name__ == "__main__":
    main()
