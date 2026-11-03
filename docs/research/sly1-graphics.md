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
  - id: paris
    resource: tools/pcsx2-frames.sh with CARD, PRESS=cross@400,gsdump@1550; sly-rs run sly1 --press cross@500 --fields 860 --gs-dump
    title: PCSX2 GS dump of the intro's "Paris, France" shot, and ours of the same shot, 2026-11-03
  - id: pcsx2
    resource: https://github.com/PCSX2/pcsx2 (pcsx2/Vif_Codes.cpp, Vif_Unpack.cpp)
    title: PCSX2's VIF, read for facts
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-03T23:46:17Z }
verified:
  - { by: claude-code/claude-opus-5-5, at: 2026-11-03T23:10:25Z }
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
place (interface Playing, 3). World and level alone cannot tell the two
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
- **Whole dumps, frame by frame** (the phase 6 gate). `gs-replay
  --frames` writes what the display shows at each V-sync, numbered as
  PCSX2 numbers a dump's frames, and `tools/framecmp.py` compares the two
  sets. (Comparing the replay's end with PCSX2's last frame, as before,
  was off: the end already holds part of the next field.) What it found:
  - **Antialiasing (`AA1`)** was missing. The binocucom's border and HUD
    are antialiased triangles: PCSX2 draws a triangle's inside, then one
    pixel per step along each edge with the share of the pixel covered,
    which becomes the pixel's alpha (shifted right by 9). Without `ABE`,
    an `AA1` primitive's inside takes alpha 0x80 and its edges their
    coverage; with `ABE`, only alpha 0x80 is replaced. Edge pixels test
    depth but never write it, and `AA1` blends (with ALPHA) even without
    `ABE`. `crates/ps2-gs/src/aa.rs` ports PCSX2's edge and line walks;
    edge pixels take their colour, depth and texture coordinates from our
    own interpolation along the edge.
  - **Points** light the pixel at their position plus half a pixel,
    truncated, as PCSX2 places them (we took the ceiling): the static in
    the binocucom's side panels.
  - Together, on the level intro: within 8 levels of PCSX2, 96.8% of
    pixels before (differences up to 115), 100% after (up to 6).
  - What is left: in the dialog, glyph edges of scaled sprites, where
    PCSX2 steps texture coordinates in floats (per row, then per group of
    pixels) and we interpolate them exactly in the GS's fixed point; one
    column can land on the next texel. In the title's 3D, rounding of
    perspective texture coordinates and shading.

  | GS dump (fields) | identical | within 2 | within 8 | within 17 | largest |
  | --- | --- | --- | --- | --- | --- |
  | Dialog, 300-399 (100) | 99.93% | 99.98% | 99.987% | 99.9997% | 79 |
  | Title's 3D, 1270-1299 (30) | 85.1% | 99.57% | 99.970% | 100% | 17 |
  | Level intro, 2570-2599 (30) | 98.1% | 99.98% | 100% | 100% | 6 |

  **The gate's tolerance:** in every scene at least 99.9% of pixels within
  8 levels (of 255, on any channel) of PCSX2's software renderer, the rest
  at texel boundaries of scaled sprites or in perspective rounding.
- **The real game, in PCSX2.** With a formatted memory card
  (`sly-rs format-card`) and Cross pressed through XTEST
  (`tools/pcsx2-frames.sh` with `CARD`, `PRESS`), headless PCSX2 gets past
  Sly's "create a save file?" dialog to the Sony and Sucker Punch logo
  movies and the intro ("Paris, France, 4:20 am"). A `gsdump` press saves
  PCSX2's own GS dump of a frame of the real game.[^paris]
  - Our intro is dark and smeared: no sky, no moon, stale pixels dragged
    across the screen.
  - Our GS draws PCSX2's dump of the real game correctly (moon, stars,
    Eiffel Tower), so the GS is not at fault.
  - The two dumps of the same shot (ours at field 856, PCSX2's at frame
    1550) agree closely: the camera, about 3,300 triangle strips and
    3,300 points, the full-screen passes, the display registers, and
    textures in GS memory outside the framebuffers.
  - Our commands drawn over PCSX2's starting GS memory give PCSX2's
    picture. PCSX2's commands over ours start wrong and recover within
    three frames. Swapping regions shows that only the two framebuffers
    carry the error.
  - The game blends each frame with the previous one, so whatever is not
    redrawn persists. Bucketed by primitive and texture, what we draw
    less of is distant: strips with three textures (`TBP` 0x2CE0 and
    0x2CE8, 64 × 64, and 0x3AE4, 32 × 32, all 4-bit; about 960 vertices)
    in a small area around the Eiffel Tower, 2,216 untextured line
    vertices in the same area (the tower's lattice, by position), and
    large strips spanning the screen, likely the sky, drawn with a fifth of
    PCSX2's vertices (`TBP` 0x2D10 and 0x2DA0 to 0x2DA6).
  - **The cause: VU flags arrived too early.** The EE sends these objects
    to VU1 every frame, and VU1 sends them on, but with the ADC bit set on
    every vertex, so the GS draws nothing. VU1's program at 0x1AB0 (MSCAL
    0x356) projects strips. Its vertices arrive with ADC set (w =
    2²³ + 0x8000 + fog × 16), and the program adds 0x4000 to a vertex's w
    when its triangle faces away, which carries out of the ADC bit. The
    facing test is `MSUBY.x vf0, ...`, a 2D cross product, whose sign
    `FSAND vi11, 0x002` reads four pairs later, with three more multiplies
    in between. On the hardware (and in PCSX2) MAC, status and clipping
    flags become visible four cycles after their instruction issues. Ours
    were visible at the next pair, so `FSAND` read the sign of the last
    `MADDAZ` instead, and every triangle of these objects counted as
    facing away.
  - The fix: flags travel through the FMAC pipeline (`ps2_vu::micro`)
    and land four cycles after issue; FSSET and FCSET go the same way; the
    rest land when the program ends. Translated microcode shares the
    interpreter's code, so both changed. The intro and the title now
    match PCSX2 by eye: sky, moon, Eiffel Tower and its lines, town
    lights, Sly on the rooftop.
  - Not modelled: stalls when an instruction reads a VF register an FMAC
    instruction still writes. On the hardware they advance the clock, so
    flags land after fewer pairs. Sly 1's code here is scheduled without
    stalls, and the pictures match without them.
  - `sly-rs vu-trace` (hidden) lists the programs of a VU1 recording or
    steps one pair by pair, with the registers each pair changes; that is
    how the test was found.
- **The lamp on the title's water tower lit nothing.** Matched against a
  PCSX2 GS dump of the same moment, our commands were PCSX2's except for
  vertex colours: objects near the lamp had the moon's light only. The
  chain, found with `vu-trace` and store probes:
  - Lighting is baked into each model's colour arrays by VU1 jobs, read
    back by `VU1_Interrupt`; the lights a model gets come from
    `TwpsFindSwLights` (from `DrawGlob`), which tests the model's sphere
    against each group of lights with VU0 microprograms (`vcallms 0x60`,
    `0x168`) on light spheres `PrepareSwLightsForDraw` uploads through
    VIF0.
  - VIF0 had stopped for good at boot. Its first UNPACK came with CYCLE
    at 0, so it read no data; the data was then read as codes, one of
    them with the interrupt bit, and VIF0 stalled. VU0's data memory
    stayed empty, every light test failed, and only the always-on light
    (the moon) was ever found.
  - The game never sets VIF0's CYCLE. A PCSX2 savestate shows it at 4, 4,
    as the BIOS leaves it, and PCSX2's FBRST reset keeps CYCLE and the
    other registers (only the FIFO, ERR, MSKPATH3 and the status are
    reset). Ours reset everything. Now both VIFs start at 4, 4, and a
    reset keeps the registers. Per-model colours now equal PCSX2's.
- The comparison also found two machine bugs: `SetGsCrt` now sets the CRT
  registers as the BIOS does for NTSC, and the GS CSR's revision and ID sit
  in bits 16 to 31.

## Speed

Measured on the replay's first fields, timing the whole process (the GS
thread included). `perf` and `samply` need `kernel.perf_event_paranoid` at
1 or lower, so the profiles came from sampling stacks with gdb. `sly-rs run`
reports where host time goes, and `sly-rs gs-replay` plays a GS dump
through our GS alone, a repeatable benchmark of drawing.

| | 1300 fields | fields 1100-1200 | fields 1200-1300 (3D) |
| --- | --- | --- | --- |
| Before (`3ed1483`) | 73.5 s | 101 ms a field | 158 ms a field |
| GS on one thread (`d0824b6`) | 37.4 s | 32 ms a field | 81 ms a field |
| Drawing on 8 threads | 32.4 s | 25 ms a field | 40 ms a field |
| The same, recompiled build | 27.6 s | 31 ms a field | 32 ms a field |
| Recompiled, VU1 microcode translated | 21.7 s | 19 ms a field | 26 ms a field |
| Recompiled, narrower drawing hazards | 17.2 s | 16 ms a field | 17 ms a field |
| Interpreted, narrower drawing hazards | 30.1 s | 24 ms a field | 34 ms a field |
| Recompiled, drawing queued to a drawer | 16.7 s | 18 ms a field | 16 ms a field |
| Recompiled, four-lane VU float operations | 16.1 s | 15 ms a field | 17 ms a field |
| Interpreted, four-lane VU float operations | 26.8 s | 32 ms a field | 31 ms a field |
| Interpreted, faster EE | 23.4 s | | |
| Recompiled, cheaper pixels | 15.0 s | | |
| Interpreted, cheaper pixels | 22.5 s | | |
| Recompiled, VU flags with their four-cycle latency | 17.5 s | | |

The last row costs 2.5 s, from two sources not yet separated: queueing
each FMAC instruction's flags, and the sky, moon and Eiffel Tower, which
are drawn now.

**Gameplay is VU1-bound.** The user's prologue recording (19,180 fields)
runs headless at 56% of console speed, 59.7% with the game built for the
host CPU (`generated/<game>/.cargo/config.toml`, `target-cpu=native`;
results are bit-identical, as lockstep and the replays confirm). VU1
takes 84.5% of host time in gameplay: about 1.4 million pairs a field.
On 6,000 recorded gameplay programs (`vu-bench --translated-only`),
translated microcode takes 12.4 ns a pair (11.5 native), about 300 host
instructions; callgrind puts about 40% of that in `exec`'s generic
per-pair work (pending Q and P, the flag queue, branch bookkeeping), 28%
in the float emulation, 16% in the pieces' `match` dispatch. Tried and
dropped, as no faster: straight-line code per basic block (11.5 ns either
way), and adds in single precision with an exact TwoSum correction (the
lane logic did not vectorise; slower). The float operations alone cost
4 ns (`mul`), 8 ns (`add`) and 16 ns (`madd`) for four lanes
(`cargo bench -p ps2-core`).

What rac-rs does instead ([rac-rs.md](rac-rs.md); read from its source):
its VU1 generator specialises each pc over the whole timing state (VF
ready cycles, Q, the flag queue, a pending branch), so every stall and
flag landing is a constant; keeps the registers in SSE locals within
basic blocks; stores raw results and builds MAC and status words only
where read; bails to the interpreter, its pipeline rebuilt, for anything
rare; and computes VU floats with one SSE instruction each under a
round-toward-zero, flush-to-zero MXCSR with operands clamped: PCSX2's
default model, without the guard-bit truncation sly-rs models. Its main
3D does not go through VU1 at all (a renderer of its own).

**PCSX2's float model for the VUs** (the user's decision): VU arithmetic
rounds the exact result toward zero, without the guard-bit truncation
(see [conformance.md](conformance.md)). Programs run in the VU float
environment (`ps2-fenv`: MXCSR round toward zero, denormals flushed),
where the four lanes are host single-precision operations
(`float::x4::rtz`), equal to the exact `f64` forms on a million random
cases of each operation (`crates/ps2-core/tests/vu_rounding.rs`). On the
gameplay programs: the interpreter from 16.5 to 10.7 ns a pair,
translated microcode from 11.5 to 7.8 ns. Division, square root and the
EFU compute in `f64` inside that environment and so round toward zero
there too, which moved the replay's hashes.

**Specialised translation** (rac-rs's approach, adapted): each pair
becomes its own statements, with the registers, the pipeline and the
flags in locals; translated microcode from 7.8 to about 2.5 ns a pair,
and the prologue's workload from 67.6% to 76.5% of console speed. See
[vu-translation.md](vu-translation.md).

**Gameplay is now GS-bound.** `replay` now reports where host time goes
(as `run` does) and can write a GS dump of its last fields; `gs-replay`
reports the drawer's time by kind of work and the GS thread's waits for
it. On the first 8,000 fields of the prologue recording (the machine
busy with another build beside it, so absolute times are high):

- the GS thread was busy 98.7% of the run, a quarter of it queueing and
  drawing; the machine waited for room in its queue 20% of the run;
- VIF1 with VU1 took 71.5%, the programs about 3.6 ns a pair without
  those waits (2.5 in `vu-bench`); the EE and the rest 27.6%; the IOP
  0.6%.

A dump of 60 fields of play (about 10 MB of GIF packets and 44,000
primitives a field), through the GS alone:

| Drawing threads | 60 fields | the drawer on the pool | one primitive after another | the GS thread waiting for the drawer |
| --- | --- | --- | --- | --- |
| 1 | 4.65 s | | | |
| 4 | 1.82 s | 1.24 s | 0.23 s | 1.16 s |
| 8 (the default cap) | 1.30 s | 0.68 s | 0.24 s | 0.61 s |
| 16 | 1.18 s | 0.47 s | 0.24 s | 0.40 s |
| 24 | 1.18 s | 0.44 s | 0.24 s | 0.40 s |

Real time is 1.0 s. With 8 threads the GS caps play at about 77% of
console speed, which is what the prologue measures (76.5%). The drawer
is the critical path; the GS thread's own work (parsing, setting up and
queueing primitives) is about 0.78 s, and the serial drawing (795
primitives drawn alone, small batches) and uploads about 0.3 s, so more
threads stop paying past 16. On one thread (callgrind), 90% of the GS's
instructions are per pixel: `fragment` 66% inclusive, `texel` 22%,
`prim` 19% itself. Each triangle pixel computes its depth with a 128-bit
multiply-add and division and its colour and fog with five 64-bit
divisions (exact barycentric weights; `__divti3` alone is 5.6% of the
instructions). The same exact quotients can be stepped along a row with
a remainder, without a division per pixel, leaving every pixel the same.

Real time is 16.7 ms a field. From the recompiled build at 17 s down,
per-stretch figures move by 2 to 4 ms a field from run to run (the totals
by about 0.3 s); the table gives typical runs. The recompiled build's EE is faster, but VU1
and the GS are the same code, so it gains least where VU1 is busiest; it
waits for the GS 8.2 s of its run. Until the fix below, it was not in the
same state as the interpreter at all: after the change to event checks
(`d507a81`), a DMA transfer started from compiled code completed late,
because the next event was recomputed only after interpreted hardware
writes. Lockstep failed at field 1 and every replay checkpoint differed;
the disc tests ran the interpreter alone, so nothing noticed. `cargo xtask
test-full` now also runs the recompiled build in lockstep and through the
replay. Earlier figures in this note (29.7 s, about
35 ms a 3D field) timed the machine without waiting for the GS thread to
catch up, and were too good. Every change left the results bit for bit the
same: the whole of GS memory at field 1300 hashes the same, and the
replay's state hashes are unchanged. (Since VU translation the state hash
also covers both VUs' memories and VU1's registers; the replay's
checkpoints were re-recorded with the interpreter, and the recompiled
build with translated microcode gives the same values.) The changes:

- **The GS on its own thread.** Nothing the EE does depends on what the GS
  draws. A tag scanner finds the SIGNAL, FINISH and LABEL writes as packets
  are sent, and the GS draws them in order on a worker thread. The queue
  holds up to 4096 packets, so memory stays bounded (about 170 MB at peak).
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
- **VU programs.**
  - Decoded pairs are kept per VU, used only while the memory still holds
    the same pair.
  - Arithmetic chooses its operand and operation once per instruction, not
    per component.
  - Flags are computed without branching.
  - Round-toward-zero comes from the bits instead of rounding and
    correcting.

  VU1 programs went from 17.1 s to 13.0 s of the run.

- **Drawing on several threads** (`ps2-gs` `batch`, after rac-rs's
  `gsfull`):
  - Primitives queue in a batch. When the batch is drawn, each primitive
    is split into 32 × 32-pixel tiles, and tiles draw in parallel, each in
    primitive order. A tile holds whole blocks of local memory in every
    format, so no two threads touch the same word.
  - A batch is drawn first whenever one primitive would read what another
    writes elsewhere: a texture from blocks the batch writes, a write over
    blocks it textures from, another frame or depth buffer, an upload or
    CLUT load touching its blocks, a copy, a read back, a picture or dump.
  - A primitive textured from its own frame, or reaching past its frame's
    width, is drawn alone.
  - **Narrower hazards.** Measured on the GS thread over 1300 fields, the
    front end (parsing and batching) took about 4.5 s; 220,734 primitives
    drawn alone, one after another, took 9.6 s. The rules were too
    coarse:
    - A texture counted as its whole declared size (up to 1024 × 1024).
      Now it is the texels the primitive can read: the range of its
      vertices' coordinates (for perspective, of S/Q and T/Q when every Q
      is positive; for a sprite, at its first and last pixels, as the
      rasteriser computes them), half a texel more for bilinear filtering
      and an eighth for rounding, wrapped as CLAMP says.
    - A depth buffer counted whenever the depth test was on. Now it
      counts as read when the test compares (GEQUAL, GREATER) and as
      written when ZMSK is clear: Sly's full-screen strips mask depth, and
      its tiny 64-pixel-wide passes keep their depth buffer at the frame's
      own address but test ALWAYS.
    - A region ran from the start of a rectangle's first page row. Now it
      is the pages each page row of the rectangle covers.
    - The batch keeps frame writes, depth writes, depth use and texture
      reads apart, so a primitive whose depth reads hit another's frame
      writes (a hazard the old rules missed) also ends the batch.

    Primitives drawn alone fell to 11,704. Checked block by block, half
    of them share no block with what they write, but those are tiny
    (about a million pixels in all); the other 5,818, 59 million pixels,
    texture from blocks they draw into and must stay in order. Two tests guard
    the rules: every pixel of random rectangles lies in its region, in
    every format and width; and the narrowed cases draw on 4 threads what
    they draw on one. Three runs of 1300 fields give the same GS memory.
  - Local memory is atomic words, so the threads share it without
    `unsafe`.
  - The GS dump replay draws in 0.055 s on 8 threads (0.204 s on one).
  - The number of threads is `$SLY_RS_GS_THREADS`, by default all but two
    of the host's, at most 8. Past 4 it gains nothing in the game.

- **Drawing queued to a drawer.** The GS thread used to hand each batch
  to the drawing threads and wait for it. Now a drawer thread takes
  batches in order, up to 16 ahead, and draws each on the pool (or one
  primitive after another, for small batches and primitives drawn alone),
  while the GS thread parses on. Measured on the way:
  - With one batch in flight it gained nothing: the GS thread waited 6.4 s
    for batches, as long as drawing took (3.4 s on the pool, 3.0 s one
    primitive after another), because batches come back to back.
  - With 16, it still waited 5.6 s, for uploads: the game streams textures
    into the same blocks, and each upload waited for the drawing of the
    texture before it. Uploads now go through the drawer too, in order.
  - Handed over one by one, the 13 million upload pieces (mostly a
    quadword each) cost more than they saved. They are now coalesced: the
    pixels not yet handed over come before the batch being built (the
    primitives that read them were queued after them), and go to the
    drawer with it, or alone when a CLUT load needs them.

  The gain is small: 17.2 s to 16.7 s for 1300 fields, and the 3D stretch
  from 17 to 16 ms a field, now at the console's own pace. The drawer
  is the critical path: it draws for about 6.5 s, 3 s of it one primitive
  after another. A test uploads textures again and again into the blocks
  the queued batches read, and a palette for a CLUT load; with uploads
  written out of order, it fails.

- **The interpreted EE.** Sampled over the first 600 fields (boot, the
  dialog, the title: EE work, little VU1), decoding took a third of the
  main thread, half of it a second decode the run loop made to spot
  system calls. Changes, all exact by construction:
  - instructions decode once, through a cache by address used only while
    memory holds the same word, fetched from main RAM directly;
  - hooks are looked up only where a bitset says one may be;
  - instructions run back to back while nothing the run loop checks can
    change: until the next event or the end of the run, a system call, a
    hardware write, an instruction that changes interrupt enabling (`ei`,
    `di`, `mtc0`), a VU started, a hooked or kernel return address; and
    only while no IOP packet waits (its delivery depends on RAM) and no
    compiled code is attached. These are the points where compiled code
    hands back, so the checks see what they saw.

  The machine finishes those 600 fields in 1.37 s instead of 2.19 s
  (`run` now reports when the machine finished and how long the GS took
  after it), about 2 ms a field. Lockstep with the replay's presses, the
  recompiled machine against the interpreter, is equal for 1500 fields.
  PCSX2's EE interpreter, read for comparison, decodes every instruction
  through its opcode tables with no cache, and tests events at branches;
  its idle-loop skip is a speed hack that changes timing. It is a
  reference for meaning, not a model for speed.

- **Cheaper pixels.** Over the first 600 fields the machine finished in
  1.3 s and the GS 2.8 s later. A GS dump of 100 fields of the "no
  memory card" dialog (`run --gs-dump`, replayed with `gs-replay`, which
  now prints a hash of all of local memory) gave a repeatable benchmark:
  about 380,000 pixels a field, nearly all sprites. Two 32 × 224 strips
  clear each of two buffers, two more copy one buffer over the other with
  blending, then the dialog box (8-bit texture) and the text (4-bit).
  Profiled, a third of the time went to working out pixel addresses.
  Changes, each leaving local memory the same:
  - a buffer's addressing (page table, page shape, base, width) is worked
    out once per primitive for its frame, depth buffer and texture;
  - CLAMP's wrapping is worked out once per primitive, and bilinear
    filtering wraps its two columns and rows once rather than per texel;
  - a sprite's U is computed once per column, not per pixel;
  - a sprite with no texture, fog, blending or alpha tests and a depth
    test that passes everything writes the same value to every pixel, so
    it is written directly; a test checks it against the pipeline pixel
    for pixel over random memory (frames of 32, 24 and 16 bits, FBMSK,
    FBA, COLCLAMP, depth written or masked).

  The dialog replay went from 1.37 s to 0.95 s on one thread and from
  0.61 s to 0.48 s on eight; 30 fields of the level from 0.69 s to 0.56 s
  and 0.25 s to 0.24 s. In the game the GS now finishes the first 600
  fields 2.0 s after the machine instead of 2.8 s.

- **Parsing and small batches.** On 30 fields of the level's GS dump the
  GS thread spent a fifth of its time parsing GIF packets, most of it
  copying the path's tag (about 48 bytes) out of the GS and back for every
  quadword; the tag now stays in a local while a send's quadwords go by.
  Batches of 4,096 pixels or more (was 16,384) now draw on the pool. The
  level dump draws in 0.464 s on one thread (was 0.559) and 0.132 s on
  eight (was 0.232); the dialog dump in 0.45 s on eight (was 0.49). In the
  game the gain is small (the recompiled build waits for the GS 2.7 s of
  about 15.4 s, was 2.9; the GS finishes the first 600 fields 1.9 s after
  the machine, was 2.0): there the machine and the drawer set the pace.
- **Tried and dropped.**
  - The adder's alignment done on `f64` values (scale by the larger
    exponent's last place, truncate to an integer, scale back): exact,
    agreeing with the scalar adder on 3 million random operands, but
    slower (16.4 ns a pair interpreted against 15.0, 11.1 translated
    against about 10), most likely the saturating float-to-integer
    conversion's checks.
  - The VU interpreter calling `exec` through one function per upper
    class (69 instances, the class a constant), to fold its matches:
    20 ns a pair against 15. Lower × upper would be thousands of
    instances.
  - Primitives drawn alone, checked block by block: the 5,818 that share
    blocks with what they write (59 million pixels, about 2.4 s of the
    drawer) texture from the buffer they draw into, so their pixels
    depend on each other's order. Only a per-row dependency analysis of
    such sprites could split them, which is not worth 2 s.

Where the time is now, and how much is left to win:
- **VU1, interpreted build.** Translated microcode does the same float
  work at about 10 ns a pair with every dispatch folded; the interpreter
  takes about 15. So decoding and dispatch cost at most 5 ns a pair,
  about 3.8 s of the interpreted build's 22.4 s; no restructuring reaches
  all of it. The rest is the exact float emulation both modes share,
  already on four lanes.
- **GS pixels.** About 25 ns a pixel on one thread: every pixel through
  the whole pipeline. Pipelines specialised to a primitive's state, as
  PCSX2's software renderer generates, are the large remaining step; they
  would pay mostly in the recompiled build (the drawer, about 6.5 s, and
  2.7 s of machine waits) and the title stretch, little in the
  interpreted build (0.7 s of waits).
- **Drawing in order**: the 5,818 primitives above, and small batches,
  about 3 s of the drawer, mostly unavoidable.

## Open

- A pixel-level comparison of the intro and the title with PCSX2 at the
  same moment: our timeline runs about 750 fields ahead, as the hooks
  skip the logo movies, so frames are matched by eye for now.

- The logo's top edge looked clipped in earlier pictures; field and
  interlace handling in the display are checked only for this game's
  mode (NTSC, field mode).

[^paris]: PCSX2 GS dump of the intro's "Paris, France" shot, and ours of the same shot, 2026-11-03
[^runs]: Runs of Sly 1 with the GS, VIFs and VUs modelled
[^symbols]: Sly 1 function and variable names (TheOnlyZac/sly1, CC0)
