"""Smoke test for the saver's dismissal rules.

The saver must **ignore pointer movement** -- Windows emits a burst of
`WM_MOUSEMOVE` messages of its own while raising a fullscreen topmost window,
and a mouse on a desk twitches by itself -- but must quit on a click or a key.

Moving the real cursor is safe as long as it is restored afterwards, and both
the click and the keypress land on the fullscreen topmost saver window, so
nothing else sees them.  F24 is used because nothing binds it.

    python tools/input_test.py [log-path] [click|key]
"""

import ctypes
import os
import sys
import time
from ctypes import wintypes

user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.SetCursorPos.argtypes = [ctypes.c_int, ctypes.c_int]
user32.GetCursorPos.argtypes = [ctypes.POINTER(wintypes.POINT)]
user32.keybd_event.argtypes = [ctypes.c_ubyte, ctypes.c_ubyte, wintypes.DWORD, ctypes.c_void_p]
user32.mouse_event.argtypes = [wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p]

VK_F24 = 0x87
KEYEVENTF_KEYUP = 0x0002
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004

DEFAULT_LOG = os.path.join(os.environ.get("APPDATA", ""), "FairyScreenSaver", "log.txt")


def exit_lines(path: str) -> list[str]:
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return [line.rstrip() for line in f if "exit:" in line]
    except FileNotFoundError:
        return []


def main() -> int:
    log = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_LOG
    dismiss = (sys.argv[2] if len(sys.argv) > 2 else "key").lower()

    origin = wintypes.POINT()
    user32.GetCursorPos(ctypes.byref(origin))
    print(f"cursor saved at ({origin.x}, {origin.y}); dismissal method = {dismiss}")

    ok = True
    try:
        # Phase 1 -- the saver has to survive this.  Both a tiny jitter and a
        # long sweep are covered, since either could trip a naive "any movement
        # quits" rule.
        for i in range(40):
            user32.SetCursorPos(origin.x + (i % 5) - 2, origin.y + (i % 3) - 1)
            time.sleep(0.02)
        for i in range(40):
            user32.SetCursorPos(300 + (i * 37) % 900, 250 + (i * 53) % 400)
            time.sleep(0.02)
        time.sleep(0.6)

        found = exit_lines(log)
        if found:
            print("phase 1 FAILED: pointer movement dismissed the saver")
            for line in found:
                print("   ", line)
            ok = False
        else:
            print("phase 1 ok: swept the pointer ~900px, saver still running")

        # Phase 2 -- an explicit action has to dismiss it.
        if dismiss == "click":
            user32.mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, None)
            time.sleep(0.05)
            user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, None)
            print("phase 2: sent a left click")
        else:
            user32.keybd_event(VK_F24, 0, 0, None)
            time.sleep(0.05)
            user32.keybd_event(VK_F24, 0, KEYEVENTF_KEYUP, None)
            print("phase 2: sent a keypress")
        time.sleep(1.5)

        found = exit_lines(log)
        if found:
            print(f"phase 2 ok: {dismiss} dismissed it")
            for line in found:
                print("   ", line)
        else:
            print(f"phase 2 FAILED: {dismiss} was ignored")
            ok = False
    finally:
        user32.SetCursorPos(origin.x, origin.y)
        print("cursor restored")

    return 0 if ok else 1


sys.exit(main())
