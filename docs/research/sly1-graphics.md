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
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-03T05:05:34Z }
verified:
  - { by: claude-code/claude-opus-5-5, at: 2026-11-03T05:05:34Z }
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

## Compared with PCSX2

PCSX2 is run headless (`tools/pcsx2-frames.sh`), and its software renderer
saves each displayed framebuffer at native resolution (640 × 224, one
field).

- **The game itself.** PCSX2 runs Sly 1 with no memory card to the same
  "no memory card" dialog. The pictures differ only where the game
  animates: "Continue" pulses, and our field count is one off from
  PCSX2's. The rest differed by 1 or 2 levels at text edges; matching
  PCSX2's bilinear order (across, then down, truncating each step)
  brought that from 3,391 pixels to 240.
- **Our own packets.** `sly-rs run --gs-dump` writes the GS traffic of the
  last fields as a PCSX2 GS dump: our GS state as the starting state, then
  every GIF packet and V-sync. PCSX2 then draws exactly what our GS drew,
  so any difference is in the drawing alone.
  - Two probes check the dump: a 32-bit picture shown as it is, and a
    4-bit texture with a CLUT drawn by a sprite. Both match pixel for
    pixel.
  - The dialog, field 599: **0 of 143,360 pixels differ**, including the
    game's blend of the previous frame.
  - The title's 3D scene, field 1299: 16,561 of 143,360 pixels differ,
    14,135 by 1 and 45 by more than 8; the largest difference is 11.
    These are rounding differences in perspective texture coordinates and
    shading between two implementations.
    - Before, 26,094 differed, by up to 127, in the spotlight. PCSX2's
      per-draw framebuffers (`SAVE_RT=1`) showed where. The game marks
      pixels with alpha 0x80 from a Gouraud-shaded fan, then darkens only
      where the destination alpha's top bit is clear. Our float
      interpolation turned 0x80 into 0x7F at some pixels. Colour, depth
      and fog are now interpolated exactly from the integer edge weights
      (a unit test covers it).
- The comparison also found two machine bugs: `SetGsCrt` now sets the CRT
  registers as the BIOS does for NTSC, and the GS CSR's revision and ID sit
  in bits 16 to 31.

## Speed

Measured on the replay's first 1300 fields; the last 200 are the 3D title.
`perf` and `samply` need `kernel.perf_event_paranoid` at 1 or lower, so
the profiles came from sampling stacks with gdb, and `sly-rs run` reports
where host time goes. `sly-rs gs-replay` plays a GS dump through our GS
alone, a repeatable benchmark of drawing.

| | 1300 fields | 3D field |
| --- | --- | --- |
| Before | 76 s | about 200 ms |
| After | 29.7 s | about 35 ms |

Real time is 16.7 ms a field. Every change left the pictures bit for bit
the same: the whole of GS memory at field 1300 hashes the same, and the
replay's state hashes are unchanged. The changes:

- **The GS on its own thread.** Nothing the EE does depends on what the GS
  draws. A tag scanner finds the SIGNAL, FINISH and LABEL writes as packets
  are sent, and the GS draws them in order on a worker thread.
- **Fewer event checks.** The machine recomputed its next event after
  every interpreted instruction, 16% of the main thread. Now it does so
  only after a system call or a hardware write.
- **Drawing.**
  - Each triangle row is scanned only across its span.
  - The depth and destination-alpha tests run before texturing.
  - Local memory is held as words, with per-format page tables.
  - Frame and depth pixels are located once per fragment.
  - Texels with a zero bilinear weight are not fetched.
  - The GS dump replay went from 0.285 s to 0.197 s.
- **VU programs.** Decoded pairs are kept per VU, used only while the
  memory still holds the same pair.

The main thread is now mostly VU1 interpretation (VIF1 with VU1, about 56%
of the run's time); the GS thread is busy about 80% of it.

## Open

- The logo's top edge looked clipped in earlier pictures; field and
  interlace handling in the display are checked only for this game's
  mode (NTSC, field mode).

[^runs]: Runs of Sly 1 with the GS, VIFs and VUs modelled
[^symbols]: Sly 1 function and variable names (TheOnlyZac/sly1, CC0)
