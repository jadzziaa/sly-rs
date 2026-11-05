---
type: Proposal
title: Sound and movies (phase 8)
description: The design for phase 8, as the user decided it - Rust rewrites by default (a Rust 989snd driving an SPU2 voice engine, movies decoded in Rust in place of libmpeg's decoding), and the hardware emulated under Sony's own code (the disc's 989snd on the IOP, an emulated IPU) behind a classic setting - with what Sly 1 asks of the SPU2 today, how both stay exact, the milestones and their checks.
tags: [proposal, spu2, sound, ipu, mpeg, movies, 989snd, determinism, phase-8]
status: draft
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-05T07:37:33Z }
stale_after: 2027-03-01T00:00:00Z
sources:
  - id: plan
    resource: plan.md (phase 8)
    title: 'sly-rs: plan, phase 8 - Sound and movies'
  - id: regs
    resource: generated/sly1 replay games/sly1/replays/new-game.toml with SLY_RS_LOG=iop::hw=trace, SPU2 register accesses tallied (2,600 fields), 2026-11-04
    title: What 989snd asks of the SPU2 in Sly 1
  - id: symbols
    resource: games/sly1/symbols.tsv (Sly 1 decompilation's symbols, CC0)
    title: Sly 1's sound and movie functions by name
  - id: rac-rs
    resource: rac-rs.md
    title: How rac-rs is built (a Rust port of 989snd emitting SPU2 voices; movies by a pure-Rust MPEG-2 decoder; GPL-3.0)
  - id: landscape
    resource: landscape.md
    title: The Sly and PS2 recompilation landscape (PSS movies, Rust building blocks)
  - id: user
    resource: human:august, 2026-11-04
    title: The user's go-ahead to start the next plan step while away
  - id: decisions
    resource: human:august, 2026-11-05
    title: 'The user''s decisions: emulation behind a classic setting, not the default; otherwise the Rust rewrites; cpal; silence on a slow host'
  - id: pcsx2-sound
    resource: tools/pcsx2-frames.sh with AUDIO (PCSX2's audio capture) against sly-rs replay --wav, the save dialog (blank card, Left at 300, Cross at 340) and the opening scene (card with a save), compared with numpy, 2026-11-05
    title: sly-rs's sound against PCSX2's
  - id: movies
    resource: sly-rs replay with card = card3.ps2 (a save present) and movies = true, --frames and --wav, against tools/pcsx2-frames.sh (frames 550-1150, AUDIO) with the same card; a 3-field GS dump of a movie field drawn by PCSX2; 2026-11-05
    title: Sly 1's logo movies against PCSX2
  - id: ipu-only
    resource: human:august, 2026-11-05
    title: 'The user''s decision: movies through the IPU; a Rust player after parity'
  - id: mods
    resource: https://slymods.info/wiki/Mod:HQ_Cutscenes and https://gbatemp.net/threads/sly-cooper-and-the-thievius-raccoonus-usa-uhd-remaster-definitive.638803/ (search results; the GBAtemp page itself refused the fetch), 2026-11-05
    title: Movie and texture mods for Sly found on the web
  - id: listened
    resource: human:august, 2026-11-05
    title: The user listened to the new-game replay's sound
  - id: pcsx2-spu2
    resource: https://github.com/PCSX2/pcsx2 (pcsx2/SPU2), GPL-3.0-or-later, read from a sparse clone, 2026-11-05
    title: PCSX2's SPU2
---

# Sound and movies (phase 8)

Phase 8's gate: every movie plays in sync, and sound effects and music
match PCSX2 recordings of the same replay by ear and by waveform.[^plan]
With phase 7's remaining work waiting on the user's playthrough, this is
the next step.[^user]

**Decided (2026-11-05):**[^decisions] the Rust rewrites are the default, a
Rust 989snd for sound and a Rust MPEG-2 decoder in place of libmpeg's
decoding for movies; the emulation of the hardware under Sony's own code
(the disc's 989snd on the emulated IOP over the SPU2, an emulated IPU
under libmpeg) is a **classic** setting, not the default. `cpal` carries
the sound out, and a slow host gets silence, not stretched sound. The
sections below keep the comparison that led there.

## Where things stand

- **Sound.** Sony's `libsd` and `989snd` run on the emulated IOP. The SPU2
  under them is silent (`ps2-iop`'s `spu2.rs`): its registers read back
  what was written, DMA fills its 2 MB of sound memory, STATX reports DMA
  done. Banks load; nothing is heard.
- **What 989snd does with it** (the new-game replay, 2,600 fields):[^regs]
  about 390 key-ons and as many key-offs; pitch, volume, ADSR and start
  addresses written per voice; the voices' envelopes (`ENVX`) **read 4,233
  times**, their next addresses 36: the driver watches its voices to know
  when they finish. Answered by the silent model with 0, every voice looks
  finished at once. No IRQ address is set in that stretch.
- **Movies** are skipped: three hooks around `CMpeg::Execute` replace the
  decoding; the game's bookkeeping around a movie runs.
- **Sound in movies and voice lines** goes through 989snd too
  (`snd_InitMovieSound`, `snd_StartMovieSound`; `snd_PlayVAGStreamByLoc`
  for streamed VAG).[^symbols]

## Design

Both sound paths drive one SPU2 voice engine, `ps2-spu2`, ported from
PCSX2's (`pcsx2/SPU2`, GPL-3.0-or-later like sly-rs),[^pcsx2-spu2] so that
the output matches PCSX2's by construction: in classic, the disc's 989snd
writes its registers; by default, the Rust 989snd does. A replay records
which (`classic = true`), since the two leave different machine states.

### Classic: the SPU2 under the real 989snd

Emulate the SPU2 as hardware, under the 989snd the disc carries, rather
than replacing 989snd as rac-rs does.[^rac-rs] The driver's sequencing,
voice allocation and streaming stay Sony's; the work is a known chip,
shared by all three games (Sly 2 and 3 use a build of the same driver).

- **Voices**: 24 per core, two cores. SPU-ADPCM decoding (16-byte blocks of
  28 samples, the loop flags), the pitch counter with the hardware's
  4-tap Gaussian interpolation, ADSR envelopes, noise, pitch modulation,
  key on and off, and what the IOP reads back: `ENVX`, `ENDX`, the next
  address.
- **Mixing**: the voice mix to the dry and reverb paths (`VMIX`), the
  reverb with its work area in sound memory, core 0's output into core 1,
  master volumes.
- **Input**: AutoDMA (`ADMAS`), the path movie and stream sound can take
  into a core.
- **The IRQ address**: a match raises the IOP's SPU2 interrupt.

**Timing, and why it stays exact.** Everything the IOP can read is computed
on emulated time: one sample every 768 IOP cycles (36.864 MHz / 48 kHz).
The SPU2 catches up to the IOP's clock whenever the IOP touches it and at
each V-blank, so a register read sees exactly the state that time
implies. Replays stay exact; their hashes change once (989snd's voice
bookkeeping follows the envelopes it now sees), and the user's recordings
need their hashes recorded again. The host never feeds back: samples
leave through a buffer.

**Where.** A `ps2-spu2` crate (the plan's), with no host dependencies; the
IOP's hardware model calls it. About 2.3 million voice-samples per
emulated second, on the IOP side of the machine thread; measured once
built, with a thread of its own as the fallback (samples could be made
there from the register writes, but reads would then wait on it).

### Output to the host

`play` sends the samples to the sound card through `cpal` (as rac-rs
does). The game is paced to 59.94 fields a second and the sound card has
its own clock, so a resampling buffer sits between them: its rate is
nudged (within ±0.5%) to keep the buffer half full. When the machine falls
behind real time (weak hosts, slow stretches), the buffer runs dry and the
gap is silence rather than slowed sound. Headless runs make the same
samples and can write them to a WAV file (`replay --wav`), which is how
the gate's waveform comparison with PCSX2 is made.

### Default: a Rust 989snd

A port of Sly 1's own driver (989snd, built 21 August 2002, with MIDI),
answering the EE's `snd_*` calls at the RPC level and driving
`ps2-spu2`'s voices directly, with the IOP running no sound code. rac-rs's
exact port of R&C 1's build is the guide (its handlers, grains, banks and
pitch tables; GPL-3.0), but not a drop-in: Sly 1's build differs, and
rac-rs leaves out what Sly needs, streamed VAG (voice lines), movie sound
and MIDI sequences.[^rac-rs] Until it exists, the classic path is the only
one that sounds.

### Movies

Two ways to play Sly's PSS movies (the first is classic, the second the
default):

| | IPU emulation | Hook libmpeg, decode in Rust |
| --- | --- | --- |
| What runs | the game's `libmpeg` (demultiplexing, motion compensation on the EE, recompiled) and an emulated IPU: bitstream and VLC decoding, inverse quantisation, IDCT, colour conversion, its two DMA channels | a full MPEG-2 decoder (rac-rs's is GPL-3.0 and could be reused)[^rac-rs] in place of `sceMpegGetPicture`, writing the pictures where the IPU would |
| Faithful | as PCSX2; the same for all three games | depends on matching libmpeg's output format and timing |
| Size | the IPU only (no motion compensation, no picture management) | a whole decoder |
| Audio | untouched: the game demultiplexes and sends it to 989snd | untouched as well, if the demultiplexer still runs |

The three movie hooks go once either plays movies; skipping movies stays
an option.

## Progress

- **M1, 2026-11-05: the SPU2 sounds.** `ps2-spu2` ports PCSX2's SPU2 in
  PS2 mode (voices, envelopes, volume slides, noise, pitch modulation, the
  mix, reverb with its resampling filters, the IRQ address, plain and
  AutoDMA transfers), 13 unit tests. The IOP's hardware model hands it
  every SPU2 register access and DMA transfer and catches it up to the
  IOP's clock (and to the machine's at each V-blank), so the disc's
  989snd now drives a real chip. Over the new-game replay the sound is
  silent through the boot, then the attract and intro play (peaks up to
  17,459 of 32,767); the replay reaches its three places, hashes recorded
  again. `replay --wav FILE` writes the sound.
- **M2, 2026-11-05: `play` sounds**, through `cpal` and the resampling
  buffer (checked headless at 1% volume: the stream opens, at 44.1 kHz
  here, and the game runs; not yet listened to). `volume` under `[play]`,
  0 for none.
- Until the Rust 989snd exists, the disc's driver is the only one, so
  there is no classic setting yet: it comes with the choice.
- **Against PCSX2, 2026-11-05.**[^pcsx2-sound] PCSX2's release builds
  record sound only through their video capture; `tools/pcsx2-frames.sh`
  now takes `AUDIO=FILE` (audio only, WAV; a `capture` press starts and
  stops it) and runs PCSX2 with its sound output off.
  - **An isolated sound is the same, sample for sample.** The cursor
    sound of the "create a save file?" dialog (Left at field 300, with a
    blank card in both): correlation 0.995 as made; with PCSX2's
    DC-blocking output filter (`spu2.cpp`'s `DCFilter`, applied to
    sly-rs's samples) **correlation 1.000000, every sample within 1** (its
    float conversion).
  - **The opening scene** (PCSX2 frames 1300-2200, the same stretch of
    ours, PCSX2's DC filter applied to ours): everything lines up at one
    offset, constant to 0.1 ms over 13 seconds, so the music runs at
    PCSX2's tempo; per second the correlation is 0.33 to 0.96, and almost
    no samples are identical, even where music dominates. *Interpretation:*
    989snd keys its voices on the IOP's timer ticks, and sly-rs's IOP
    (high-level, not cycle-timed) places those ticks a few samples away
    from PCSX2's, so each sound starts a few samples earlier or later; the
    sound effects also follow the animation, a few frames apart between
    the two runs. The isolated sound above shows the chip itself is exact.
  - **Streams reach the chip**: from field 800 of the new-game replay the
    SPU2 gets a 4,096-byte transfer about every ten fields (the attract's
    music, then the binocucom's voice), by plain DMA; the binocucom's line
    at field 2600 is the same as before the SPU2.
  - `play` applies the same DC filter; `replay --wav` writes the chip's
    own output.
  - **By ear** (the user, 2026-11-05):[^listened] the new-game replay's
    sound (`replay --wav`, 43 seconds: the attract, the title, the
    binocucom dialogue) sounds correct, with no distortion or dialogue off
    its timing.
- **Cost**: on the user's prologue recording (PGO, GPU drawing) the IOP,
  with the SPU2 in it, takes 7.0% of the machine thread's host time,
  against about 1% before: the chip costs about 6%.
- **Recordings made before the SPU2 drift**: with voices that now play
  out, the game's timing changes where it waits on sound, so a recording
  made against the silent chip loses step. The user's prologue recording
  still plays (the binocucom, the titles, the level) but ends in the
  first level instead of the next, its input out of step; it needs
  recording again. The new-game replay reaches its places; its hashes are
  recorded again. No SPU2 interrupt is raised over the prologue: Sly's
  voice lines do not wait on one.
- **M4, 2026-11-05: movies play, through the emulated IPU.** `ps2-ipu`
  ports PCSX2's IPU (its command engine, the MPEG-2 tables, IDCT,
  colour conversion and dithering; 28 unit tests). The EE's hardware
  model gives it its registers, its FIFOs and its DMA channels (toIPU
  fed as the IPU asks, fromIPU drained as it produces, as PCSX2
  interleaves them), and its interrupt (INTC 8). With the movie hooks
  taken away (`run --movies`, `movies = true` in a replay, `movies` under
  `[play]`, true by default), Sly's own player and Sony's libmpeg decode
  the Sony and Sucker Punch logo movies at boot.[^movies]
  - **Pictures**: the movie runs in step with PCSX2's (every one of our
    fields has its match 222 frames later in PCSX2); 96 to 98% of pixels
    identical, all within 8 levels (the largest 7 on a run of 30 fields).
    PCSX2 drawing our GS traffic for movie fields gives our pictures (two
    fields identical, a third within 4), and the differences in the real
    game sit on the picture's edge rows and on a few rows in every 16:
    *interpretation*, the filtered sprite that puts the decoded picture on
    screen, not the decoding.
  - **Sound**: 989snd plays movie sound through a core's auto-DMA input.
    sly-rs first completed those transfers at once (70,491 of them in 420
    fields); now an auto-DMA transfer feeds the core a kilobyte at a time,
    read from IOP memory when the core asks for it, and completes when it
    asks after the last: with PCSX2's DC filter on ours, the movie's sound
    is **the same as PCSX2's, every sample within 1, for its 8 seconds**.
    Reading the whole block at the start had got about 900 samples in
    every 3,072 wrong: the player refills that IOP memory just behind the
    sound being played.
- **The Rust decoder (the default the user chose) does not fit Sly's
  player as hoped.** libmpeg is not a decoder the game calls: it drives
  the game's own callbacks from inside (`FMpegDecodeVideo` feeds the IPU
  through its DMA channel, `CbDemuxed` pulls the stream from the disc,
  `CMpegAudio` sends the sound to the IOP). Replacing `sceMpegGetPicture`
  stops those callbacks, and with them the stream; a Rust path would have
  to replace Sly's whole movie player natively (its disc streaming, its
  sound queue, its GS packets). The IPU path is what plays movies now.
- `lockstep` (recompiled against interpreted, now with a replay's card and
  movies) is equal at every field through the logo movies (900 fields).
- **Decided (2026-11-05):**[^ipu-only] movies go through the emulated IPU
  only for now; a Rust player waits until after parity.
- **Movie mods.** The user knows of mods that bring the PS3 release's
  better movies to the game. Found so far:[^mods] SlyMods' "HQ Cutscenes"
  is for the PS Vita's Sly Trilogy (rePatch, the Vita's own video files),
  not the PS2 disc; GBAtemp's "UHD Remaster Definitive" for Sly 1 is a
  PCSX2 texture pack, not movies. What a PS2 movie mod could do with
  sly-rs: the disc is identified by its executable alone (its SHA-1), so
  an image whose movie data is replaced but whose executable is the
  original boots as is; but Sly's player decodes into a buffer of exactly
  1,200 macroblocks (`sceMpegGetPicture(..., 1200)`, 0x12C000 bytes), so
  movies larger than 640×480 need a changed executable, which sly-rs
  refuses as an unknown build.
- Not checked yet: audio-to-picture sync measured within one run (against
  PCSX2 the sound lines up 246 ms from the pictures, most likely when
  PCSX2's capture started), and the movies later in the game.

## Milestones and checks

1. **M1 The SPU2**: `ps2-spu2` (voices, envelopes, mixing, reverb, the IRQ
   address, AutoDMA), wired under the classic path; `replay --wav`.
   Checks: unit tests; the new-game replay's sound against a PCSX2
   recording of the same presses, by ear and by waveform.
2. **M2 Play**: `cpal` output with the resampling buffer; the classic
   setting in `[play]`, `play --classic`, and in replays.
3. **M3 The Rust 989snd**: banks, sounds and their grains, voice
   allocation, MIDI, streams, movie sound, against the classic path's
   register writes and output.
4. **M4 Movies**: the Rust decoder (default), then the IPU (classic); the
   hooks removed. Check: every movie against PCSX2's frames and sound, in
   sync.

Each milestone keeps every replay exact and `lockstep` equal; hashes are
recorded again where the machine's state changes.

[^plan]: 'sly-rs: plan, phase 8 - Sound and movies'
[^regs]: What 989snd asks of the SPU2 in Sly 1
[^symbols]: Sly 1's sound and movie functions by name
[^rac-rs]: How rac-rs is built (a Rust port of 989snd emitting SPU2 voices; movies by a pure-Rust MPEG-2 decoder; GPL-3.0)
[^user]: The user's go-ahead to start the next plan step while away
[^decisions]: The user's decisions: emulation behind a classic setting, not the default; otherwise the Rust rewrites; cpal; silence on a slow host
[^pcsx2-spu2]: PCSX2's SPU2
[^pcsx2-sound]: sly-rs's sound against PCSX2's
[^listened]: The user listened to the new-game replay's sound
[^ipu-only]: The user's decision: movies through the IPU; a Rust player after parity
[^mods]: Movie and texture mods for Sly found on the web
[^movies]: Sly 1's logo movies against PCSX2
