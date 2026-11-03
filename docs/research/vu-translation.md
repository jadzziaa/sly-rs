---
type: Finding
title: VU microcode translated ahead of time
description: How sly-rs translates the VU microcode overlays into Rust at recompile time, how the translations are matched against what the game uploads and checked against the interpreter bit for bit, and what they gain on Sly 1.
tags: [finding, vu, vu1, microcode, codegen, speed, sly1, phase-6]
sources:
  - id: runs
    resource: sly-rs and the recompiled sly1 binary, run, replay, lockstep and vu-bench on the user's Sly 1 disc, 2026-11-03
    title: Runs of Sly 1 with interpreted and translated VU microcode
  - id: plan
    resource: docs/plan.md, phase 6
    title: 'The plan: ahead-of-time translation of every .DVP overlay'
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-03T09:20:37Z }
verified:
  - { by: claude-code/claude-opus-5-5, at: 2026-11-03T09:20:37Z }
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
  pairs where control does not fall through (after a program's last pair
  or an unconditional jump's delay slot). Each piece is a function: a loop
  over a `match` on the pair's address, so it can start at any pair and
  follow branches inside the piece. The game's binary carries them in
  `generated/<game>/vu`, a crate depending on `ps2-vu` only.
- **One semantics**, the design's main property. Each arm calls the
  interpreter's own `exec` with the pair's words and operation class as
  constants. `exec`, the per-component
  loop and the float operations are always inlined, so the compiler keeps
  only what that pair does. The interpreter and the translations cannot
  disagree on what an instruction means; they share Q and P latencies,
  delay slots, the E bit and the runaway guard through one `State`.
- **Matching** (`micro::run`): a piece runs only where micro memory holds
  its code byte for byte. Writes to micro memory are counted (VIF's `MPG`,
  and the EE's accesses to 0x11000000 and 0x11008000); when the count
  moves, the pieces are matched again. Anything else, including an entry
  between two translated pieces, runs in the interpreter, pair by pair,
  and hands back to a piece as soon as one covers the address.
- **Switching it off.** `run --interpret` and the plain `sly-rs` binary
  interpret everything, EE and VUs.

## Checks

- The replay's checkpoints now hash both VUs' memories and VU1's
  registers too; lockstep compares them. The recompiled binary, with
  translated microcode, gives the interpreter's hashes at every checkpoint,
  and the same GS memory at field 1300 (`eb7df7d73cee4d19`).
- `vu-bench` (hidden) replays VU1 programs recorded with
  `$SLY_RS_VU_RECORD=PATH,SKIP,COUNT`, through the interpreter and the
  translations, and compares registers, data memory, the packets sent and
  pair counts. 4000 programs of the 3D stretch: identical.
- `cargo xtask test-full` builds the recompiled game and runs it in
  lockstep and through the replay.

## What it gains

On the 4000 recorded programs (2.1 million pairs), with no GS:

| | ns a pair |
| --- | --- |
| Interpreter | 19.5 (19.8 before the float changes below) |
| Translated, first version (one function per program) | 13.0 |
| Translated, in pieces, closures replaced by macros | 10.8 |
| Four-lane float operations: interpreter | 15.5 |
| Four-lane float operations: translated | 9.4 to 10.2 |

The first version compiled in almost 6 minutes and left the per-component
loop and the data loads as closure calls; pieces build in under a minute.

The whole recompiled build, 1300 fields of the replay:

| | 1300 fields | fields 1100-1200 | fields 1200-1300 (3D) |
| --- | --- | --- | --- |
| VU1 interpreted | 27.6 s | 31 ms a field | 32 ms a field |
| VU1 translated | 21.7 s | 19 ms a field | 26 ms a field |

Real time is 16.7 ms a field.

## Where the time is now

- **The float operations.** Each component converts to `f64`, operates,
  and rounds toward zero to a PS2 single by hand, twice for a
  multiply-add. `ps2_core::float::x4` now does the four components at
  once (add, sub, mul, madd, msub, max, min, ITOF, FTOI): every lane
  without branches, from operations SSE2 has (`f64` compares in place of
  64-bit integer ones), in plain Rust the compiler vectorises; the lanes
  outside `dest` are dropped. A test checks every lane against the scalar
  operations on 200,000 random operands (every exponent, both signs,
  near-cancelling pairs, NaN and infinite doubles). The flags are built
  four lanes per group. Building the adder's alignment mask from a float
  exponent instead of a shift was slower and was dropped. Timings move by
  a few tenths of a nanosecond from one build to the next.
- **The GS.** The machine waited for the GS 8.3 s of its 21.7 s. This
  note first blamed the GS's front end; measured, most of that time was
  primitives drawn one after another under too coarse hazard rules (see
  [sly1-graphics.md](sly1-graphics.md), "Narrower hazards").

[^runs]: Runs of Sly 1 with interpreted and translated VU microcode
[^plan]: The plan: ahead-of-time translation of every .DVP overlay
