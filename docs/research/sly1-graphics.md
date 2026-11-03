---
type: Finding
title: How Sly 1's graphics reach the GS in sly-rs
description: The path Sly 1's frames take through DMA, the scratchpad, VIF0/VIF1, VU0/VU1 and the GS as sly-rs models it, and the four missing pieces that each stopped the first level from drawing.
tags: [finding, graphics, gs, vif, vu, dma, sly1, phase-6]
status: draft
sources:
  - id: runs
    resource: sly-rs run sly1 with --press, --screenshot, SLY_RS_LOG=vif=trace,vu=trace and SLY_RS_VU_DUMP, 2026-11-03
    title: Runs of Sly 1 with the GS, VIFs and VUs modelled
  - id: symbols
    resource: games/sly1/symbols.tsv
    title: Sly 1 function and variable names (TheOnlyZac/sly1, CC0)
  - id: pcsx2
    resource: https://github.com/PCSX2/pcsx2 (pcsx2/Vif_Codes.cpp, Vif_Unpack.cpp)
    title: PCSX2's VIF, read for facts
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-03T03:14:22Z }
verified:
  - { by: claude-code/claude-opus-5-5, at: 2026-11-03T03:29:05Z }
---

# How Sly 1's graphics reach the GS in sly-rs

Observed with `sly-rs run sly1` and the VIF and VU traces.[^runs] Names come
from the decompilation.[^symbols]

## The path of a frame

| Step | What the game does | How sly-rs models it |
| --- | --- | --- |
| 1 | Builds DMA chains (`DMAS`), stages data in the scratchpad through toSPR and back with fromSPR | Scratchpad DMA copies its data when kicked; with TTE, toSPR also writes each tag |
| 2 | Sends VU0 microcode and data through VIF0, then `vcallms` | VIF0 runs each transfer into VU0 when it completes. VU0's registers are the EE's COP2 registers. Programs run to their end at once |
| 3 | Sends VIF1 chains: MPG (two VU1 images that alternate), UNPACKs into double buffers (BASE/OFFSET/TOPS), MSCALs | VIF1 is fed from its chain word by word as it is kicked. Each VU1 program runs to its end when started. XGKICK sends its GIF packet to the GS on PATH1 |
| 4 | Ends slices with `FLUSHE` with the interrupt bit | VIF1 stalls and raises INTC 5. `VU1_Interrupt` reads the slice list, patches what the chain sends next, and writes STC to `VIF1_FBRST`, which resumes the chain from memory |
| 5 | Sends 2D (text, HUD, the title) and texture uploads on PATH3 | GIF packets reach the GS when kicked, so all paths arrive in kick order. SIGNAL, FINISH and LABEL are raised when the transfer completes |
| 6 | Waits on `g_semaFrmMac` in `OpenFrame` | Signalled once the frame's VIF1 work completes |

## What stopped the first level, in order

Each missing piece hid the next:

1. **VIF1's interrupt bit.** Before VIF1 was modelled, the main thread waited
   on `g_semaFrmMac` forever at the level's first frame (about field 1000).
   `StartupFrame` registers `VU1_Interrupt` for INTC cause 5, VIF1.
2. **Words written to `VIF1_FIFO`.** The game sets VIF1 up by storing
   `STCYCL 4,4; OFFSET 0; NOP; STMOD 0` straight into `0x10005000` at boot.
   Without them, CYCLE stays 0, the first UNPACK reads no data, and the
   rest of the stream is misread: the garbage "codes" raised bogus
   interrupts. `VU1_Interrupt` then walked off its slice list and
   overwrote its own code.
3. **VU0 microcode.** It arrives only through VIF0. While VIF0 went
   nowhere, `vcallms` ran empty micro memory.
4. **Scratchpad DMA.** toSPR, kicked thousands of times a frame, and fromSPR
   moved nothing. VU1 programs then started with VI registers from stale
   data and ran without end (a loop counting `vi5` by 4 to an unaligned
   `vi6`). With the copies made, they end, and the title and its 3D scene
   draw.

DMA channels also now read busy (CHCR STR) until their transfer completes.
Before, CHCR always read idle, a leftover from when transfers completed at
once.

## The title is the first level

Sly 1 shows its title over the first level, `jb_intro` (world 0, level 3),
with the interface state in Attract (1). Start begins the level's intro in
place (interface Pausing, 3). World and level alone cannot tell the two
apart, so the replay's checkpoints also give the interface state.

## Open

- Pictures are compared with nothing yet. Next: PCSX2's title at native
  resolution, and a GS conformance program.
- Horizontal tearing in the 3D scene, and the logo's top edge clipped.
  Field and interlace handling are not checked.
- Speed: about 0.05 s per field on the title's dialog; about 0.2 s per
  field once the 3D scene draws (software rasteriser, VU interpreter).

[^runs]: Runs of Sly 1 with the GS, VIFs and VUs modelled
[^symbols]: Sly 1 function and variable names (TheOnlyZac/sly1, CC0)
