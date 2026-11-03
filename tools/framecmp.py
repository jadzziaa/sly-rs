#!/usr/bin/env python3
"""Compares a GS dump's frames as sly-rs and PCSX2 draw them:

    tools/framecmp.py OURS PCSX2

OURS is a directory `sly-rs gs-replay DUMP --frames OURS` wrote
(`frame<k>.png`); PCSX2 one `tools/pcsx2-frames.sh DUMP PCSX2 0 N` wrote
(`*_f<k>_fr1_*.png`, display read circuit 1). Prints each frame's count of
differing pixels, those over 8 and the largest difference, then the share
of all pixels identical and within 2, 8 and 17 levels. Needs numpy and
Pillow. Run with `python3 -I`.
"""
import glob
import re
import sys

import numpy as np
from PIL import Image


def main():
    ours, theirs = sys.argv[1], sys.argv[2]
    frames = {
        int(re.search(r"_f(\d+)_fr1_", f).group(1)): f
        for f in glob.glob(f"{theirs}/*_f*_fr1_*.png")
    }
    hist = np.zeros(256, int)
    for k in sorted(frames):
        a = np.asarray(Image.open(f"{ours}/frame{k:05d}.png").convert("RGB")).astype(int)
        b = np.asarray(Image.open(frames[k]).convert("RGB")).astype(int)
        d = np.abs(a - b).max(axis=2)
        hist += np.bincount(d.ravel(), minlength=256)
        print(f"{k}: {int((d > 0).sum())} differ, {int((d > 8).sum())} over 8, max {int(d.max())}")
    total = hist.sum()
    share = lambda t: 100 * hist[: t + 1].sum() / total
    print(
        f"{len(frames)} frames: identical {share(0):.3f}%, within 2 {share(2):.3f}%, "
        f"within 8 {share(8):.4f}%, within 17 {share(17):.4f}%, max {np.nonzero(hist)[0].max()}"
    )


if __name__ == "__main__":
    main()
