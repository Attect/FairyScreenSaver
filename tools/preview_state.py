"""Inspect the live preview child window inside the screen saver settings page.

Answers three things at once: is a preview host present, what size is it, and
what is actually inside it (by capturing just that rectangle).

    python tools/preview_state.py

Note: the settings page only spawns `/p` when it is *opened*.  Switching the
dropdown does not restart the preview, and once the process dies the page does
not replace it -- so open the page fresh if this reports nothing.
"""

import ctypes
import sys
from ctypes import wintypes

u = ctypes.WinDLL("user32", use_last_error=True)
u.FindWindowW.restype = wintypes.HWND
u.FindWindowW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR]
u.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
u.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
u.IsWindowVisible.argtypes = [wintypes.HWND]

ENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
u.EnumChildWindows.argtypes = [wintypes.HWND, ENUMPROC, wintypes.LPARAM]

sys.path.insert(0, "tools")
import capture as C  # noqa: E402

hwnd = None
for title in ("屏幕保护程序设置", "Screen Saver Settings"):
    hwnd = u.FindWindowW(None, title)
    if hwnd:
        break
if not hwnd:
    raise SystemExit("设置页没打开 —— 先手动打开『屏幕保护程序设置』再跑这个")

found = []


def visit(child, _):
    cls = ctypes.create_unicode_buffer(128)
    u.GetClassNameW(child, cls, 128)
    if "Fairy" in cls.value or "SSDemo" in cls.value:
        r = wintypes.RECT()
        u.GetWindowRect(child, ctypes.byref(r))
        found.append((cls.value, child, r.left, r.top,
                      r.right - r.left, r.bottom - r.top, bool(u.IsWindowVisible(child))))
    return True


u.EnumChildWindows(hwnd, ENUMPROC(visit), 0)

if not found:
    print("没有找到预览窗口 —— 说明 Windows 此刻没有为这个屏保启动 /p")
    sys.exit(0)

for cls, child, x, y, w, h, vis in found:
    print(f"{cls:<32} hwnd={child} rect=({x},{y}) {w}x{h} visible={vis}")

# Capture just the preview host rectangle.
for cls, child, x, y, w, h, vis in found:
    if "Fairy" in cls:
        C.capture("preview_live.png", x, y, w, h)
        print(f"captured preview_live.png from {cls}")
