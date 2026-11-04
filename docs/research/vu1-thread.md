---
type: Finding
title: VIF1 and VU1 on a thread of their own
description: How sly-rs runs VIF1, VU1 and the GS's feed on their own thread while keeping replays exact - a shadow VIF1 on the EE's side, every GS-bound packet through one ordered queue, the few points where the EE waits - with the rule it changes (PATH1's GS events raised at the next V-blank), the checks and the measured gain.
tags: [finding, performance, threads, vif, vu1, gs, determinism, phase-9]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-04T11:10:00Z }
sources:
  - id: counts
    resource: counters on generated/sly1 replay of an 8,000-field cut of the user's prologue (gameplay from field 3,644), 2026-11-04
    title: How the EE and VIF1/VU1 interact in Sly 1
  - id: prologue
    resource: generated/sly1 replay prologue.toml --timing --every 300 --gpu (the user's recording, 21,981 fields), PGO builds, Ryzen 9 9950X and RX 9070 XT, 2026-11-04
    title: The prologue with and without VU1's thread
  - id: user
    resource: human:august, 2026-11-04
    title: The user's choice (VIF1 and VU1 on their own thread first)
---

# VIF1 and VU1 on a thread of their own

## What changed

Until now a VIF1 DMA transfer ran whole when its CHCR was written: the
chain's words through VIF1, every VU1 program they started, the packets
those kicked to the GS; then its end was scheduled from the modelled DMA
time (tags and quadwords moved). Now `crates/ps2-ee/src/vu1.rs` runs the
real VIF1, VU1's registers and memories and the GS's handle on a thread
of their own, and the EE keeps a **shadow** VIF1 (`Vif::shadow`): it reads
the same codes and data and keeps the same registers, stalls and
interrupts, but starts no VU program, loads no micro code and unpacks
only what changes its own registers (ROW in modes 2 and 3). None of
VIF1's registers, nor how far a transfer gets, depends on what VU1 does,
so the EE knows them as before, at once. The words the shadow read go to
the thread in order.

Everything bound for the GS goes through the thread's queue too (PATH3
from the GIF's DMA and FIFO, pictures, dumps), so the GS gets what it got
before, in the same order.

## Where the EE waits, and the rule that changed

| The EE needs | How |
| --- | --- |
| VIF1's registers, a transfer's end, its interrupt | from the shadow, at once |
| GS events (SIGNAL, FINISH, LABEL) in PATH2 (DIRECT) and PATH3 | looked for on the EE's side, raised when the transfer ends, as before |
| GS events in PATH1 (packets VU1 builds and kicks) | the thread looks for them; the EE raises them at the **first V-blank start after the transfer ends**, waiting for the thread there if need be |
| VU1's memories (EE loads and stores) | through MMIO: a load waits for the thread, a store is queued |
| VU1's registers and memories for a state hash or a lockstep comparison | copied back, after the thread has run everything sent before |
| VU1 programs the EE starts (CMSAR1) | queued |

The PATH1 rule is the only change in behaviour, and it is deterministic:
it depends on emulated time alone. In Sly 1 it changes nothing: over
8,000 fields of play the chains' 7,886 FINISH writes all came through
DIRECT, PATH1 asked for none, and the SIGNAL and FINISH interrupts were
unmasked at every transfer's end, so the EE does need PATH2's at once.[^counts]
Per field of play Sly sends about one large VIF1 chain, ending about
212,000 cycles (6% of a field) after its kick; waiting for VU1 there
would have left little to overlap, the V-blank rule leaves up to a field.

## Checks

- The new-game replay reaches its three checkpoints with the same state
  hashes (they cover VU1's registers and memories, copied back).
- `lockstep` (recompiled against interpreted, 300 fields) equal at every
  field.
- A GS dump of three fields of play written by the binary from before
  and by this one: byte for byte the same (34 MB).
- The user's prologue ends on the same state hash, `bf6bae6858dce562`.

## Measured

On the prologue with the GPU drawing, PGO builds:[^prologue]

| | VU1 on the machine thread | On its own thread |
| --- | --- | --- |
| Whole prologue | 203.1 s | 171.8 s |
| Slowest stretches | 119-121% of NTSC | 129%, then 137-149% |
| Slowest stretches' threads | machine thread the limit, GS thread 81-83% | GS thread 90-97%, VU1's thread 78-88%, the machine thread waiting for VU1's thread about half the time |

The GS thread is the limit again, VU1's thread next; the shadow VIF1 costs
the machine thread 11%. VU1's programs take 3.6 ns a pair on their
thread, against 3.1 on the machine thread: why is not measured yet.[^user]

[^counts]: How the EE and VIF1/VU1 interact in Sly 1
[^prologue]: The prologue with and without VU1's thread
[^user]: The user's choice (VIF1 and VU1 on their own thread first)
