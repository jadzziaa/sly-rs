---
type: Finding
title: Widescreen in Sly 1
description: 'How sly-rs makes Sly 1 draw at 16:9 behind an option - the camera''s aspect ratio set in one place, changed by an instruction patch the interpreter runs while memory keeps the game''s bytes - and the checks: off leaves replays exact, on stays equal in lockstep and widens the 3D view while the 2D interface stretches.'
tags: [widescreen, enhancement, patch, camera, phase-9]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-05T07:56:03Z }
sources:
  - id: disasm
    resource: sly-rs disasm sly1 over the whole .text section (SCUS-97198), 2026-11-05
    title: Sly 1's code, disassembled
  - id: decomp
    resource: https://github.com/TheOnlyZac/sly1
    title: The Sly 1 decompilation (CC0; function names from the May 2002 prototype's symbols)
  - id: pnach
    resource: https://github.com/PCSX2/pcsx2_patches
    title: "pcsx2_patches: the community widescreen patch for Sly 1 and 2 (lui 0x3FA6 to 0x3FDD)"
  - id: runs
    resource: sly-rs replay and lockstep on games/sly1/replays/new-game.toml and a copy with widescreen = true, 2026-11-05
    title: The checks
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
- The 2D interface (captions, the binocucom's frame, the portraits) is
  drawn for 4:3 and stretches, as with the PCSX2 patch. Correcting it would
  need the HUD's drawing code changed; not done.
- `lockstep` with widescreen on (the recompiled build and the interpreter
  side by side, through the title's 3D scene): 1,100 fields equal.

[^disasm]: Sly 1's code, disassembled
[^decomp]: The Sly 1 decompilation (CC0; function names from the May 2002 prototype's symbols)
[^pnach]: pcsx2_patches: the community widescreen patch for Sly 1 and 2 (lui 0x3FA6 to 0x3FDD)
[^runs]: The checks
