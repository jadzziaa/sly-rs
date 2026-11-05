---
type: Finding
title: Widescreen in Sly 1
description: 'How sly-rs makes Sly 1 draw at 16:9 behind an option - the camera''s aspect ratio set in one place, changed by an instruction patch the interpreter runs while memory keeps the game''s bytes - and the checks: off leaves replays exact, on stays equal in lockstep, widens the 3D view, and keeps the 2D interface at 4:3 by narrowing its GIF packets.'
tags: [widescreen, enhancement, patch, camera, phase-9]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-05T11:02:38Z }
sources:
  - id: disasm
    resource: sly-rs disasm sly1 over the whole .text section (SCUS-97198), 2026-11-05
    title: Sly 1's code, disassembled
  - id: decomp
    resource: https://github.com/TheOnlyZac/sly1
    title: The Sly 1 decompilation (CC0; function names from the May 2002 prototype's symbols)
  - id: pnach
    resource: https://github.com/PCSX2/pcsx2_patches
    title: 'pcsx2_patches: the community widescreen patch for Sly 1 and 2 (lui 0x3FA6 to 0x3FDD)'
  - id: runs
    resource: sly-rs replay and lockstep on games/sly1/replays/new-game.toml and a copy with widescreen = true, 2026-11-05
    title: The checks
  - id: anchors
    resource: human:august, 2026-11-05
    title: 'The user''s choice: the binocucom''s frame stays stretched; the portraits sit at the screen''s edges as in 4:3'
  - id: hud-dump
    resource: a GS dump of the widescreen new-game replay at fields 2501-2502 (the binocucom), its GIF transfers grouped by path, frame, depth test and scissor; the user's prologue recording (first 7,000 fields) with and without widescreen, pictures every 250 fields, 2026-11-05
    title: How Sly 1's 2D reaches the GS
---

# Widescreen in Sly 1

`widescreen = true` under `[play]` (or `play --widescreen`, `run
--widescreen`, `widescreen = true` in a replay) makes Sly 1 draw for a
16:9 screen and the window show it at 16:9. Off, nothing is installed and
the game runs exactly as before.

## The camera's aspect ratio

Sly 1 keeps its camera's aspect ratio in `CM::rAspect` (offset `0x1E0`).
Only `InitCm` (0x00143648) writes it: `lui $at, 0x3FA6; ori $at, $at,
0x3BD8`, 1.298701, stored at 0x001436E0.[^disasm] `RecalcCmFrustrum`, which
builds the view frustum and culling planes from it, `SetCmLookAtSmooth` and
`UpdateCplook` read it.[^decomp] Eight more `lui $at, 0x3FA6` in the
executable load 1.3 (`ori 0x6666`) in look-at and flashlight code; they are
not the aspect and are left alone.

Widescreen multiplies the aspect by 4/3: 1.731601 (`0x3FDDA520`), both
instructions replaced. The community's PCSX2 patch changes the `lui` only,
which gives 1.728389, 0.2% short.[^pnach]

## A patch the interpreter runs

The plan's rules: guest code bytes are never modified (Sly 1 checksums its
own code), and an option that is off leaves the machine's state exactly as
it was. So the patch is not written to memory:

- `Machine::patch(address, word)` makes the EE run `word` at `address`
  instead of what memory holds; memory keeps the original. The interpreter's
  fetch looks the patch map up only when it is not empty.
- Patched functions are left out of generated code
  (`sly_common::hooks::left_out`, as hooked ones were), so both builds run
  `InitCm` in the interpreter and agree to the cycle. It runs once per
  camera set-up, so this costs nothing measurable.
- `sly_common::hooks::use_widescreen` applies the patch only if memory holds
  the words it expects (the supported build); Sly 2 and 3 have none yet
  (their patches change the same kind of constant: `0x3FA6` in Sly 2,
  `0x4420` in Sly 3[^pnach]).

The window's shader places the picture in the largest 16:9 rectangle
instead of 4:3. `replay --frames` and `run --screenshot` keep the GS's
pixels, unstretched.

## Checks

Over the new-game replay (2,600 fields: dialog, title, new game, the first
level's intro), recompiled build regenerated without `InitCm`:[^runs]

| Run | Field 2600 | VU1 programs |
| --- | --- | --- |
| Committed replay, option off | `f9e37a7eecfc97ae`, the recorded hash | 5,328,876 |
| Widescreen | world 0, level 3 as expected; hash `222925fd34f91eaa` | 5,934,499 (+11%) |

- Off, every checkpoint keeps its recorded hash.
- On, the 3D view widens: more rooftops on both sides at the title and
  through the binocucom, the moon still round at 16:9. The 11% more VU1
  programs are the extra scenery the wider frustum lets through culling.
- `lockstep` with widescreen on (the recompiled build and the interpreter
  side by side, through the title's 3D scene): 1,100 fields equal.

## The 2D interface

Sly 1 lays its interface out for 4:3, and at 16:9 it stretched (as with
the PCSX2 patch). The interface code is mostly not decompiled and every
kind of element is drawn its own way, so the fix is in the GS's input
instead (`ps2-gs`'s `widescreen.rs`): with widescreen on, each GIF
transfer passes through a filter before the GS (and so both the CPU and
GPU renderers) takes it.

What a GS dump at the binocucom shows:[^hud-dump]

- The 2D comes by all three paths: the portrait frames by VIF1's DIRECT
  (path 2), captions, the portraits' static, the binocucom's filter and
  outline by the GIF's DMA (path 3), the 3D world by VU1 (path 1).
- Everything flat is drawn with ZTST ALWAYS (or no depth test); the world
  is depth-tested, except three huge sky pieces.
- The portraits are 3D models, depth-tested, drawn in sub-views: the
  scissor is set to each portrait's box (x 1-160 and 479-638), and the
  text box has its own (180-459). The world and the binocucom's view use
  the full width.

So, in a frame the display shows (its FBP among DISPFB's, which the
machine reports at each V-blank), these narrow to 3/4 of their width:
each GIF tag drawn without a depth test whose vertices do not reach both
edges, and everything drawn under a scissor narrower than the frame, the
scissor included. Where they keep their place follows the user's choice
(2026-11-05):[^anchors] a sub-view in the frame's left or right third keeps
its distance from that edge, as in 4:3, so the portraits sit at the screen's
edges; a flat tag centred in such a box goes with it (the portraits' static
and outlines are drawn without their scissor); everything else narrows
towards the centre (the text box, captions, the binocucom's indicators). Full-width 2D (screen passes, fades, the
binocucom's outline and tick band) stays across 16:9, and so does the
depth-tested 3D world; frames not shown (textures drawn into) are left
alone.

- Pictures: captions, dialogue text, portraits (models, frames, static),
  the binocucom's side indicators and the credits keep their 4:3
  proportions; the portraits sit at the screen's edges, the rest in the
  central 4:3 area; the binocucom's frame spans 16:9.
- The machine's state is not touched (the widescreen replay's hash is the
  same before and after), so `lockstep` stays equal (1,100 fields) and
  widescreen off leaves everything as it was. GS dumps written with it on
  hold the narrowed packets.
- Not covered: 2D drawn depth-tested in the whole frame, or a flat element
  meant to sit at a 16:9 edge; none seen so far.

[^disasm]: Sly 1's code, disassembled
[^decomp]: The Sly 1 decompilation (CC0; function names from the May 2002 prototype's symbols)
[^pnach]: pcsx2_patches: the community widescreen patch for Sly 1 and 2 (lui 0x3FA6 to 0x3FDD)
[^runs]: The checks
[^hud-dump]: How Sly 1's 2D reaches the GS
[^anchors]: The user's choice: the binocucom's frame stays stretched; the portraits sit at the screen's edges as in 4:3
