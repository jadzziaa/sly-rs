#!/usr/bin/env python3
"""Presses DualShock buttons in a headless PCSX2, for tools/pcsx2-frames.sh.

    pcsx2-press.py OUT BUTTON@FRAME[,BUTTON@FRAME...]

Runs inside the same gamescope as PCSX2 (its DISPLAY is gamescope's own
Xwayland) and sends key presses through XTEST to the focused window. A
press is sent once PCSX2 has saved a frame numbered FRAME or later into
OUT, so presses follow the frames PCSX2 saves (STEP=N spaces them). Each
key is held for a fifth of a second; the keys are the ones
pcsx2-frames.sh binds to pad 1.
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
}


def main() -> None:
    out = Path(sys.argv[1])
    presses = []
    for item in sys.argv[2].split(","):
        button, frame = item.split("@")
        presses.append((int(frame), KEYS[button], button))
    presses.sort()

    d = None
    while d is None:
        try:
            d = display.Display()
        except Exception:  # noqa: BLE001 - the X server is still starting
            time.sleep(0.5)

    frame_re = re.compile(r"_f(\d{5})_")
    for frame, key, button in presses:
        while True:
            names = [p.name for p in out.glob("*.png")] if out.exists() else []
            frames = [int(m.group(1)) for n in names if (m := frame_re.search(n))]
            if frames and max(frames) >= frame:
                break
            time.sleep(0.2)
        code = d.keysym_to_keycode(XK.string_to_keysym(key))
        xtest.fake_input(d, X.KeyPress, code)
        d.sync()
        time.sleep(0.2)
        xtest.fake_input(d, X.KeyRelease, code)
        d.sync()
        print(f"pressed {button} at frame {max(frames)}", flush=True)


if __name__ == "__main__":
    main()
