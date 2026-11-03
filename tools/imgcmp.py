#!/usr/bin/env python3
"""Compares two pictures pixel by pixel (sly-rs's against PCSX2's):

    tools/imgcmp.py OURS THEIRS [DIFF.png]

Prints how many pixels differ, a histogram of the largest channel
difference (0, 1, 2, ...), and the first few pixels that differ by more
than 3; with DIFF.png, writes the difference in red over a dimmed copy of
OURS. Needs numpy and Pillow. Run with `python3 -I`.
"""
import sys

import numpy as np
from PIL import Image


def main():
    ours = np.asarray(Image.open(sys.argv[1]).convert("RGB")).astype(int)
    theirs = np.asarray(Image.open(sys.argv[2]).convert("RGB")).astype(int)
    if ours.shape != theirs.shape:
        sys.exit(f"sizes differ: {ours.shape} and {theirs.shape}")
    diff = np.abs(ours - theirs).max(axis=2)
    n = int((diff > 0).sum())
    print(f"{n} of {diff.size} pixels differ ({100 * n / diff.size:.3f}%), max {diff.max()}")
    print("by difference:", np.bincount(diff.ravel())[:9].tolist(), f"over 8: {int((diff > 8).sum())}")
    ys, xs = np.nonzero(diff > 3)
    for y, x in list(zip(ys, xs))[:8]:
        print(f"  ({x}, {y}): {ours[y, x].tolist()} vs {theirs[y, x].tolist()}")
    if len(sys.argv) > 3:
        out = np.zeros(ours.shape, np.uint8)
        out[..., 0] = np.minimum(diff * 8, 255)
        out[..., 1] = (ours.mean(axis=2) / 3).astype(np.uint8)
        Image.fromarray(out).save(sys.argv[3])


if __name__ == "__main__":
    main()
