"""Convert the saver's diagnostics BMP into a PNG the chat client can display.

The saver deliberately writes plain 24-bit BMP: it is a few lines of Rust with
no image crate attached, and the point of the probe is to work on a machine that
has nothing installed.  This side of the fence is where convenience wins.
"""

import struct
import sys
import zlib


def chunk(kind: bytes, payload: bytes) -> bytes:
    return (
        struct.pack(">I", len(payload))
        + kind
        + payload
        + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)
    )


def bmp_to_png(src: str, dst: str) -> None:
    data = open(src, "rb").read()
    if data[:2] != b"BM":
        raise SystemExit(f"{src}: not a BMP")

    pixel_offset = struct.unpack("<I", data[10:14])[0]
    width, height = struct.unpack("<ii", data[18:26])
    bpp = struct.unpack("<H", data[28:30])[0]
    if bpp != 24:
        raise SystemExit(f"{src}: expected 24bpp, got {bpp}")

    top_down = height < 0
    height = abs(height)
    stride = ((width * 3 + 3) // 4) * 4

    rows = []
    for y in range(height):
        start = pixel_offset + y * stride
        rows.append(data[start : start + width * 3])
    # BMP stores bottom-up unless the height is negative; PNG is always top-down.
    if not top_down:
        rows.reverse()

    raw = bytearray()
    for row in rows:
        raw.append(0)  # filter type 0 (None)
        for i in range(0, width * 3, 3):
            raw += bytes((row[i + 2], row[i + 1], row[i]))  # BGR -> RGB

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 6))
    png += chunk(b"IEND", b"")
    open(dst, "wb").write(png)
    print(f"{src} -> {dst}  {width}x{height}")


if __name__ == "__main__":
    if len(sys.argv) < 3:
        raise SystemExit("usage: bmp2png.py <in.bmp> <out.png> [more pairs...]")
    pairs = sys.argv[1:]
    for i in range(0, len(pairs) - 1, 2):
        bmp_to_png(pairs[i], pairs[i + 1])
