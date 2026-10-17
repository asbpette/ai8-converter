#!/usr/bin/env python3
"""Generate scripts/windows/ai8.ico (standard library only).

Design: rounded square with an orange gradient and the letters "AI8" drawn
as thick white strokes. Rendered with supersampled signed distances, written
as a multi-size ICO: 16/24/32/48/64 px as 32-bit BMP (DIB) entries and 256 px
as a PNG entry (what Windows Vista+ and NSIS expect).

Usage: python3 scripts/windows/make-icon.py [out.ico]
"""
import math
import os
import struct
import sys
import zlib


def seg_dist(px, py, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    t = ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)
    t = max(0.0, min(1.0, t))
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))


def rrect_sd(px, py, x0, y0, x1, y1, r):
    """Signed distance to a rounded rectangle (negative inside)."""
    cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
    hx, hy = (x1 - x0) / 2 - r, (y1 - y0) / 2 - r
    qx, qy = abs(px - cx) - hx, abs(py - cy) - hy
    outside = math.hypot(max(qx, 0), max(qy, 0))
    return outside + min(max(qx, qy), 0) - r


# Glyph geometry in a unit square (0..1, y down). Stroke half-width W.
W = 0.045
SEGMENTS = [
    # A
    (0.13, 0.70, 0.25, 0.30), (0.25, 0.30, 0.37, 0.70), (0.18, 0.56, 0.32, 0.56),
    # I
    (0.47, 0.30, 0.47, 0.70),
]
# 8: two stacked rings (cx, cy, radius)
RINGS = [(0.705, 0.395, 0.095), (0.705, 0.600, 0.110)]


def coverage_glyph(u, v):
    d = min(seg_dist(u, v, *s) for s in SEGMENTS)
    for cx, cy, r in RINGS:
        d = min(d, abs(math.hypot(u - cx, v - cy) - r))
    return d <= W


def render(size):
    ss = 4 if size >= 64 else 6
    px = []
    for y in range(size):
        row = []
        for x in range(size):
            acc_bg = acc_fg = 0
            for sy in range(ss):
                for sx in range(ss):
                    u = (x + (sx + 0.5) / ss) / size
                    v = (y + (sy + 0.5) / ss) / size
                    if rrect_sd(u, v, 0.03, 0.03, 0.97, 0.97, 0.20) <= 0:
                        acc_bg += 1
                        if coverage_glyph(u, v):
                            acc_fg += 1
            n = ss * ss
            a = acc_bg / n
            f = acc_fg / max(acc_bg, 1)
            t = (y + 0.5) / size  # vertical gradient
            br, bg, bb = (255 - 25 * t, 154 - 60 * t, 0 + 10 * t)  # orange -> deep orange
            r = br + (255 - br) * f
            g = bg + (255 - bg) * f
            b = bb + (255 - bb) * f
            if size <= 16:  # small icon: darker background for contrast
                r, g, b = r * 0.95, g * 0.95, b
            row.append((round(r), round(g), round(b), round(255 * a)))
        px.append(row)
    return px


def png_bytes(px):
    h, w = len(px), len(px[0])
    raw = b"".join(b"\0" + bytes(c for p in row for c in p) for row in px)

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def dib_bytes(px):
    h, w = len(px), len(px[0])
    hdr = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    xor = b"".join(bytes((b, g, r, a)) for row in reversed(px) for (r, g, b, a) in row)
    mask_row = ((w + 31) // 32) * 4
    andm = b"".join(
        bytes(
            sum((0x80 >> (i % 8)) for i in range(byte * 8, min(byte * 8 + 8, w)) if row[i][3] == 0)
            for byte in range(mask_row)
        )
        for row in reversed(px)
    )
    return hdr + xor + andm


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "ai8.ico")
    sizes = [16, 24, 32, 48, 64, 256]
    images = []
    for s in sizes:
        px = render(s)
        images.append((s, png_bytes(px) if s >= 256 else dib_bytes(px)))
    data = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    body = b""
    for s, img in images:
        data += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(img), offset)
        offset += len(img)
        body += img
    with open(out, "wb") as f:
        f.write(data + body)
    print(f"wrote {out} ({len(data + body)} bytes)")


if __name__ == "__main__":
    main()
