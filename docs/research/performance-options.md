---
type: Proposal
title: Performance after the GPU GS
description: Where host time goes now that the GS draws on the GPU (the machine thread, the GS thread, the drawer thread, the GPU, the window), measured on the user's prologue and GS dumps, and the options for weaker machines ranked by payoff, cost and risk to determinism.
tags: [proposal, performance, threads, gpu, vu1, gs, phase-9]
status: draft
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-04T12:51:34Z }
stale_after: 2027-02-01T00:00:00Z
sources:
  - id: prologue
    resource: generated/sly1 replay prologue.toml --timing --every 300 --gpu (the user's recording, 21,981 fields), RX 9070 XT and Ryzen 9 9950X, 2026-11-04
    title: The prologue with the GPU drawing
  - id: dumps
    resource: sly-rs gs-replay on the 60-field play dump (play60.gs), --gpu-only at scales 1 to 4, and on the integrated Radeon, 2026-11-04
    title: GS dump timings
  - id: gpu-gs
    resource: research/gpu-gs.md
    title: A GPU GS on wgpu compute shaders
  - id: user
    resource: human:august, 2026-11-04
    title: The user's request (review the options now that the GPU stack works, weaker machines in mind)
---

# Performance after the GPU GS

## Where the time goes now

With the GPU drawing, every five-second stretch of the user's prologue
runs at 108% of NTSC or more on the reference machine (a Ryzen 9 9950X
and an RX 9070 XT), and the machine almost never waits for the
GS.[^prologue] The headroom is small: the slowest stretches take 15.0 to
15.4 ms a field against NTSC's 16.7. Four threads and the GPU share the
work; in the slowest stretch (fields 9,600 to 9,900):

| Where | A field | What it is |
| --- | --- | --- |
| Machine thread (the critical path) | 15.4 ms | VU1 programs 48% (about 7.4 ms), the rest of VIF1 22% (3.4 ms), the EE and the rest 29% (4.5 ms), the IOP 1% |
| GS thread | 13.4 ms (87% busy) | on the play dump: GIF parsing and vertex handling about 45%, primitive setup, footprints and batching 43%, waiting for the drawer 11%[^dumps] |
| Drawer thread (feeds the GPU) | about 5 ms | encoding (in parallel), binning, submission; transfers' pixels |
| GPU, RX 9070 XT | about 2.2 ms at 1× (time spent submitting, which waits on the GPU) | the play dump, GS thread and drawer included, takes 0.74, 0.81, 0.91 and 1.40 s for 60 fields at 1× to 4×: 4× (23 ms a field) is the only scale the GPU holds below NTSC |
| GPU, integrated Radeon (2 units) | about 50 ms at 1× | the same shader; the new-game replay runs at 39-61% of NTSC in 3D |
| Window thread | 0.4 ms at 1×, 3.8 ms at 3× | deinterlacing on the CPU, then uploading the picture again |

So on a CPU about a quarter slower per thread, the slowest scenes would
fall below NTSC, held by the machine thread and soon after by the GS
thread; a GPU is only the limit when it is as weak as the integrated
Radeon here.

## Options

Gains are estimates from the measurements above, to be checked by
`replay --timing` on the prologue before and after.

| | Option | Relieves | Expected gain | Cost and risk |
| --- | --- | --- | --- | --- |
| 1 | **Profile-guided optimisation** of the recompiled game (`cargo pgo`, the prologue as the training run) | every CPU thread | typically 5-15% | small: a build step; the code is the same, so replays and hashes cannot change |
| 2 | **Split the GS thread**: it parses GIF packets and builds primitives; footprints, batching and the upload and CLUT-load ordering move to the drawer thread, which already runs in order | GS thread | about 40% of it (the 43% spent queueing) | medium; the order of everything is kept, so the GPU's output and the replays are unchanged and `gs-replay --gpu` checks it |
| 3 | **VIF1 and VU1 on a thread of their own**, overlapped with the EE (as the GS already is) | machine thread | up to about 70% of it in the slowest scenes (VU1 and VIF1 are 70% there) | large: the EE reads VIF1 and VU1 state (status registers, DMA completion, VU1 memory); each such read becomes a synchronisation point, and timing the game sees must stay counted in cycles, not host time, for replays to stay exact |
| 4 | **More from the VU1 translator** (registers across blocks, fewer flag snapshots, longer pieces) | machine thread | perhaps 20-30% of the VU1 programs' 48% | medium to large; checked as before by `vu-bench` digests and the interpreter |
| 5 | **Present on the GPU**: one device for the GS and the window; deinterlacing and scaling in a shader; no per-field read-back | drawer and window threads | 5 MB read back and 10 MB uploaded a field at 3× go, with 3.8 ms of deinterlacing | medium; pictures only, nothing the game sees |
| 6 | **Weak GPUs**: shade only covered pixels (compact them per tile first, as paraLLEl-GS does), and cheaper setup per triangle | the GPU | perhaps 3-5× on the integrated Radeon, making such GPUs usable | medium to large; checked by `gs-replay --gpu` on the thirteen dumps and the random test |
| 7 | **One GPU submission a field** instead of about thirty | drawer thread | small | small |

## Done

**1, profile-guided optimisation** (2026-11-04): `cargo xtask recomp
sly1 --pgo games/sly1/replays/new-game.toml` builds an instrumented
binary, plays the new-game replay on it drawing on the CPU and on the
GPU (two drawing threads: with sixteen, their updates of the same
counters made the instrumented binary crawl), merges the counts with
`llvm-profdata` and builds the release binary with them; about nine
minutes. Measured on the user's prologue, which it was not trained on,
with the same state hash at the end:

| | Without | With PGO |
| --- | --- | --- |
| GPU drawing, whole prologue | 229.1 s, 223.3 s | 204.3 s |
| GPU drawing, slowest stretches | 106-110% of NTSC (15.1-15.7 ms a field) | 120% (13.9 ms) |
| Software drawing, whole prologue | 308.8 s | 298.6 s |
| Software drawing, stretches below NTSC | 12 (slowest 87%) | 10 (slowest 89%) |

With the GPU drawing, the slowest stretches now have the GS thread 93-95%
busy: it is the limit next, as foreseen, so 2 comes next.

**2, in part** (2026-11-04). Timed on the play dump with the GPU alone
drawing, the GS thread's 0.755 s for 60 fields went about 40% to parsing
GIF packets, 25% to building primitives, 30% to batching (12% to
footprints), and inside the parsing 15% to CLUT loads, which read local
memory on the CPU that the GPU does not need: it loads its CLUTs itself,
in order. With the GPU alone drawing, a CLUT load now only hands over a
batch that draws into what it reads (without waiting for it) and records
the load for the GPU: 0.660 s for those 60 fields, every dump's GPU
memory still equal to the software GS's. On the prologue (PGO build) the
GS thread is now 81-83% busy in the slowest stretches (74% over the
whole, from 90%), and the speed is unchanged (203.1 s; the slowest
stretches 119%), because the machine thread holds them at 13.5-14.0 ms a
field. Moving batching to the drawer thread would free more of the GS
thread but gain nothing until the machine thread is faster, so it waits;
3 or 4 (the machine thread) comes first.

**3** (2026-11-04): VIF1 and VU1 run on a thread of their own, the EE
keeping a shadow of VIF1 ([vu1-thread.md](vu1-thread.md)); replays,
lockstep and GS dumps unchanged; GS events in packets VU1 builds are
raised at the next V-blank (Sly 1 has none). The prologue (PGO, GPU
drawing) takes 171.8 s instead of 203.1 s, its slowest stretches 129%
then 137-149% of NTSC instead of 119-121%. The GS thread is now the limit
(90-97% busy in the slowest stretches), VU1's thread next (78-88%): the
rest of 2 (batching on the drawer thread) pays now, then 4.

**2, the rest** (2026-11-04): with the GPU alone drawing, batching
(footprints, conflicts, the order of uploads and CLUT loads) runs on a
thread of its own between the GS thread and the drawer, rather than on
the drawer thread, which would have lost the overlap with drawing when
the CPU draws. The GS thread hands it primitives, transfers' pixels, CLUT
loads and requests for pictures in order, 256 at a time; nothing the GS
thread does then reads local memory, so it waits only for read backs,
copies, dumps and reports. Two more changes came with it: IMAGE data
reaches the transfer in runs instead of one quadword at a time (the play
dump made 791,000 upload operations of 478 transfers), and a call the
machine posts to the GS (the window's picture each field) no longer
waits for everything queued to be drawn first. When the CPU draws,
batching stays on the GS thread: its CLUT loads read local memory there,
and moving them would mean resolving each primitive's CLUT on the
batching thread.

Checked: the thirteen dumps' GS and GPU memory with the GPU alone, at 1×
and 2× and with one drawing thread, equal to the software GS's from
before; `gs-replay --gpu` the same in memory and pictures; the random
GPU test now draws every case a second time with the GPU alone; lockstep
equal for 300 fields; the new-game replay's three hashes with and
without the GPU; the prologue's final hash the same.[^prologue]

| | Batching on the GS thread | On its own thread |
| --- | --- | --- |
| Play dump, GPU alone, 60 fields | 0.66 s | 0.50 s (the drawer thread the limit) |
| Whole prologue (PGO, GPU drawing) | 171.8 s | 157.1 s |
| Slowest stretch | 129% of NTSC | 133% |
| The next slowest | 137-151% | 163-170% |
| Their threads | GS thread 90-97%, VU1's 78-88% | GS thread 68-82%, batching 33-52%, VU1's 78-86% |
| VU1's thread waiting for the GS | 12.4 s | 0.35 s |

VU1's programs now take 3.20 ns a pair (3.64 before): the figure counts
VU1's thread's waits for the GS, which were most of the difference from
the machine thread's 3.1. The machine waits for VU1's thread 38-58% of
the slowest stretches: VU1 is the limit now, so 4 comes next, with the
shadow VIF1 (12% of the machine thread) after it.

**4, in part** (2026-11-04): the translator now decides, block by block,
which pairs' MAC flags anything can read and which operands its own
arithmetic made clean of exponent 255, and leaves out the rest of that
work ([vu-translation.md](vu-translation.md)). On the 6,000 recorded
gameplay programs, 2.13 ns a pair instead of 2.68 (without PGO); every
recording gives the interpreter's result, also handing back after every
block. In the prologue (PGO, GPU drawing), VU1's programs took 93.6 s
instead of 103.0 s (2.91 ns a pair, from 3.20), and the whole 148.6 s
instead of 157.1 s, with the same final hash; lockstep, the new-game
replay and the full tests unchanged.

| | Before | After |
| --- | --- | --- |
| Slowest stretch | 133% of NTSC | 137% |
| The next slowest | 163-170% | 170-181% |
| Their threads | VU1's 77-86%, GS 68-82% | VU1's 77-85%, GS 72-89% |

In the slowest stretch (fields 9,600-9,900) the machine waits for VU1's
thread 57% of the time while that thread is busy only 77%: the waits are
not all for VU1's work, which is where to look next, with the shadow VIF1
(13% of the machine thread).

## Recommendation

For weaker CPUs, which is what decides whether most machines hold NTSC:
1 first (cheap, every thread), then 2 (clear and checkable), then 3
(the largest gain, and the riskiest). For weaker GPUs, 6. For the higher
resolutions, 5. Speed work was set aside on 2026-11-03 once the
recompiled game ran at about the console's speed; this note is the
review the user asked for now that the GPU stack works, not a decision
to resume it.[^user]

[^prologue]: The prologue with the GPU drawing
[^dumps]: GS dump timings
[^user]: The user's request (review the options now that the GPU stack works, weaker machines in mind)
