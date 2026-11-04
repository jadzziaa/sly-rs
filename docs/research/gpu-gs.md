---
type: Proposal
title: A GPU GS on wgpu compute shaders
description: The design for sly-rs's GPU renderer - the GS's local memory in a GPU buffer and the pixel pipeline in compute shaders, as paraLLEl-GS does, sharing one integer arithmetic with the software GS so that the two can be compared bit for bit on GS dumps - with the facts checked, the milestones and their gates, and the decision it needs.
tags: [proposal, gs, gpu, wgpu, compute, phase-9]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-04T23:17:56Z }
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

**Texture reads, normative**: a primitive that textures from blocks it
writes (its own frame, or the depth it writes; such a primitive is drawn
alone) reads its texture as those blocks were before it, not as its own
pixels leave them. Drawing order then does not matter, so the GPU can draw
its pixels at once (it copies the blocks first). Sly does this with a
64 × 64 sprite that textures from its own frame, shifted.
Checked (2026-11-04): with this rule the title's 3D against PCSX2 is
87.19% identical (was 86.40%), 100% within 8 (was 99.9708%), largest
difference 3 (was 17); the dialog and the level intro are unchanged; one
thread and four give the same hashes; seven of the thirteen dumps are
re-baselined.

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

**M2, checked** (2026-11-04): `crates/ps2-gs-wgpu`. The software GS
hands a tap (`ps2_gs::export`) what it hands its drawer, in order:
each batch as primitives of 72 words (state, layouts, vertices set up as
the rasteriser sets them up, CLUTs), each transfer's pixels as word
writes. The GPU bins a batch into 16 × 16 tiles on the CPU and draws it
in one dispatch, a workgroup per tile, an invocation per pixel, each
pixel's primitives in order (`draw.wgsl`, the `fragment` pipeline line
for line); sub-word writes (16-bit frames, CLUT formats) use atomic and
and or. `gs-replay --gpu` compares the two memories after every field:
the title's 3D, the trial, our Paris dump and the two probes are the
same after every field, twice on the RX 9070 XT and once on the
integrated Radeon; breaking one line of the shader is caught at the
first field. A test draws 160 random cases (every format, filter,
function, test, blend and wrap mode, UV and STQ with Q of either sign)
through both and compares every word.

**M3, checked** (2026-11-04): points, lines, antialiased edges (AA1) and
sprites with STQ textures are walked on the CPU by the software GS's own
walks (now handing each fragment to a callback, which the software GS
draws and the encoder records), sent as fragments (eight words each) and
shaded on the GPU: a batch's fragments are binned by tile, an
antialiased triangle's after its inside, with coverage alpha and AA1's
blending in the shader. Hazards are the software GS's batch boundaries,
one dispatch each. Transfers' pixels reach the tap in the drawer's order
(a transfer that must wait for the queued batch comes after it). All
thirteen dumps, play60's 35,000 antialiased lines among them, are the
same after every field, twice on the RX 9070 XT and once on the
integrated Radeon. The random test adds points, lines, AA1 and STQ
sprites, and transfers among the primitives into the blocks they draw;
handing transfers over in the old order fails it.

Checked before M4: with the GPU drawing alone, a CPU memory with only
transfers in it would give every CLUT load in the thirteen dumps the
same values; but checked word by word against what primitives may have
written, two loads a field in play (CBP 5120 and 5121, pages the depth
buffer shares) read words a primitive's footprint covered. M4 therefore
loads CLUTs on the GPU.

**M4, checked** (2026-11-04):

- **The drawer thread feeds the GPU.** The tap now lives on the GS's
  drawer thread and is handed the drawer's items in its order (batches,
  transfers' pixels, local memory, display read-outs, read-backs), each
  after the CLUT loads before it. With `set_tap(tap, false)` the CPU no
  longer draws: encoding (in parallel for large batches), binning and
  submission happen off the GS thread. Local memory on the CPU goes
  stale; local-to-host and local-to-local transfers and dumps read it
  back from the GPU first.
- **CLUTs on the GPU.** Every CLUT load makes a new version of the CLUT,
  kept in one of 1,024 slots on the GPU (`clut.wgsl`: one workgroup,
  each invocation owning one word of every slot, the loads in order);
  primitives name their version's slot; a batch is drawn before loads
  could overwrite a slot it reads.
- **The display on the GPU** (`display.wgsl`, the read-out line for line,
  read back once a field). `gs-replay --gpu` compares it with the
  software read-out after every field: the same in all thirteen dumps
  (258 fields), and in the random test's cases.
- **Exact divisions without dividing.** GPUs emulate 64-bit division
  slowly: each quotient (depth, colour, fog, UV) is now estimated in
  floats from the barycentric weights and settled exactly from its
  integer remainder (`settle`), STQ's reciprocal likewise; depth and fog
  are skipped when unused. GPU time on 60 fields of play fell from
  0.38 s to 0.13 s on the RX 9070 XT, from 12.8 s to 3.0 s on the
  integrated Radeon; the results are unchanged.
- **The prologue** (the user's recording, 21,981 fields) replays with
  `--gpu` to its state hash; it took 224 s headless (305 s in software);
  the machine waited for the GS 0.8 s in all (34 s in software); every
  five-second stretch is at or above 108% of NTSC (in software 13 of 73
  were below, the slowest 88%).
- **Play** (`play --gpu`, or `gpu = true` under `[play]`): a session that
  went live after a recording wrote a recording that replays to its
  checkpoint.
- **The integrated Radeon** (2 compute units): the new-game replay
  reaches all three checkpoints with their hashes, at 39-61% of NTSC in
  3D, the GPU the bottleneck. The draw shader does an invocation's whole
  interpolation whenever any pixel of its wave is inside a small
  triangle; most of the remaining GPU time is that. Compacting covered
  pixels before shading (as paraLLEl-GS does) is the next step for weak
  GPUs.

**M5, checked** (2026-11-04): a higher internal resolution, `scale` S
of 1 to 4 (`play --gpu --scale 3`, `scale = 3` under `[play]`,
`replay --gpu --scale`, `gs-replay --gpu --scale`). Local memory is kept
S² times on the GPU, in sample planes: plane `j·S + i` is drawn with
every sample moved `(i, j)/S` pixel (16/S in 1/16 pixel; a primitive's
box widened by a pixel left and up, within the scissor), its textures
read from the same plane, so a frame drawn and then textured from stays
consistent. Plane 0 samples where the GS does, so it is local memory
exactly: CLUT loads and read-backs use it, and with any scale the
hashes cannot move. Transfers go to every plane; the display reads each
GS pixel as S × S pixels, one from each plane; the window filters a
scaled picture (bilinear) instead of taking its pixels.

- With `--scale 2`, all thirteen dumps are the same as the software GS
  after every field (memory, and the pictures' plane-0 pixels); a test
  compares random cases at 2× and 3× likewise. With the option off
  nothing changes: the software hashes and the 1× comparisons are as
  before.
- GPU time on 60 fields of play (RX 9070 XT, the GS's own work
  included): 0.73 s at 1×, 0.76 s at 2×, 0.83 s at 3×, 1.33 s at 4× (22
  ms a field: slower than real time).
- Seen in the window at 3×: geometry and textures are sharper. Two
  limits, fixed the same day: points, lines and antialiased edges (Sly's
  outlines, an antialiased line then a depth-only one) were walked once
  and the same in every plane, so they stayed blocky; they are now walked
  once per plane, moved by minus its offset, so each plane's fragments
  sit where its samples do (a plane's box widened as on the GPU). And
  effects that blur a small frame by texturing it from itself (the
  character shadows) showed a fine hatching, each plane blurring its own
  shifted copy: the GS now tracks blocks drawn into since a transfer last
  wrote them (`flag::RENDERED` when a primitive's texture lies there), and
  planes other than the first read such a texture as the one picture the
  planes make together, at the higher resolution (a texel clamped to an
  edge takes that edge's samples). The first plane still samples as the
  GS does: all thirteen dumps are the same as the software GS at 1× and
  2×. 60 fields of play: 0.81 s at 2×, 0.91 s at 3×, 1.40 s at 4×. A
  session played at 3× wrote a recording that replays to its checkpoint.

**Cheaper planes** (2026-11-04). At 4× the drawer thread spent 1.23 of
its 1.36 s submitting, that is waiting for the GPU. Turning parts of the
work off in planes other than the first, on the play dump at 4×: sprites
there cost about 0.08 s of the GPU's 1.1, CPU-walked fragments about
0.05, triangles' inside tests about 0.16, and the rest, about 0.9 s, was
triangles' exact interpolation in 64-bit integers (depth about 0.38 s,
colour, fog and texture coordinates about 0.54); shading the fragments
cost little. So paraLLEl-GS's single-sample path, which shades sprites
and flat primitives once per pixel, would gain little on Sly, whose cost
is its 3D triangles. Two changes instead:

- Planes other than the first interpolate colour, fog and texture
  coordinates in floats (`near`, `scaled` in `draw.wgsl`: the barycentric
  weights' dot product, held within the vertices' range; STQ as S/Q
  scaled as the exact quotient is). Depth stays exact in every plane, so
  that surfaces drawn at the same depth (decals, shadows) still meet.
  Plane 0 is unchanged, so no hash can move.
- A tile's CPU-walked fragments are ordered by plane, row and column (a
  pixel's keeping their order), and each invocation finds its own by
  binary search instead of reading every plane's.

Checked: all thirteen dumps the same as the software GS after every field
at 2× (memory and pictures); the random GPU test at 2× and 3×; at 3×
the pictures of the play dump and the title differ from before in 0.01%
and 0.00% of pixels, by at most 3 and 1. The play dump, 60 fields with
the GPU alone, five runs each in turn (another program was using the GPU
and three cores meanwhile, the same for both):

| Scale | Before | After |
| --- | --- | --- |
| 2× | 0.76 s | 0.645 s |
| 3× | 1.13 s | 0.92 s |
| 4× | 1.675 s | 1.29 s (21.5 ms a field) |

4× still takes longer than a field per field; the exact depth is now the
largest part of the planes' cost.

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
| M2 | compute rasteriser for triangles and sprites, the full `fragment` pipeline (not just the hot state: mixing CPU and GPU drawing would need constant syncs) (**done**, 2026-11-04) | GPU memory hash equals the software's on the dumps that use only those, twice in a row |
| M3 | hazards as dispatch boundaries, uploads, CLUT loads, AA1, lines, points (**done**, 2026-11-04) | all twelve dumps |
| M4 | play on the GPU, the display read on the GPU (**done**, 2026-11-04) | the prologue replays with its state hashes; `replay --timing` shows the GS off the critical path; runs on the integrated GPU |
| M5 | super-sampling, behind an option (**done**, 2026-11-04) | options off leave the hashes as they are |

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
change so that the GPU can match it exactly; in the event triangles'
texture coordinates changed (M1), and so did what a primitive that
textures from its own writes reads (2026-11-04, under that agreement);
PCSX2 parity improved both times.

[^parallel-gs]: PlayStation 2 GS emulation - the final frontier of Vulkan compute emulation (paraLLEl-GS)
[^parallel-gs-repo]: paraLLEl-GS (LGPL-3.0)
[^wgpu-int64]: wgpu 0.20.0 release notes (SHADER_INT64)
[^measurements]: Measurements on the reference machine
[^user]: The user's direction
