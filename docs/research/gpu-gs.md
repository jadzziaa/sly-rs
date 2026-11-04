---
type: Proposal
title: A GPU GS on wgpu compute shaders
description: The design for sly-rs's GPU renderer - the GS's local memory in a GPU buffer and the pixel pipeline in compute shaders, as paraLLEl-GS does, sharing one integer arithmetic with the software GS so that the two can be compared bit for bit on GS dumps - with the facts checked, the milestones and their gates, and the decision it needs.
tags: [proposal, gs, gpu, wgpu, compute, phase-9]
status: draft
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-04T03:00:00Z }
stale_after: 2027-03-01T00:00:00Z
sources:
  - id: parallel-gs
    resource: https://themaister.net/blog/2024/07/03/playstation-2-gs-emulation-the-final-frontier-of-vulkan-compute-emulation
    title: "PlayStation 2 GS emulation - the final frontier of Vulkan compute emulation (paraLLEl-GS)"
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
    title: The user's direction (start the GPU renderer once software is near NTSC; weaker machines in mind)
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

A GPU cannot run the software GS's pixel arithmetic as it is: 64-bit edge
functions, a 64- and 128-bit division per pixel for depth, colour and fog
(exact barycentric weights), and `f64` texture coordinates with a true
division by Q. `f64` is out on GPUs; 64-bit integers exist in wgpu
(`SHADER_INT64`: Vulkan, DX12, Metal)[^wgpu-int64] and on both GPUs here,
but not in a per-pixel loop that must be fast on weak ones. And `f32` in the
per-pixel path would not do either: WGSL allows fused multiply-adds and
vendors differ, so hashes would differ from one GPU to another.

So: one all-integer arithmetic, 32-bit in the per-pixel loop, defined
once and implemented twice.

- Setup (on the CPU, where 64 bits are free): edge functions relative to
  the primitive's or tile's origin so that per-pixel values fit in 32
  bits; plane equations for Z, RGBA and fog in fixed point with a
  specified rounding; a fixed-point reciprocal for 1/Q (paraLLEl-GS uses
  about 24 bits, for reproducibility).
- The software GS changes to that arithmetic first, on the CPU, where
  the tools to check it exist. Its pixels change slightly; the PCSX2
  parity gate of phase 6 (within 8 levels on at least 99.9% of pixels on
  the dialog, title and level dumps) is run again, then the twelve dump
  hashes are re-recorded. The replay checkpoints do not move: the state
  hash covers RAM, the scratchpad and both VUs, not GS memory.
- The shaders then translate that arithmetic, gated on the GPU's memory
  hash equal to the software GS's on every dump.

## Facts checked

- **Sly never reads GS memory back.** On six dumps (ours and PCSX2's, the
  dialog, title, level and play) every transfer is host to local; none
  local to host or local to local. Play needs no GPU-to-CPU sync; only
  screenshots, dumps and hash checks do.
- **The GPUs here**: an RX 9070 XT (RADV, Vulkan 1.4) and the Ryzen's
  integrated Radeon, a stand-in for weak machines. Both have 64-bit and
  16-bit shader integers, 16-bit storage, subgroups of 64, 1,024
  invocations a workgroup, storage buffers far over 4 MB.
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
| M0 | wgpu in `play`: device, surface, presentation (softbuffer goes); the software GS still draws | the window shows the same frames; `play` still records and replays |
| M1 | the shared integer arithmetic in the software GS | PCSX2 parity re-run; twelve dumps re-baselined |
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

## Decision needed

M1 changes the reference renderer's pixels (slightly) so that the GPU can
match it exactly. It is a decision about the reference, not an
implementation detail: the alternative is a GPU renderer checked by eye.

[^parallel-gs]: PlayStation 2 GS emulation - the final frontier of Vulkan compute emulation (paraLLEl-GS)
[^parallel-gs-repo]: paraLLEl-GS (LGPL-3.0)
[^wgpu-int64]: wgpu 0.20.0 release notes (SHADER_INT64)
[^measurements]: Measurements on the reference machine
[^user]: The user's direction
