---
type: Proposal
title: Performance after the GPU GS
description: Where host time goes now that the GS draws on the GPU (the machine thread, the GS thread, the drawer thread, the GPU, the window), measured on the user's prologue and GS dumps, and the options for weaker machines ranked by payoff, cost and risk to determinism.
tags: [proposal, performance, threads, gpu, vu1, gs, phase-9]
status: draft
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-04T22:57:53Z }
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

**The machine's waits for VU1's thread** (2026-11-04). `replay --timing`
now splits them: at V-blank for PATH1's events, and to read VU1's
memories. In fields 9,600-9,900 (without PGO) the machine waited 50% of
the time, 30% at V-blank and 19% to read; the reads are Sly's
`VU1_Interrupt` handler, which a VIF1 interrupt code starts in the middle
of each frame's chain and which reads back what VU1's programs computed,
a dependency the EE must keep. Both waits were longer than they needed to
be: the shadow VIF1 handed VU1's thread a transfer only once it had read
all of it (up to the interrupt or the end), so VU1 sat idle while the
shadow read. It now hands the words over 4,096 at a time, and VU1 starts
on them at once. VU1's thread itself got faster at VIF1's work: commands
read their data where it lies in the FIFO instead of copying it out
(2 ns a word before), the thread takes each piece without a copy, and
unmasked UNPACKs in MODE 0 without filling (V3-32, V4-32, V4-16, V4-8:
nearly all of Sly's) write memory directly instead of through the general
loop (5.6 ns a vector before), checked against it by a test. Nothing the
game sees changes: the prologue ends on the same hash, and lockstep, the
new-game replay and the full tests are unchanged.

| | Before | After |
| --- | --- | --- |
| Whole prologue, without PGO (the same machine, in turn) | 196.7 s | 150.4 s |
| Fields 9,600-9,900, without PGO | 14.5 ms a field (115% of NTSC) | 11.1 ms (151%) |
| Whole prologue, PGO | 148.6 s | 132.3 s |
| The median stretch, PGO | 235% of NTSC | 274% |
| The machine waiting for VU1's thread, PGO | 41.5% | 33.0% (at V-blanks 36.5 s, to read 7.2 s) |
| VU1's thread and the GS thread busy, PGO | 81.7%, 74.1% | 92.4%, 90.5% |

Another session's builds loaded the machine during these runs (load
average 8 to 34), so single stretches moved by up to a quarter between
runs; the comparisons above were made in turn on the same machine. VU1's
thread and the GS thread are now the limits together: the stretches the
GS thread holds at 97% busy run at 165-175% of NTSC. VU1's thread waited
for the GS 7.2 s (0.2 s before), so the next gains are in the GS thread
(GIF parsing and vertex handling, which no GPU technique takes over; see
[performance-prior-art.md](performance-prior-art.md)) and in VU1's
programs.

**The GS thread's own work** (2026-11-04). `gs-replay` now reports the GS
thread's time on packets apart from its waits for the batching thread:
about 0.42 s of the play dump's 0.5 s with the GPU alone drawing. Four
changes, none to what the GS draws:

- `Gs::draw` read the clock twice for every primitive to report the time
  spent queueing them, about 23 ns a read: 0.12 s of those 0.42. It now
  times only primitives it draws itself (software drawing on one thread),
  and the reports say "drawing primitives itself".
- The decoded pipeline state was rebuilt (and allocated) whenever a
  register other than a vertex's was written: 255,000 times for 2.66
  million primitives, 245,000 of them for PRIM, which Sly writes with
  every GIF tag. A PRIM write now keeps it when the attribute bits are
  unchanged (or PRMODECONT takes them from PRMODE); the cache remembers
  whether it was made for a kind that may be antialiased.
- PACKED RGBAQ, ST, UV and XYZ quadwords set their registers and queue
  their vertex directly instead of through `Gs::write`'s dispatch.
- VU1's thread sent each packet VU1 kicked as a job of its own (about
  3,000 a field, each an allocation, a channel message and two clock
  reads on the GS thread); consecutive packets on one path now go as one
  job, at most 4,096 quadwords, handed over after each of VU1's thread's
  jobs. While a dump is written they go one by one as before, so dumps
  keep their transfers.

On the play dump the GS thread is now busy about 0.27 s. Checked: the
thirteen dumps' GS and GPU memory, drawn by the GPU alone and by the CPU
on four threads, equal to the build before; `gs-replay --gpu` the same
in memory and pictures; a dump of three fields written by the game byte
for byte the same; lockstep, the new-game replay and the full tests
unchanged; the prologue's final hash the same.

| | Before | After |
| --- | --- | --- |
| GS thread busy over the prologue, without PGO (two pairs, in turn) | 80-81% | 54-58% |
| Whole prologue, without PGO, the quieter pair | 138.4 s | 132.7 s |
| Whole prologue, PGO | 132.3 s | 113.5 s (a quieter machine: load 6-12, against 8-18) |
| Slowest and median stretch, PGO | 149%, 274% of NTSC | 187%, 307% |
| VU1's thread waiting for the GS, PGO | 7.2 s | 0.0 s |
| VU1's programs, PGO | 3.29 ns a pair (waits included) | 2.73 ns |

VU1's thread (92% busy) is now the one limit; the machine waits for it
25% of the time, mostly at V-blanks.

**VU1's thread around its programs** (2026-11-04). Callgrind on the
new-game replay (an AVX2 build, since valgrind cannot run AVX-512; it
also ignores the rounding mode VU arithmetic sets, so the game diverges
under it from field 1,500, while the AVX2 build run natively reaches all
three hashes) put 14% of VU1's thread in VIF1's `run`, most of it the
direct unpacks element by element, and 10% in XGKICK, whose packet grew
a fresh vector (9.7 million reallocations) before being copied into the
GS's gathered job. Now:

- the direct unpacks take each vector's source words as one slice, with
  one bounds check, and write it as one 16-byte store (element by element
  only where the data runs short, a V3's W past the end);
- XGKICK reads its packet into a buffer kept between kicks and hands it
  over as a slice (`Sink::gif_slice`), which the GS's handle appends to
  the gathered job;
- one VU program in 16 is timed for the reports (`SAMPLED`), its time
  counted sixteen times, instead of two clock reads for each of the 61
  million programs.

Checked: all eight VU recordings give the interpreter's results; a GS
dump written by the game byte for byte the same; lockstep, the new-game
replay and the full tests unchanged; the prologue's final hash the same.

| | Before | After |
| --- | --- | --- |
| Prologue, without PGO (two pairs, in turn, load 4-6) | 130.2, 130.0 s | 126.4, 126.3 s |
| VU1's thread busy, without PGO | 117.8, 117.4 s | 110.6, 111.0 s |
| Prologue, PGO | 113.5 s | 108.8 s |
| VU1's thread busy, PGO | 104.5 s | 98.9 s |
| Slowest and median stretch, PGO | 187%, 307% of NTSC | 195%, 323% |

VU1's thread spends about 90% of its time in the translated programs
now; one program's body is a third of it, and the exponent-255 checks on
operands the translator cannot prove clean (loaded from memory) about 7%.

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
