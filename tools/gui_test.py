#!/usr/bin/env python3
"""End-to-end checks for the two GUI entry points of the screen saver.

* `/p <hwnd>` — the live thumbnail the Windows settings page embeds.  We host it
  inside Notepad so we get a real parent window without needing a UI toolkit.
* `/c`        — the configuration window.

    python tools/gui_test.py preview|config
"""

import ctypes
import os
import subprocess
import sys
import time
from ctypes import wintypes

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import capture as C  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def find_exe():
    """Locate the built binary without assuming anybody's directory layout."""
    env = os.environ.get("FAIRY_EXE")
    if env:
        return env
    # Where package.ps1 stages it, and where a plain `cargo build` puts it.
    for cand in (
        os.path.join(ROOT, "dist", "FairyScreenSaver.exe"),
        os.path.join(ROOT, "target", "release", "FairyScreenSaver.exe"),
    ):
        if os.path.exists(cand):
            return cand
    # Build output may be redirected by .cargo/config.toml or CARGO_TARGET_DIR,
    # so ask cargo rather than guess.
    try:
        import json

        meta = subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=True,
        )
        target = json.loads(meta.stdout)["target_directory"]
        return os.path.join(target, "release", "FairyScreenSaver.exe")
    except Exception:
        return os.path.join(ROOT, "target", "release", "FairyScreenSaver.exe")

user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.EnumWindows.restype = wintypes.BOOL
user32.EnumWindows.argtypes = [ctypes.c_void_p, wintypes.LPARAM]
user32.GetClassNameW.restype = ctypes.c_int
user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user32.GetWindowRect.restype = wintypes.BOOL
user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.c_void_p]
user32.IsWindowVisible.restype = wintypes.BOOL
user32.IsWindowVisible.argtypes = [wintypes.HWND]
user32.PostMessageW.restype = wintypes.BOOL
user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
user32.SetWindowPos.restype = wintypes.BOOL
user32.SetWindowPos.argtypes = [wintypes.HWND, wintypes.HWND, ctypes.c_int, ctypes.c_int,
                                ctypes.c_int, ctypes.c_int, wintypes.UINT]

HWND_TOPMOST = -1
HWND_NOTOPMOST = -2
SWP_NOMOVE = 0x0002
SWP_NOSIZE = 0x0001

EXE = find_exe()


def find_windows(class_name):
    found = []

    def cb(hwnd, _lparam):
        if not user32.IsWindowVisible(hwnd):
            return True
        buf = ctypes.create_unicode_buffer(256)
        user32.GetClassNameW(hwnd, buf, 256)
        if buf.value == class_name:
            found.append(hwnd)
        return True

    user32.EnumWindows(ctypes.cast(ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)(cb), ctypes.c_void_p), 0)
    return found


def window_rect(hwnd):
    rect = (ctypes.c_long * 4)()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))
    return rect[0], rect[1], rect[2] - rect[0], rect[3] - rect[1]


def test_preview():
    notepad = subprocess.Popen(["notepad.exe"])
    time.sleep(2.0)
    hosts = find_windows("Notepad")
    if not hosts:
        print("FAIL: could not find a Notepad window to host the preview")
        notepad.kill()
        return 1
    host = hosts[0]
    print(f"host hwnd = {host:#x}")

    child = subprocess.Popen([EXE, "/p", str(host)])
    time.sleep(3.0)

    kids = [h for h in find_windows("FairyScreenSaverPreviewClass")]
    print(f"preview child windows found: {len(kids)}")
    for k in kids:
        print(f"  child {k:#x} rect={window_rect(k)}")

    x, y, w, h = window_rect(host)
    C.capture("preview.png", x, y, w, h)

    child.terminate()
    time.sleep(0.5)
    user32.PostMessageW(host, 0x0010, 0, 0)  # WM_CLOSE
    time.sleep(0.8)
    notepad.kill()
    return 0 if kids else 2


def test_config():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    p = subprocess.Popen([EXE, "/c"], cwd=root)
    time.sleep(4.0)

    hosts = find_windows("FairyScreenSaverConfigClass")
    print(f"config windows found: {len(hosts)}")
    if not hosts:
        p.terminate()
        return 1
    # The dialog is a normal top-level window; the test harness is not the
    # foreground app, so lift it above whatever the user has open first.
    user32.SetWindowPos(hosts[0], HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE)
    time.sleep(0.6)
    x, y, w, h = window_rect(hosts[0])
    print(f"  rect = ({x},{y},{w},{h})")
    C.capture("config.png", x, y, w, h)
    user32.SetWindowPos(hosts[0], HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE)

    user32.PostMessageW(hosts[0], 0x0010, 0, 0)  # WM_CLOSE
    time.sleep(0.8)
    if p.poll() is None:
        p.terminate()
    return 0


if __name__ == "__main__":
    what = sys.argv[1] if len(sys.argv) > 1 else "preview"
    if what == "preview":
        sys.exit(test_preview())
    elif what == "config":
        sys.exit(test_config())
    else:
        print("usage: gui_test.py preview|config")
        sys.exit(2)
