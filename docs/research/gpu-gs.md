---
type: Proposal
title: A GPU GS on wgpu compute shaders
description: The design for sly-rs's GPU renderer - the GS's local memory in a GPU buffer and the pixel pipeline in compute shaders, as paraLLEl-GS does, sharing one integer arithmetic with the software GS so that the two can be compared bit for bit on GS dumps - with the facts checked, the milestones and their gates, and the decision it needs.
tags: [proposal, gs, gpu, wgpu, compute, phase-9]
status: draft
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-04T06:08:03Z }
stale_after: 2027-03-01T00:00:00Z
sources:
  - id: parallel-gs
    resource: https://themaister.net/blog/2024/07/03/playstation-2-gs-emulation-the-final-frontier-of-vulkan-compute-emulation
    title: PlayStation 2 GS emulation - the final frontier of Vulkan compute emulation (paraLLEl-GS)
  - id: parallel-gs-repo
    resource: https://github.com/Arntzen-Software/parallel-gs
    title: paraLLEl-GS (LGPL-3.0)
  - id: wgpu-int64
    resource: https://newreleases.io/project/github/gfx-rs/wgpu/release/v0.20.0
    title: wgpu 0.20.0 release notes (SHADER_INT64)
  - id: measurements
    resource: sly-rs gs-replay and replay --timing on the user's Sly 1 dumps and prologue recording; vulkaninfo on the reference machine, 2026-11-03
    title: Measurements on the reference machine
  - id: user
    resource: human:august, 2026-11-03
    title: The user's direction (start the GPU renderer once software is near NTSC; weaker machines in mind; shared arithmetic agreed)
---

# A GPU GS on wgpu compute shaders

## Why now

The user's prologue recording runs at 124% of NTSC in software, but 9 of
its 73 five-second stretches stay below NTSC because the GS's thread is
busy about 99% of the time there, and on a 4-core machine the software
GS would cap play near 55% (see [sly1-graphics.md](sly1-graphics.md)).[^measurements]
Taking drawing off the CPU is what makes play hold NTSC throughout and
what weaker machines need.[^user]

## Two ways to put the GS on a GPU

| | Render targets as GPU textures, a texture cache (PCSX2's hardware renderer) | Local memory in a GPU buffer, the pipeline in compute shaders (paraLLEl-GS) |
| --- | --- | --- |
| Correctness | by eye and per-game fixes; cannot match the software GS bit for bit | the same memory, the same rules: can match the software GS exactly |
| Sly's hard cases (one-pixel antialiased outlines, frame blending, drawing from the frame being drawn) | each a texture-cache or blending special case | ordinary: the shader reads the memory it writes |
| Upscaling | render targets at a larger size; known bugs | super-sampling with a single-sampled reference copy |
| Speed | PCSX2 at 4x could not hold 60 fps on MGS2 | paraLLEl-GS ran MGS2 at 16x SSAA with 17% of an RX 7600 |

**Recommended: the compute design.** Everything in sly-rs is gated on bit-exact
comparison (VU digests, replay hashes, the twelve GS dumps); only the
compute design can be gated that way. It also matches the software GS's
own structure: batches split where one primitive depends on another
(`queue`, `footprint`), 32-pixel tiles drawn in order, page and block
tracking; paraLLEl-GS's coarse and fine binning, page tracker and in-order
shading per tile are the same ideas on a GPU.[^parallel-gs] Its source is
LGPL-3.0, compatible with sly-rs's GPL-3.0, so code as well as ideas may be
taken.[^parallel-gs-repo] Plan §5 and phase 9 said "a texture cache"; this
replaces that.

## One arithmetic, two implementations

`f64` is out on GPUs, and `f32` per pixel would make hashes differ from
one GPU to another (WGSL allows fused multiply-adds and vendors differ).
64-bit integers exist in wgpu (`SHADER_INT64`: Vulkan, DX12,
Metal)[^wgpu-int64] and on both GPUs here. Going through the software GS's
pixel path (2026-11-03), only one part needs a new definition:

| Per pixel | Software GS | On the GPU |
| --- | --- | --- |
| Inside test (edge functions, top-left rule) | 64-bit integers | the same, 64-bit |
| Depth, colour, fog | the exact quotient `Σ wₖ·aₖ / area` | the same quotient: area is the same for every pixel of a triangle, so multiply by a per-triangle reciprocal and correct by one (exact). Sly draws with 32-bit depth everywhere, so depth's sum can pass 64 bits: split into high and low halves, two quotients |
| Lines, points, antialiased edges | PCSX2's walks, in `f32` | walked on the CPU, as today; the GPU shades the fragments it is given |
| Sprites' UV | integer already (fits 32 bits) | the same |
| **Triangles' texture coordinates** | were `f64` (barycentric, true division by Q) | **new: integer, below** |

**Texture coordinates, normative** (`crates/ps2-gs/src/fixed.rs`; the
shaders translate it):

- Coordinates are integers in 1/16 texel, rounded down. Nearest sampling
  takes `c >> 4`, bilinear `c - 8` then shifts. Primitives other than
  triangles convert their float coordinates for the sampling mode as
  sampling did (nearest: the texel the value lies in; bilinear: `u·16`
  truncated), so their pixels are unchanged.
- UV triangles: `floor(Σ wₖ·Uₖ / area)`, `Uₖ` the vertices' UV in 1/16
  texel (exact; the same reciprocal trick on the GPU).
- STQ triangles: at setup, each of S, T, Q is scaled by a power of two
  per triangle so that its largest magnitude lies in `[2^23, 2^24)`,
  rounded to nearest (non-finite values count as 0). Per pixel,
  `Σ wₖ·Sₖ` and `Σ wₖ·Qₖ` are exact 64-bit integers (at most
  3·2^35·2^24). Their ratio: normalise both by their leading zeros; take
  `2^62 / Q's top 32 bits` once (one exact division, rounded down and up;
  shared by u and v); multiply by S's top 32 bits; shift by
  `62 − k + zeros(S) − zeros(Q)` with `k = scale(Q) − scale(S) + log2(TW)
  + 4`. A positive quotient uses the reciprocal rounded up and drops the
  fraction, a negative one the reciprocal rounded down and rounds its
  magnitude up, so an exact quotient comes out exactly (a texture
  coordinate constant over a triangle stays the same texel). Magnitudes
  are held to 2^30. Q = 0 counts as 1.

**Checked** (2026-11-03): with the old `f64` path selected, all twelve
dumps hashed as before (the refactor itself is exact); with the new one,
the dumps with sprites and lines only are unchanged, and PCSX2 parity is
as good or slightly better: the title's 3D 86.40% identical (was
85.08%), 99.9708% within 8 (was 99.9703%), largest 17; the level intro
98.11% (was 98.109%), 100% within 8, largest 6; the dialog unchanged
(99.928%, 99.987%, 79). The dump hashes are re-recorded. The replay
checkpoints do not move: the state hash covers RAM, the scratchpad and
both VUs, not GS memory.

**M0, checked** (2026-11-04): `play` draws with wgpu 30
(`crates/sly-app/src/screen.rs`). The picture is uploaded as a texture
and a fragment shader maps window pixels to picture pixels with the CPU
painter's integer arithmetic; a test renders offscreen and compares with
that painter (kept in the test as the reference) over four picture sizes
and five window sizes: identical on the RX 9070 XT and the integrated
Radeon, through Vulkan and OpenGL. A session played through the short
recording in the window wrote a recording that replays to its checkpoint.

## Facts checked

- **Sly never reads GS memory back.** On six dumps (ours and PCSX2's, the
  dialog, title, level and play) every transfer is host to local; none
  local to host or local to local. Play needs no GPU-to-CPU sync; only
  screenshots, dumps and hash checks do.
- **The GPUs here**: an RX 9070 XT (RADV, Vulkan 1.4) and the Ryzen's
  integrated Radeon, a stand-in for weak machines. Both have 64-bit and
  16-bit shader integers, 16-bit storage, subgroups of 64, 1,024
  invocations a workgroup, storage buffers far over 4 MB.
- **Sly draws with 32-bit depth everywhere**; Q at STQ vertices ranges
  over about 2^-16 to 1 and is sometimes negative (about 12,000 vertices
  in 60 fields of play); no sprite uses STQ in any dump.
- **The GS's work as the GPU would see it**, on 60 fields of play: 44,000
  primitives and 2.15 million fragments a field; 90% of the pixels in one
  state (a CLUT texture, bilinear, blending, fog); 795 primitives drawn
  alone and 1,376 batches in 60 fields (dispatch boundaries); 478 uploads.

## What stays and what goes

- Stays on the CPU: the GS thread's parsing of GIF packets, `Prim` setup,
  the batch rules (`queue`, `footprint`, `Uses`), now deciding dispatch
  boundaries; uploads and CLUT loads, sent to the GPU in order.
- Goes to the GPU: the drawer thread and its pool, the pixel pipeline,
  the display read-out (CRTC merge and deinterlacing).
- Ported, not approximated: AA1 edges follow PCSX2's walk (`aa.rs`), where
  paraLLEl-GS draws parallelograms.

## Milestones

| | What | Gate |
| --- | --- | --- |
| M0 | wgpu in `play`: device, surface, presentation (softbuffer goes); the software GS still draws (**done**, 2026-11-04) | the window shows the same frames; `play` still records and replays |
| M1 | the shared integer arithmetic in the software GS (**done**, 2026-11-03: only triangles' texture coordinates changed) | PCSX2 parity re-run; twelve dumps re-baselined |
| M2 | compute rasteriser for triangles and sprites, the full `fragment` pipeline (not just the hot state: mixing CPU and GPU drawing would need constant syncs) | GPU memory hash equals the software's on the dumps that use only those, twice in a row |
| M3 | hazards as dispatch boundaries, uploads, CLUT loads, AA1, lines, points | all twelve dumps |
| M4 | play on the GPU, the display read on the GPU | the prologue replays with its state hashes; `replay --timing` shows the GS off the critical path; runs on the integrated GPU |
| M5 | super-sampling, behind an option | options off leave the hashes as they are |

## Risks

- The arithmetic change makes pictures differ from today's, slightly; if
  PCSX2 parity drops below the phase 6 gate, the arithmetic is wrong, not
  the gate.
- Subgroup operations differ between vendors (sizes 16 to 64); designs
  that depend on a size need a path for each.
- wgpu's compute and WGSL lag Vulkan GLSL in places (no specialization
  constants as such: pipeline-overridable constants instead; bindless
  textures behind native features). Local memory read from the buffer
  with the swizzle in the shader avoids textures altogether at first.
- GPU timing: the CPU builds geometry; paraLLEl-GS found its CPU side the
  bottleneck at high SSAA. Here the GS thread's parsing and setup stays,
  about 10.6 ms a field on the reference CPU.

## Decided

The user agreed (2026-11-03) that the reference renderer's pixels may
change so that the GPU can match it exactly; in the event only triangles'
texture coordinates changed, and PCSX2 parity improved slightly.

[^parallel-gs]: PlayStation 2 GS emulation - the final frontier of Vulkan compute emulation (paraLLEl-GS)
[^parallel-gs-repo]: paraLLEl-GS (LGPL-3.0)
[^wgpu-int64]: wgpu 0.20.0 release notes (SHADER_INT64)
[^measurements]: Measurements on the reference machine
[^user]: The user's direction
