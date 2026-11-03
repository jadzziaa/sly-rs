---
type: Finding
title: VU microcode translated ahead of time
description: How sly-rs translates the VU microcode overlays into Rust at recompile time, each pair as its own statements with the VU's registers, pipeline and flags in locals, how the translations are matched against what the game uploads and checked against the interpreter bit for bit, and what they gain on Sly 1.
tags: [finding, vu, vu1, microcode, codegen, speed, sly1, phase-6]
sources:
  - id: runs
    resource: sly-rs and the recompiled sly1 binary, run, replay, lockstep and vu-bench on the user's Sly 1 disc, 2026-11-03
    title: Runs of Sly 1 with interpreted and translated VU microcode
  - id: plan
    resource: docs/plan.md, phase 6
    title: 'The plan: ahead-of-time translation of every .DVP overlay'
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-03T23:07:08Z }
verified:
  - { by: claude-code/claude-opus-5-5, at: 2026-11-03T23:10:25Z }
---

# VU microcode translated ahead of time

The plan's phase 6 asks for "ahead-of-time translation of every `.DVP`
overlay, checked against the bytes the game actually uploads, interpreter
for anything else".[^plan] This note records how it is built and what it
measured.[^runs]

## Which microcode Sly 1 runs

Micro memory was dumped at every distinct program start over the replay's
2600 fields and each pair matched against the executable's overlays
(`ps2_analysis::vu_overlays`). Every pair of every program run comes from
an overlay:

| Overlay id | Where | Size | Runs on |
| --- | --- | --- | --- |
| 437275 | 0x0000-0x3AB0, eight parts | 1878 pairs | VU1: the 3D renderer |
| 436667 | 0x0000-0x16D0, three parts, over 437275 | 730 pairs | VU1 |
| 437755 | 0x0000 | 4 pairs | VU1 |
| 13243 | 0x0000 | 90 pairs | VU0 |

436667 is uploaded over the start of 437275, so micro memory holds a mix
of the two. Sly 1 enters its programs at 28 addresses.

## How it is built

- **Generation** (`ps2_codegen::vu`, run by `cargo xtask recomp`): the
  parts of one overlay become one program, cut into pieces of about 128
  pairs: preferably where control does not fall through, never inside an
  inner loop (a backward branch's span of at most 128 pairs), at most 384
  pairs. Each piece is a function over its basic blocks: a loop over a
  `match` on the block to run, entered only at a block's first pair. The
  game's binary carries them in `generated/<game>/vu`, a crate depending
  on `ps2-vu` only.
- **Each pair becomes its statements.** A block is straight-line code on
  locals: the registers (a copy the compiler breaks into scalars), the
  cycle, Q's and P's results under way, and the flags. `vf0` and `vi0` are
  constants (a piece checks that the registers hold them). A
  `maddw.xyzw $vf10, $vf5, $vf0w` is a `lanes::madd` on two registers and
  a constant.
- **Flags as the interpreter carries them.** `micro::State` keeps the
  flags as four snapshots: the flags visible now and in each of the next
  three cycles. A pair that sets flags makes the last snapshot (its own
  flags, visible four cycles on), and every pair shifts them by one. In a
  block the shift is a renaming of locals; between blocks it costs a few
  moves. This matters because Sly 1's flag reads cross blocks: in the 3D
  loop, every `fsand` reads a `msuby` four pairs back in an earlier block,
  across the loop's back edge for the first. (`State` kept the flags as a
  queue of timed writes before; the change left the interpreter's results
  on every recorded program the same.)
- **Waits** (WAITQ, WAITP, a DIV or EFU instruction while its unit is
  busy) move the cycle and shift the flags by the cycles waited, as the
  interpreter does. Q and P land where they are read.
- **One semantics.** Every operation comes from `ps2_vu::micro`'s
  functions, which the interpreter's `exec` uses too (the float lanes,
  CLIP, the divider, the EFU, loads and stores). What the generator does
  not handle, it leaves to the interpreter: FSSET (Sly 1 never runs one),
  and a branch or an E bit beside another. A piece then hands its state
  back, and the interpreter runs pair by pair to the next block's start.
- **Matching** (`micro::run`): a piece runs only where micro memory holds
  its code byte for byte. Writes to micro memory are counted (VIF's `MPG`,
  and the EE's accesses to 0x11000000 and 0x11008000); when the count
  moves, the pieces are matched again. Anything else runs in the
  interpreter.
- **Switching it off.** `run --interpret` and the plain `sly-rs` binary
  interpret everything, EE and VUs.

## Checks

- `vu-bench` (hidden) replays VU1 programs recorded with
  `$SLY_RS_VU_RECORD=PATH,SKIP,COUNT` through the interpreter and the
  translations, and compares registers, data memory, the packets sent and
  pair counts. With `--one-block` each piece hands its state back after
  every block, so every block edge goes through `micro::State`: the
  hand-over is checked as hard as the blocks. Six recordings (6,000
  gameplay programs, 2.9 million pairs, and five shorter ones: lamps,
  lights, baking, VU0): identical both ways. It also prints a digest of
  the interpreted outcomes, to compare builds.
- `vu-trace --profile` counts the pairs run at each address and by
  instruction.
- The replay's checkpoints hash both VUs' memories and VU1's registers;
  `cargo xtask test-full` runs the recompiled game in lockstep with the
  interpreter and through the replay: unchanged.

## What it gains

On the 6,000 gameplay programs (`vu-bench`, the generated game built for
the host CPU):

| | ns a pair |
| --- | --- |
| Interpreter | 14.2 (10.7 before the flag snapshots; interpreted is for debugging) |
| Translated, each pair through `exec` (the first design, before the flag snapshots) | 7.8 |
| Translated, pairs as statements | 4.4 |
| ... and waits in place, not handed to the interpreter | 3.9 |
| ... and pieces cut outside inner loops | 2.5 to 3.6 |

The figures move by about a nanosecond from run to run. Larger pieces
(up to 1,024 pairs, so that no loop was cut) gave 5.8: big functions
compile to worse code.

Where the first design spent its time: `exec`'s generic per-pair work.
The flag queue's write and landing, the pending Q and P checks, and the
delay-slot and E-bit bookkeeping were each a load and a store through
`State`, so each pair waited on the one before through memory. Removing
one of them at a time (straight-line blocks over `exec`, flags landed
lazily inside `exec`) gained nothing. With the whole state in locals they
all go at once.

**In play.** The user's prologue recording (19,180 fields; it diverges
from its recording since the float switch, as before) runs headless in
418.5 s: 76.5% of console speed, from 67.6% with the first design. VU1
is three times faster but the whole run only 13%, so VU1 no longer sets
the pace alone: where the rest of the time goes is the next thing to
measure.

[^runs]: Runs of Sly 1 with interpreted and translated VU microcode
[^plan]: The plan: ahead-of-time translation of every .DVP overlay
