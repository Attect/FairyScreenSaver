"""Open the settings page, wait long enough, then capture its preview box.

The earlier capture came back pure white because the page had only just handed
the window to `/p` -- Vulkan pipeline creation takes a few hundred ms, so the
box is blank for the first moment.  This waits properly.

    python tools/preview_shot.py
"""

import ctypes
import subprocess
import sys
import time
from ctypes import wintypes

u = ctypes.WinDLL("user32", use_last_error=True)
u.FindWindowW.restype = wintypes.HWND
u.FindWindowW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR]
u.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
u.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
ENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
u.EnumChildWindows.argtypes = [wintypes.HWND, ENUMPROC, wintypes.LPARAM]

sys.path.insert(0, "tools")
import capture as C  # noqa: E402

DETACHED = 0x00000008

subprocess.Popen(
    ["rundll32.exe", "shell32.dll", "Control_RunDLL", "desk.cpl,,1"],
    creationflags=DETACHED,
)

hwnd = None
for _ in range(20):
    time.sleep(0.5)
    for t in ("屏幕保护程序设置", "Screen Saver Settings"):
        hwnd = u.FindWindowW(None, t)
        if hwnd:
            break
    if hwnd:
        break
if not hwnd:
    raise SystemExit("设置页没出现")

print("settings page up; waiting for the preview process to draw")
box = None
for i in range(30):
    time.sleep(0.5)
    found = []

    def visit(child, _):
        cls = ctypes.create_unicode_buffer(128)
        u.GetClassNameW(child, cls, 128)
        if "Fairy" in cls.value:
            r = wintypes.RECT()
            u.GetWindowRect(child, ctypes.byref(r))
            found.append((r.left, r.top, r.right - r.left, r.bottom - r.top))
        return True

    u.EnumChildWindows(hwnd, ENUMPROC(visit), 0)
    if found:
        box = found[0]
        if i >= 8:          # ~4 s after the window appeared
            break

if not box:
    raise SystemExit("预览窗口没出现")

x, y, w, h = box
print(f"preview box {w}x{h} at ({x},{y})")
C.capture("preview_live.png", x, y, w, h)
print("captured preview_live.png  (settings page left open on purpose)")
