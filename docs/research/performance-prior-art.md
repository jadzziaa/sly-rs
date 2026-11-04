---
type: Research Note
title: Performance techniques in rac-rs and paraLLEl-GS
description: Which performance techniques rac-rs (Ratchet & Clank recompiled) and paraLLEl-GS (a Vulkan compute GS) use that sly-rs does not yet, what each would buy here, and whether it could keep replays bit-exact.
tags: [research, prior-art, performance, rac-rs, parallel-gs, gpu, gs, ee, phase-9]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-04T23:17:56Z }
stale_after: 2027-02-01T00:00:00Z
sources:
  - id: rac-rs
    resource: https://github.com/AsobiCodes/rac-rs
    title: AsobiCodes/rac-rs v0.4.3 (commit 5b14154, 2026-10-31), read from a shallow clone, not built
  - id: pgs
    resource: https://github.com/Arntzen-Software/parallel-gs
    title: Arntzen-Software/parallel-gs (commit cc6184a), read from a shallow clone, not built
  - id: pgs-blog
    resource: https://themaister.net/blog/2024/07/03/playstation-2-gs-emulation-the-final-frontier-of-vulkan-compute-emulation/
    title: 'Themaister: PlayStation 2 GS emulation, the final frontier of Vulkan compute emulation (2024)'
  - id: profile
    resource: callgrind (--separate-threads) of sly-rs gs-replay on the 60-field play dump with --gpu-only, workspace release build, 2026-11-04
    title: Instructions by thread on the play dump
  - id: options
    resource: performance-options.md
    title: Performance after the GPU GS
  - id: user
    resource: human:august, 2026-11-04
    title: The user's request (compare with rac-rs and paraLLEl-GS for techniques left on the table)
---

# Performance techniques in rac-rs and paraLLEl-GS

The user asked which performance techniques the two nearest projects use
that sly-rs leaves on the table.[^user] Both were read, not built:
rac-rs[^rac-rs] (its architecture is in [rac-rs.md](rac-rs.md)) and
paraLLEl-GS with its author's write-up.[^pgs][^pgs-blog] sly-rs's own
state is in [performance-options.md](performance-options.md).[^options]

## Already the same, or further

- **Profile-guided optimisation**: rac-rs ships per-disc profiles; sly-rs
  trains on the new-game replay (option 1).
- **VU microcode translated ahead of time**: rac-rs generates VU1 code for
  its hot programs; sly-rs translates every overlay and checks it against
  the interpreter bit for bit ([vu-translation.md](vu-translation.md)).
- **The GS in compute shaders, tile-binned, with the local memory in a GPU
  buffer and the CLUT copies in a ring**: sly-rs's GPU GS follows
  paraLLEl-GS's design ([gpu-gs.md](gpu-gs.md)).

rac-rs's largest gain, a renderer that draws the 3D world from game RAM
instead of running VU1 and the GS, does not carry over: it is
Ratchet-specific, and sly-rs keeps the hardware path the replays check.
Neither project has anything for VU1's thread, sly-rs's limit today.

## Not done in sly-rs

| Technique | From | What it buys | Fit, and determinism |
| --- | --- | --- | --- |
| Triangle setup on the GPU (`gs/shaders/triangle_setup.comp`), then binning (`binning.comp`) | paraLLEl-GS | Primitive setup off the CPU | **Low.** In sly-rs setup runs on the drawer thread, not the GS thread, and is 1.5% of the drawer's instructions on the play dump (callgrind);[^profile] the drawer's large costs are transfers' pixels turned into word writes (43%) and binning with buffer copies (33%). paraLLEl-GS's author still finds it bound by geometry processing on the CPU.[^pgs-blog] |
| Single-sample paths when supersampling: sprites at 1×, flat primitives shaded once at the pixel centre (`single_sample_heuristic.comp`) | paraLLEl-GS | The cost of 3× and 4× in 2D-heavy games | **Measured not to fit:** in Sly sprites are under a tenth of the extra planes' cost, which is triangles' exact interpolation; adapted instead as float interpolation in planes other than the first (2026-11-04, [gpu-gs.md](gpu-gs.md)): 4× 23% faster, plane 0 unchanged |
| Deinterlacing and scan-out in shaders, no read back (`weave.frag`) | paraLLEl-GS | The window thread's deinterlacing and the per-field read back | Good, pictures only (option 5) |
| Fine binning with subgroup ballots, shading only the primitives that cover a tile (`ubershader.comp`) | paraLLEl-GS | GPU time on weak GPUs | Good, deterministic; needs wgpu's subgroup feature, not yet checked on the two GPUs here (option 6) |
| Lazy shading: an opaque pixel replaces the pending one, early depth tests turned late when needed | paraLLEl-GS | GPU fill | Medium: must leave the same words; gain on Sly unmeasured |
| Skipping iterations of loops that busy-poll a hardware register, counting the same instructions | rac-rs | EE time spent polling | Deterministic; sly-rs only skips kernel waits. Whether Sly polls anything needs a profile |
| Hand-written fast paths for hot functions (registers in locals, back to the compiled code on anything unusual) | rac-rs | About 9% less CPU time for rac-rs's game logic | Low until the EE is the limit again; generated from the disc, they would keep retail code out of the repository |
| Memory accesses to known ranges without the range check, cold paths outlined | rac-rs | EE speed, compile time | Small, no effect on determinism |
| Busy threads at high priority, Windows power throttling off | rac-rs | Hybrid CPUs and laptops | Cheap, for a Windows build |

## Left out on purpose

- **Hardware texture filtering and unswizzled texture caches**
  (paraLLEl-GS): the precision of hardware filtering can differ between GPU
  vendors, which would break the bit-exact comparison with the software GS.
- **Heuristics on the host's timeline, unsynchronised read backs, cheaper
  blending** (paraLLEl-GS): its own code needs a debug switch to make the
  first repeatable and calls the others inaccurate; replays must not
  depend on host time.
- **Running fields ahead of the display** (rac-rs): smoother frames at
  the cost of input latency, no throughput.

## What is on the table, ranked

Neither project relieves sly-rs's two limits, VU1's thread and the GS
thread: paraLLEl-GS too parses GIF packets and assembles vertices on the
CPU. On the play dump with the GPU alone drawing, the GS thread's
instructions go to GIF parsing (27% in `feed` and `packed` themselves),
vertex handling (11%), IMAGE transfers (9%), primitive bounds (6%), and
timers and allocation (about 7%);[^profile] speeding those up is sly-rs's
own work. Of the two projects' techniques:

1. Single-sample paths when supersampling: done in an adapted form, see
   the table.
2. Presenting on the GPU (option 5).
3. Fine binning and lazy shading for weak GPUs (option 6).
4. Polling-loop skipping on the EE, once a profile shows a loop worth it.
5. Triangle setup on the GPU, only if the drawer thread becomes a limit.

[^rac-rs]: AsobiCodes/rac-rs v0.4.3 (commit 5b14154, 2026-10-31), read from a shallow clone, not built
[^pgs]: Arntzen-Software/parallel-gs (commit cc6184a), read from a shallow clone, not built
[^pgs-blog]: Themaister: PlayStation 2 GS emulation, the final frontier of Vulkan compute emulation (2024)
[^profile]: Instructions by thread on the play dump
[^options]: Performance after the GPU GS
[^user]: The user's request (compare with rac-rs and paraLLEl-GS for techniques left on the table)
