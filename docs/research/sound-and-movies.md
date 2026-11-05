---
type: Proposal
title: Sound and movies (phase 8)
description: The design for phase 8 - an SPU2 under Sony's own 989snd, run on the IOP's clock so that everything the game reads stays exact, its output carried to the host through a resampling buffer, and movies decoded by an emulated IPU under the game's own libmpeg - with what Sly 1 asks of the SPU2 today, the milestones and their checks, and the four decisions it needs.
tags: [proposal, spu2, sound, ipu, mpeg, movies, 989snd, determinism, phase-8]
status: draft
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-05T05:20:00Z }
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
---

# Sound and movies (phase 8)

Phase 8's gate: every movie plays in sync, and sound effects and music
match PCSX2 recordings of the same replay by ear and by waveform.[^plan]
With phase 7's remaining work waiting on the user's playthrough, this is
the next step;[^user] it needs decisions before code, so this is a
proposal.

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

### An SPU2 under the real 989snd

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

### Movies: an emulated IPU under the game's libmpeg

Two ways to play Sly's PSS movies:

| | IPU emulation | Hook libmpeg, decode in Rust |
| --- | --- | --- |
| What runs | the game's `libmpeg` (demultiplexing, motion compensation on the EE, recompiled) and an emulated IPU: bitstream and VLC decoding, inverse quantisation, IDCT, colour conversion, its two DMA channels | a full MPEG-2 decoder (rac-rs's is GPL-3.0 and could be reused)[^rac-rs] in place of `sceMpegGetPicture`, writing the pictures where the IPU would |
| Faithful | as PCSX2; the same for all three games | depends on matching libmpeg's output format and timing |
| Size | the IPU only (no motion compensation, no picture management) | a whole decoder |
| Audio | untouched: the game demultiplexes and sends it to 989snd | untouched as well, if the demultiplexer still runs |

**Proposed: the IPU.** The three movie hooks then go, behind an option
that keeps skipping movies.

## Milestones and checks

1. **M1 Voices**: ADPCM, pitch and Gaussian interpolation, ADSR, key on and
   off, `ENVX`/`ENDX`/next address; `replay --wav`. Checks: unit tests
   against the published tables and known VAG data; the new-game replay's
   sound effects against a PCSX2 recording of the same presses, by ear and
   by waveform of isolated effects.
2. **M2 Play**: `cpal` output with the resampling buffer; volume in
   `[play]`.
3. **M3 The rest of the chip**: reverb, noise, pitch modulation, the IRQ
   address. Check: music against PCSX2 recordings.
4. **M4 AutoDMA input**: streamed voice lines and movie sound.
5. **M5 Movies**: the IPU; the hooks removed. Check: every movie against
   PCSX2's frames and sound, in sync.

Each milestone keeps every replay exact (hashes re-recorded once at M1) and
`lockstep` equal.

## Decisions needed

1. **The SPU2 under the real 989snd** (proposed), or a Rust 989snd as rac-rs
   has.
2. **Movies through an emulated IPU** (proposed), or hooking libmpeg with a
   Rust MPEG-2 decoder.
3. **`cpal`** as a new dependency for sound output (mainstream; `cargo deny`
   would check it).
4. **On a slow host**: silence when the buffer runs dry (proposed), or
   stretching the sound.

[^plan]: 'sly-rs: plan, phase 8 - Sound and movies'
[^regs]: What 989snd asks of the SPU2 in Sly 1
[^symbols]: Sly 1's sound and movie functions by name
[^rac-rs]: How rac-rs is built (a Rust port of 989snd emitting SPU2 voices; movies by a pure-Rust MPEG-2 decoder; GPL-3.0)
[^user]: The user's go-ahead to start the next plan step while away
