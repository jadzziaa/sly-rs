#!/usr/bin/env python3
"""Presses DualShock buttons in a headless PCSX2, for tools/pcsx2-frames.sh.

    pcsx2-press.py OUT BUTTON@FRAME[+HOLD][,BUTTON@FRAME[+HOLD]...]

Runs inside the same gamescope as PCSX2 (its DISPLAY is gamescope's own
Xwayland) and sends key presses through XTEST to the focused window. A
press is sent once PCSX2 has saved a frame numbered FRAME or later into
OUT, so presses follow the frames PCSX2 saves (STEP=N spaces them). Each
key is held for a fifth of a second; the keys are the ones
pcsx2-frames.sh binds to pad 1. With +HOLD the key is held until PCSX2
has saved frame FRAME+HOLD (the left stick's directions, lup, ldown,
lleft and lright, need it).
"""

import re
import sys
import time
from pathlib import Path

from Xlib import X, XK, display
from Xlib.ext import xtest

KEYS = {
    "cross": "Return",
    "circle": "BackSpace",
    "square": "z",
    "triangle": "a",
    "start": "space",
    "select": "Tab",
    "up": "Up",
    "down": "Down",
    "left": "Left",
    "right": "Right",
    "gsdump": "F8",
    "state": "F1",
    "capture": "F9",
    "lup": "i",
    "ldown": "k",
    "lleft": "j",
    "lright": "l",
}


def main() -> None:
    out = Path(sys.argv[1])
    presses = []
    for item in sys.argv[2].split(","):
        button, when = item.split("@")
        frame, _, hold = when.partition("+")
        presses.append((int(frame), KEYS[button], button, int(hold or 0)))
    presses.sort()

    d = None
    while d is None:
        try:
            d = display.Display()
        except Exception:  # noqa: BLE001 - the X server is still starting
            time.sleep(0.5)

    frame_re = re.compile(r"_f(\d{5})_")

    def wait_for(frame: int) -> int:
        while True:
            names = [p.name for p in out.glob("*.png")] if out.exists() else []
            frames = [int(m.group(1)) for n in names if (m := frame_re.search(n))]
            if frames and max(frames) >= frame:
                return max(frames)
            time.sleep(0.2)

    for frame, key, button, hold in presses:
        seen = wait_for(frame)
        code = d.keysym_to_keycode(XK.string_to_keysym(key))
        xtest.fake_input(d, X.KeyPress, code)
        d.sync()
        if hold:
            wait_for(frame + hold)
        else:
            time.sleep(0.2)
        xtest.fake_input(d, X.KeyRelease, code)
        d.sync()
        print(f"pressed {button} at frame {seen}", flush=True)


if __name__ == "__main__":
    main()
