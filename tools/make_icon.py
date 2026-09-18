#!/usr/bin/env python3
"""Turn a screen capture of the saver into a multi-resolution Windows .ico.

The eye is procedural, so rather than hand-authoring an icon we render it once at
454px and box-filter that down — which keeps the lashes, iris and highlight
correct at every size.

    python tools/make_icon.py centered.png assets/app.ico [eye_px]
"""

import os
import struct
import sys
import zlib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pngdiff import decode  # noqa: E402

# Eye geometry in the shader's 160-unit space.
VIEWBOX = 160.0
DISC_RADIUS = 68.0
FADE_IN = 67.0     # alpha is 1 inside this radius ...
FADE_OUT = 77.0    # ... and 0 outside this one

SIZES = [16, 24, 32, 48, 64, 128, 256]
PNG_FROM = 128     # PNG-compressed entries for the large sizes


def smoothstep(a, b, x):
    t = min(1.0, max(0.0, (x - a) / (b - a)))
    return t * t * (3.0 - 2.0 * t)


def crop_rgba(w, h, px, eye_px, crop):
    """Square RGBA crop around the eye, with a soft alpha edge."""
    scale = eye_px / VIEWBOX          # pixels per eye unit
    cx, cy = w / 2.0, h / 2.0
    x0 = int(cx - crop / 2)
    y0 = int(cy - crop / 2)

    out = bytearray(crop * crop * 4)
    for y in range(crop):
        sy = y0 + y
        if sy < 0 or sy >= h:
            continue
        row = sy * w * 3
        o = y * crop * 4
        for x in range(crop):
            sx = x0 + x
            if sx < 0 or sx >= w:
                continue
            i = row + sx * 3
            du = ((sx - cx) / scale) ** 2 + ((sy - cy) / scale) ** 2
            a = 1.0 - smoothstep(FADE_IN, FADE_OUT, du ** 0.5)
            oi = o + x * 4
            if a <= 0.0:
                continue
            # Premultiply so the box filter below averages correctly.
            out[oi] = int(px[i] * a)
            out[oi + 1] = int(px[i + 1] * a)
            out[oi + 2] = int(px[i + 2] * a)
            out[oi + 3] = int(round(a * 255))
    return out, crop


def box_downscale(rgba, size, target):
    """Area-average RGBA (premultiplied) from `size` to `target`."""
    out = bytearray(target * target * 4)
    step = size / target
    for ty in range(target):
        y0 = int(ty * step)
        y1 = max(y0 + 1, int((ty + 1) * step))
        for tx in range(target):
            x0 = int(tx * step)
            x1 = max(x0 + 1, int((tx + 1) * step))
            sr = sg = sb = sa = n = 0
            for sy in range(y0, min(y1, size)):
                row = sy * size * 4
                for sx in range(x0, min(x1, size)):
                    i = row + sx * 4
                    sr += rgba[i]
                    sg += rgba[i + 1]
                    sb += rgba[i + 2]
                    sa += rgba[i + 3]
                    n += 1
            n = max(1, n)
            a = sa / n
            o = (ty * target + tx) * 4
            if a > 0.5:
                inv = 255.0 / a
                out[o] = min(255, int(sr / n * inv))
                out[o + 1] = min(255, int(sg / n * inv))
                out[o + 2] = min(255, int(sb / n * inv))
            out[o + 3] = int(round(a))
    return out


def png_bytes(rgba, size):
    from capture import write_png
    rows = []
    for y in range(size):
        row = bytearray()
        for x in range(size):
            i = (y * size + x) * 4
            row += bytes((rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]))
        rows.append(bytes(row))
    tmp = "__icon_tmp.png"
    write_png(tmp, size, size, rows)
    with open(tmp, "rb") as fh:
        data = fh.read()
    os.remove(tmp)
    return data


def bmp_bytes(rgba, size):
    header = struct.pack(
        "<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, size * size * 4, 0, 0, 0, 0
    )
    body = bytearray()
    for y in range(size - 1, -1, -1):          # BMP rows are bottom-up
        for x in range(size):
            i = (y * size + x) * 4
            body += bytes((rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]))
    mask_stride = ((size + 31) // 32) * 4       # 1bpp AND mask, 4-byte aligned
    mask = b"\x00" * (mask_stride * size)       # alpha channel already handles it
    return header + bytes(body) + mask


def build_ico(source, out_path, eye_px=454.0):
    w, h, px = decode(source)
    crop = 430
    rgba, size = crop_rgba(w, h, px, eye_px, crop)

    entries = []
    for s in SIZES:
        small = box_downscale(rgba, size, s) if s != size else rgba
        data = png_bytes(small, s) if s >= PNG_FROM else bmp_bytes(small, s)
        entries.append((s, data))

    blob = struct.pack("<HHH", 0, 1, len(entries))
    offset = 6 + 16 * len(entries)
    dirs = b""
    for s, data in entries:
        dirs += struct.pack(
            "<BBBBHHII", s if s < 256 else 0, s if s < 256 else 0, 0, 0, 1, 32, len(data), offset
        )
        offset += len(data)

    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    with open(out_path, "wb") as fh:
        fh.write(blob + dirs + b"".join(d for _, d in entries))
    total = sum(len(d) for _, d in entries)
    print(f"wrote {out_path}: {len(entries)} sizes {SIZES}, {total} bytes of image data")


if __name__ == "__main__":
    src = sys.argv[1] if len(sys.argv) > 1 else "centered.png"
    dst = sys.argv[2] if len(sys.argv) > 2 else "assets/app.ico"
    eye = float(sys.argv[3]) if len(sys.argv) > 3 else 454.0
    build_ico(src, dst, eye)
