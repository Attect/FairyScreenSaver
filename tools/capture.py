#!/usr/bin/env python3
"""Capture the Windows desktop to a PNG.

Development helper for verifying the screen saver's Vulkan output: DWM composites
Vulkan surfaces into the desktop image, so a plain GDI `BitBlt` from the screen DC
sees exactly what the user sees.  Pure stdlib (ctypes + zlib), no Pillow needed.

NOTE: every handle-returning call needs an explicit 64-bit restype, otherwise
ctypes truncates the pointer to `int` and BitBlt silently fails.

    python tools/capture.py out.png [left top width height]
"""

import ctypes
import struct
import sys
import zlib
from ctypes import wintypes

user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)

# Without this the helper is DPI-virtualised: window rectangles come back in
# logical units while BitBlt works in physical pixels, and every capture taken
# from a window rectangle lands in the wrong place.
try:
    ctypes.WinDLL("user32").SetProcessDPIAware()
except Exception:
    pass

SRCCOPY = 0x00CC0020
DIB_RGB_COLORS = 0
BI_RGB = 0

user32.GetDC.restype = wintypes.HDC
user32.GetDC.argtypes = [wintypes.HWND]
user32.ReleaseDC.restype = ctypes.c_int
user32.ReleaseDC.argtypes = [wintypes.HWND, wintypes.HDC]
user32.GetSystemMetrics.restype = ctypes.c_int
user32.GetSystemMetrics.argtypes = [ctypes.c_int]

gdi32.CreateCompatibleDC.restype = wintypes.HDC
gdi32.CreateCompatibleDC.argtypes = [wintypes.HDC]
gdi32.CreateCompatibleBitmap.restype = wintypes.HBITMAP
gdi32.CreateCompatibleBitmap.argtypes = [wintypes.HDC, ctypes.c_int, ctypes.c_int]
gdi32.SelectObject.restype = wintypes.HGDIOBJ
gdi32.SelectObject.argtypes = [wintypes.HDC, wintypes.HGDIOBJ]
gdi32.BitBlt.restype = wintypes.BOOL
gdi32.BitBlt.argtypes = [
    wintypes.HDC, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int,
    wintypes.HDC, ctypes.c_int, ctypes.c_int, wintypes.DWORD,
]
gdi32.GetDIBits.restype = ctypes.c_int
gdi32.GetDIBits.argtypes = [
    wintypes.HDC, wintypes.HBITMAP, wintypes.UINT, wintypes.UINT,
    ctypes.c_void_p, ctypes.c_void_p, wintypes.UINT,
]
gdi32.DeleteObject.restype = wintypes.BOOL
gdi32.DeleteObject.argtypes = [wintypes.HGDIOBJ]
gdi32.DeleteDC.restype = wintypes.BOOL
gdi32.DeleteDC.argtypes = [wintypes.HDC]


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", wintypes.DWORD),
        ("biWidth", ctypes.c_long),
        ("biHeight", ctypes.c_long),
        ("biPlanes", wintypes.WORD),
        ("biBitCount", wintypes.WORD),
        ("biCompression", wintypes.DWORD),
        ("biSizeImage", wintypes.DWORD),
        ("biXPelsPerMeter", ctypes.c_long),
        ("biYPelsPerMeter", ctypes.c_long),
        ("biClrUsed", wintypes.DWORD),
        ("biClrImportant", wintypes.DWORD),
    ]


class BITMAPINFO(ctypes.Structure):
    _fields_ = [("bmiHeader", BITMAPINFOHEADER), ("bmiColors", wintypes.DWORD * 3)]


def write_png(path, width, height, bgra_rows):
    raw = bytearray()
    for row in bgra_rows:
        raw.append(0)  # filter type 0
        for x in range(0, len(row), 4):
            b, g, r = row[x], row[x + 1], row[x + 2]
            raw += bytes((r, g, b))

    def chunk(tag, payload):
        data = tag + payload
        return struct.pack(">I", len(payload)) + data + struct.pack(">I", zlib.crc32(data) & 0xFFFFFFFF)

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 6))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as fh:
        fh.write(png)


def capture(path, left=None, top=None, width=None, height=None):
    hdc_screen = user32.GetDC(None)
    if not hdc_screen:
        raise OSError("GetDC failed")

    if width is None:
        width = user32.GetSystemMetrics(0)
        height = user32.GetSystemMetrics(1)
        left, top = 0, 0

    hdc_mem = gdi32.CreateCompatibleDC(hdc_screen)
    hbmp = gdi32.CreateCompatibleBitmap(hdc_screen, width, height)
    if not hdc_mem or not hbmp:
        raise OSError("GDI object creation failed")
    gdi32.SelectObject(hdc_mem, hbmp)

    if not gdi32.BitBlt(hdc_mem, 0, 0, width, height, hdc_screen, left, top, SRCCOPY):
        raise OSError(f"BitBlt failed ({ctypes.get_last_error()})")

    info = BITMAPINFO()
    info.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    info.bmiHeader.biWidth = width
    info.bmiHeader.biHeight = -height  # negative => top-down rows
    info.bmiHeader.biPlanes = 1
    info.bmiHeader.biBitCount = 32
    info.bmiHeader.biCompression = BI_RGB

    stride = width * 4
    buf = ctypes.create_string_buffer(stride * height)
    got = gdi32.GetDIBits(hdc_mem, hbmp, 0, height, buf, ctypes.byref(info), DIB_RGB_COLORS)
    if got == 0:
        raise OSError(f"GetDIBits failed ({ctypes.get_last_error()})")

    data = buf.raw
    rows = [data[y * stride:(y + 1) * stride] for y in range(height)]
    write_png(path, width, height, rows)

    gdi32.DeleteObject(hbmp)
    gdi32.DeleteDC(hdc_mem)
    user32.ReleaseDC(None, hdc_screen)

    sr = sg = sb = 0
    samples = 0
    for y in range(0, height, max(1, height // 60)):
        row = rows[y]
        for x in range(0, width, max(1, width // 60)):
            sr += row[x * 4 + 2]
            sg += row[x * 4 + 1]
            sb += row[x * 4]
            samples += 1
    mean = (sr + sg + sb) / max(1, samples * 3)
    print(f"captured {width}x{height} at ({left},{top}) -> {path}")
    print(f"  mean RGB = ({sr/samples:.1f}, {sg/samples:.1f}, {sb/samples:.1f})  mean={mean:.1f}")
    return mean


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "capture.png"
    if len(sys.argv) == 6:
        capture(out, *(int(v) for v in sys.argv[2:6]))
    else:
        capture(out)
