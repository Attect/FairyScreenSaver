#!/usr/bin/env python3
"""Decode two PNGs (pure stdlib) and compare them.

Used to check the Vulkan render against the upstream SVG/CSS reference.

    python tools/pngdiff.py a.png b.png
"""

import struct
import sys
import zlib


def decode(path):
    with open(path, "rb") as fh:
        data = fh.read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path}: not a PNG")
    pos = 8
    width = height = None
    bit_depth = color_type = None
    idat = bytearray()
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos:pos + 4])
        tag = data[pos + 4:pos + 8]
        payload = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if tag == b"IHDR":
            width, height, bit_depth, color_type, _, _, interlace = struct.unpack(">IIBBBBB", payload)
            if interlace:
                raise ValueError("interlaced PNG not supported")
        elif tag == b"IDAT":
            idat += payload
        elif tag == b"IEND":
            break

    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[color_type]
    if bit_depth != 8:
        raise ValueError(f"unsupported bit depth {bit_depth}")
    raw = zlib.decompress(bytes(idat))
    stride = width * channels
    out = bytearray(width * height * 3)
    prev = bytearray(stride)
    p = 0
    for y in range(height):
        filt = raw[p]
        p += 1
        line = bytearray(raw[p:p + stride])
        p += stride
        if filt == 1:
            for i in range(channels, stride):
                line[i] = (line[i] + line[i - channels]) & 0xFF
        elif filt == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif filt == 3:
            for i in range(stride):
                a = line[i - channels] if i >= channels else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 0xFF
        elif filt == 4:
            for i in range(stride):
                a = line[i - channels] if i >= channels else 0
                b = prev[i]
                c = prev[i - channels] if i >= channels else 0
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 0xFF
        prev = line
        base = y * width * 3
        if channels == 3:
            out[base:base + stride] = line
        elif channels == 4:
            for x in range(width):
                out[base + x * 3:base + x * 3 + 3] = line[x * 4:x * 4 + 3]
        elif channels == 1:
            for x in range(width):
                v = line[x]
                out[base + x * 3:base + x * 3 + 3] = bytes((v, v, v))
        else:
            raise ValueError("unsupported channel count")
    return width, height, out


def mean_rgb(w, h, px, x0, y0, x1, y1):
    sr = sg = sb = n = 0
    for y in range(max(0, y0), min(h, y1)):
        row = y * w * 3
        for x in range(max(0, x0), min(w, x1)):
            i = row + x * 3
            sr += px[i]
            sg += px[i + 1]
            sb += px[i + 2]
            n += 1
    n = max(1, n)
    return sr / n, sg / n, sb / n


def main():
    a_path, b_path = sys.argv[1], sys.argv[2]
    aw, ah, ap = decode(a_path)
    bw, bh, bp = decode(b_path)
    print(f"{a_path}: {aw}x{ah}")
    print(f"{b_path}: {bw}x{bh}")
    if (aw, ah) != (bw, bh):
        print("  size mismatch; comparing the overlapping region only")

    w, h = min(aw, bw), min(ah, bh)
    total = n = 0
    for y in range(0, h, 3):
        row = y * w * 3
        for x in range(0, w, 3):
            i = row + x * 3
            total += abs(ap[i] - bp[i]) + abs(ap[i + 1] - bp[i + 1]) + abs(ap[i + 2] - bp[i + 2])
            n += 3
    print(f"  mean abs diff = {total / max(1, n):.2f} (0-255)")

    regions = {
        "top-left bg": (0, 0, w // 4, h // 8),
        "bottom-right bg": (w * 3 // 4, h * 7 // 8, w, h),
        "top-centre bg": (w // 2 - 200, 0, w // 2 + 200, 80),
        "eye disc": (w // 2 - 100, h // 2 - 100, w // 2 + 100, h // 2 + 100),
        "sclera ring": (w // 2 - 150, h // 2 - 20, w // 2 - 110, h // 2 + 20),
        "filament left": (w // 2 - 420, h // 2 - 60, w // 2 - 240, h // 2 + 60),
        "halo ring": (w // 2 - 230, h // 2 - 230, w // 2 + 230, h // 2 - 190),
        "whole eye box": (w // 2 - 400, h // 2 - 400, w // 2 + 400, h // 2 + 400),
    }
    for name, (x0, y0, x1, y1) in regions.items():
        ra = mean_rgb(w, h, ap, x0, y0, x1, y1)
        rb = mean_rgb(w, h, bp, x0, y0, x1, y1)
        print(f"  {name:16s} A=({ra[0]:6.1f},{ra[1]:6.1f},{ra[2]:6.1f})  B=({rb[0]:6.1f},{rb[1]:6.1f},{rb[2]:6.1f})")


if __name__ == "__main__":
    main()
