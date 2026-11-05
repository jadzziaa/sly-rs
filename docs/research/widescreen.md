---
type: Finding
title: Widescreen in Sly 1
description: 'How sly-rs makes Sly 1 draw at 16:9 behind an option - the camera''s aspect ratio set in one place, changed by an instruction patch the interpreter runs while memory keeps the game''s bytes - and the checks: off leaves replays exact, on stays equal in lockstep, widens the 3D view, and keeps the 2D interface at 4:3 by narrowing its GIF packets.'
tags: [widescreen, enhancement, patch, camera, phase-9]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-05T23:11:35Z }
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
  - id: logo-dump
    resource: a GS dump of the user's widescreen recording game1.toml at fields 19,619-19,620 (the fade to Sly's logo), its flat tags and their vertices; pictures every 60 fields of the whole recording before and after the change, 2026-11-05
    title: The fade to the logo
  - id: fade-trace
    resource: traces of every full-width flat tag (bounds, vertex count, primitive, vertices on the edges and corners) over prologue.toml and session.toml replayed in widescreen, the new-game replay in widescreen and the user's second game1.toml (2,982 fields); GS dumps at fields 1,101-1,104 and 965-966 of the latter; pictures every 2 fields of its fade with widescreen on and off, 2026-11-05
    title: The logo larger than the frame
  - id: anchors
    resource: human:august, 2026-11-05
    title: 'The user''s choices: the binocucom''s frame stays stretched; the portraits and captions sit as in 4:3 relative to the screen''s edges'
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

- each GIF tag sent by path 2 or 3 drawn without a depth test whose
  vertices do not reach both edges (VU1's flat output is projected 3D,
  such as the light cones around a lamp, which reach far off screen; the
  wider camera already widens them);
- everything drawn under a scissor narrower than the frame, the scissor
  included, whatever its path or depth test.

Where they keep their place follows the user's choices
(2026-11-05):[^anchors] what lies wholly in the frame's left or right third
keeps its distance from that edge, as in 4:3; anything else narrows towards
the centre. A sub-view is placed by its scissor (the portraits at the
screen's edges, the text box centred). A transfer's flat tags are placed in
blocks: tags that overlap across and are at most a line apart down share the
anchor of their joint bounds, so a caption's lines stay together ("Paris,
France / 4:20 am" at the left edge, "Starring / Sly Cooper" at the right,
"Sony Computer Entertainment America / Presents" centred; placed line by
line, the short "Presents" broke away to the left). Full-width 2D (screen passes, fades, the
binocucom's outline and tick band) stays across 16:9, and so does the
depth-tested 3D world; frames not shown (textures drawn into) are left
alone.

One kind of full-width 2D has to follow what it surrounds: the fade at the
end of the prologue, as Sly jumps into the van, closes on his logo. It is
one triangle strip over the whole frame with a rectangular hole cut to the
logo's box, followed in the same transfer by the logo itself (a quad and
its outline's triangles).[^logo-dump] The logo narrowed and the hole did
not, so two bars of the scene showed beside it (9 pixels each at field
19,620 of the user's recording; never on hardware). So when a full-width
tag has vertices on both the left and the right side of a narrowed tag's
box (within a pixel, within its rows, away from the frame's edges), those
vertices move with it; its others stay, so it still spans the frame.

- Matched against each tag's own box, not its block's: the logo's block
  also takes in a piece to its right, and matching the block left the
  right-hand bar.
- Both sides, because the binocucom's lower fill has a corner on each
  portrait frame's inner corner (x 159 and 479, y 157): matched on one
  side, it moved and poked a dark wedge out above the left portrait.
- Over the whole recording (a picture every 60 fields, before and after),
  only the fade's pictures changed. Pictures after the save notice
  (field 26,220 on) differ by one level in 8 to 20 pixels between runs of
  the same build, with or without the change; the machine's hash is the
  same.

The same fade also opens from and closes on the logo while the logo is
larger than the frame (the title after a save is loaded, the start and end
of the prologue). The logo is five triangle tags (186, 66, 66, 159 and 141
vertices), the 141's box being the fade's hole; they reach past the
frame's edges one by one, and each that did counted as full-width 2D and
stopped narrowing. So for a few fields the logo was drawn at two scales,
then jumped to 16:9 (fields 1,103-1,104 of the user's second recording,
the "brief distortion").[^fade-trace] Now, when a fill whose corners are
the frame's has a hole that a full-width tag fills across, that tag and
the others lying within the hole narrow towards the centre, and the hole
with them; so the logo keeps its 4:3 shape at every size.

What full-width 2D comes through, over the prologue, the user's other
session and both recordings in widescreen:[^fade-trace]

| Tags | Vertices on both edges, exactly | Corners |
| --- | --- | --- |
| Screen tiles (40 sprites), fades (2 or 4), the binocucom's frame (200, strip), the fade with its hole (10, strip) | yes, always | the fades and tiles; not the binocucom's frame |
| The binocucom's tick band (384 sprites, -227 to 867 px) | never | no |
| The logo's tags, grown past the frame | never | no |

- How far a tag reaches past the edges does not tell the logo from the
  tick band (the logo reached -263 px), and the logo's vertices fall
  within half a pixel of both edges about one time in fifteen; exactly on
  them, never. So a tag counts as framed only inside a fill's hole, never
  one with vertices exactly on both edges, and the tick band, in no hole,
  stays.
- Sly 1 draws every other field half a line up: the fade's corners are at
  y -0.5 and 223.5 there. The corner test allows a pixel up and down; at
  first it did not, so the logo narrowed in one field and not the next,
  and the display's blend of the two showed a ghost of it.
- A hole that is the whole frame (a tiled fill's inner vertices) is
  skipped.
- A picture every 2 to 30 fields over the four replays, before and after:
  only fades changed, and the binocucom's stretches did not. The dark
  speckle a contact sheet shows at field 1,092 of the same recording shows
  with widescreen off too (and not at full size): it is not the widescreen
  filter's.

- Pictures: captions, dialogue text, portraits (models, frames, static),
  the binocucom's side indicators and the credits keep their 4:3
  proportions; the portraits and captions sit where they sit in 4:3
  relative to the screen's edges; the binocucom's frame spans 16:9.
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
[^logo-dump]: The fade to the logo
[^fade-trace]: The logo larger than the frame
[^anchors]: The user's choices: the binocucom's frame stays stretched; the portraits and captions sit as in 4:3 relative to the screen's edges
