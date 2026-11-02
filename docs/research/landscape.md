---
type: Research Note
title: "The Sly and PS2 recompilation landscape"
description: Prior art for porting the Sly trilogy (decompilations, a hand-written Sly 1 port, an existing C++ static recompilation of Sly 2), what is known about the Sly engine (disc layout, compression, scripting, rendering, audio, anticrack), PCSX2's quirks for these games, the PS2 recompilation ecosystem and Rust building blocks, and hardware references - as of 2026-11-02.
tags: [research, prior-art, sly, decompilation, pcsx2, ecosystem, rust-crates]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-02T21:05:00Z }
stale_after: 2027-01-01T00:00:00Z
sources:
  - id: sly1
    resource: https://github.com/TheOnlyZac/sly1
    title: "TheOnlyZac/sly1: matching decompilation of Sly 1 (CC0-1.0)"
  - id: decomp-dev
    resource: https://decomp.dev/TheOnlyZac/sly1.json
    title: "decomp.dev progress for TheOnlyZac/sly1 (2026-10-26)"
  - id: openthievius
    resource: https://github.com/OpenThievius/sly1-decomp
    title: "OpenThievius/sly1-decomp: clean-room decompilation of Sly 1 (AGPL-3.0), with DISC.md and TOOLCHAIN.md"
  - id: projectcane
    resource: https://github.com/theclub654/ProjectCane
    title: "theclub654/ProjectCane: a playable hand-written C++/OpenGL PC port of Sly 1 (no licence file)"
  - id: sly2-recomp
    resource: https://github.com/menaman123/Sly-2-Recomp
    title: "menaman123/Sly-2-Recomp: static recompilation of Sly 2 as a NativeEE title module (Apache-2.0)"
  - id: nativeee
    resource: https://github.com/menaman123/NativeEE
    title: "menaman123/NativeEE: title-neutral PS2 static-recompilation engine in C++ (MIT)"
  - id: komasia
    resource: https://github.com/Komasia/sly2-decomp
    title: "Komasia/sly2-decomp: claims all 6,662 Sly 2 functions matched (unverified, no licence file)"
  - id: rogue-byte
    resource: https://github.com/Blightwidow/rogue-byte
    title: "Blightwidow/rogue-byte: Sly 2 decompilation (46.76% on ByteMatch)"
  - id: bevy
    resource: https://github.com/Campfirecrucifix/sly2-bevy-of-thieves
    title: "Campfirecrucifix/sly2-bevy-of-thieves: abandoned Rust/Bevy hand-port of Sly 2 (MIT, archived) and its post-mortem"
  - id: slymods
    resource: https://slymods.info
    title: "SlyMods wiki: builds, formats (WAL, WAD, FK), Splice, anticrack, widescreen, 60 FPS, Ghidra"
  - id: slymods-anticrack
    resource: https://slymods.info/wiki/Sly_1:Anticrack
    title: "SlyMods: Sly 1 anticrack"
  - id: hiddenpalace
    resource: https://hiddenpalace.org/Sly_Cooper_and_the_Thievius_Raccoonus_(May_21,_2002_prototype)
    title: "Hidden Palace: Sly 1 May 21 2002 prototype (full debugging symbols)"
  - id: gameindex
    resource: https://github.com/PCSX2/pcsx2/blob/master/bin/resources/GameIndex.yaml
    title: "PCSX2 GameIndex.yaml (and pcsx2/Docs/GameIndex.md for the meanings)"
  - id: pcsx2-7241
    resource: https://github.com/PCSX2/pcsx2/issues/7241
    title: "PCSX2 issue 7241: Sly 3 VU0 clamping (\"Real hardware doesn't clamp\")"
  - id: pnach
    resource: https://github.com/PCSX2/pcsx2_patches/tree/main/patches
    title: "pcsx2_patches: widescreen, no-interlacing, no-motion-blur patches"
  - id: ps2recomp
    resource: https://github.com/ran-j/PS2Recomp
    title: "ran-j/PS2Recomp: R5900 to C++ static recompiler (GPL-3.0)"
  - id: n64recomp
    resource: https://github.com/N64Recomp/N64Recomp
    title: "N64Recomp"
  - id: xenonrecomp
    resource: https://github.com/hedge-dev/XenonRecomp
    title: "XenonRecomp"
  - id: opengoal
    resource: https://github.com/open-goal/jak-project
    title: "OpenGOAL (Jak and Daxter)"
  - id: ps2-float
    resource: https://github.com/FIZL0/ps2-float
    title: "FIZL0/ps2-float: EE FPU and VU0 arithmetic in Rust, bit-exact to PCSX2 (MIT or Apache-2.0)"
  - id: ps2-oracle
    resource: https://github.com/FIZL0/ps2-oracle
    title: "FIZL0/ps2-oracle: R5900 interpreter in Rust for diffing a port's functions against the original (MIT or Apache-2.0)"
  - id: rabbitizer
    resource: https://github.com/Decompollaborate/rabbitizer
    title: "rabbitizer: MIPS disassembler with R5900 support, Rust bindings"
  - id: ghidra-ee
    resource: https://github.com/chaoticgd/ghidra-emotionengine-reloaded
    title: "ghidra-emotionengine-reloaded"
  - id: ps2tek
    resource: https://psi-rockin.github.io/ps2tek/
    title: "ps2tek: PS2 hardware documentation"
  - id: disc-survey
    resource: disc-survey.md
    title: "What the three discs contain: a survey"
---

# The Sly and PS2 recompilation landscape

A snapshot of 2026-11-02. Most of the Sly projects below are weeks old and
moving fast; re-check before relying on a number. Repository existence,
licences and the Sly 1 decompilation's target hash were checked against
GitHub on that date; the rest is as the sources state it.

## 1. Prior art for each game

| | Sly 1 | Sly 2 | Sly 3 |
| --- | --- | --- | --- |
| Matching decompilation | TheOnlyZac/sly1: 9.26% of code, 1,385 of 4,690 functions; **CC0**; targets exactly our ELF[^sly1][^decomp-dev] | Komasia: claims 6,662/6,662 (unverified)[^komasia]; rogue-byte 46.76%[^rogue-byte] | none |
| Clean-room decompilation | OpenThievius: 78.99% of code, AGPL-3.0[^openthievius] | - | - |
| Symbols | ~5,500 names from the May 2002 prototypes' debug symbols[^hiddenpalace] | none public | none public |
| Native port | **ProjectCane**: playable hand-written C++/OpenGL port, no licence file[^projectcane] | **Sly-2-Recomp**: C++ static recompilation on NativeEE, boots to gameplay with audio, below 60 fps; Apache-2.0 (engine MIT)[^sly2-recomp][^nativeee] | none |
| Abandoned | - | Rust/Bevy hand-port, "97.7% ported", never playable; its post-mortem recommends decomp or recompilation[^bevy] | - |

What follows for sly-rs:

- **Sly 1's function list and names are a solved input.** The CC0
  decompilation's `config/checksum.sha1` is `57dc305d…`, the user's
  `SCUS_971.98` ([disc survey §1](disc-survey.md)), and its
  `config/symbol_addrs.txt` (~300 KB) gives addresses and mangled C++ names
  that may be used without restriction.
- **Sly 2 already has a C++ static recompilation.** Its engine's README and
  `hle/*.h` notes are the richest public documentation of Sly 2's internals,
  under permissive licences. It recompiles the IOP modules (`lgAud`,
  `989snd`, `spiop`, `FILEIO`) instead of reimplementing them, compiles VU1
  microprograms ahead of time after verifying them against captured kicks,
  and still runs below 60 fps with VU1 as the main cost.
- **Sly 3 has nothing.** It is the game a trilogy project serves most.
- Code without a licence (ProjectCane, Komasia) can be read, not copied.
  AGPL-3.0 code (OpenThievius) is read for facts only.

## 2. The Sly engine

- **Lineage**: engines "P2" (Sly 1), "P3" (Sly 2), "P4" (Sly 3), descended
  from *Rocket: Robot on Wheels*.[^slymods]
- **Compiler**: SN Systems ProDG GCC 2.95.x for game code (the exact Sly 1
  build is disputed between projects); Sony's SDK libraries were built with
  Sony's older `2.9-ee-991111`, which tail-calls with `j` and saves registers
  with 8-byte `sd`, while game code uses 16-byte `sq` and rarely tail-calls.
  Function-boundary detection must handle both styles.[^openthievius]
- **Disc layout**: Sly 1's data is a hidden, encrypted WAL archive in
  unlisted sectors from 12,345, indexed by a WAC catalogue inside the
  executable. Sly 2 locates files through 64-byte XOR-obfuscated descriptors
  (`lsn = (w0 ^ w13) + 0xCA0`, `size = w1 ^ w15`); 0xCA0 = 3232 is the first
  data sector we measured. Sly 3 uses `SLY3.WAD`, reported encrypted by a
  low-confidence source.[^slymods][^bevy]
- **Level files** are typed by an "FK" enum and compressed with a custom LZ
  (8 KiB window) that Sly 2 decodes **in scratchpad memory at
  `0x70000000`**: the scratchpad must be modelled exactly.[^slymods][^bevy]
- **Scripting**: "Splice", a Lisp-like interpreter with a garbage collector,
  scripts stored in level files; Sly 2 adds `CSeq` cutscene bytecode.
- **Rendering**:
  - Sly 1 has exactly two VU1 microprograms, `VU1_GlobMpg` (0x3AF0 bytes,
    meshes) and `VU1_BlipMpg` (0x16E8 bytes, particles); it also uses VU0
    micro mode and scratchpad DMA.[^sly1]
  - Sly 2 loads VU1 programs as overlays.
  - Toon outlines are one-pixel GS `LINE` primitives with antialiasing
    (`AA1`), a colour pass plus a Z-only pass, on a 512×224 field buffer.
    They thin out at high resolution.
  - Sly 2 counts shadow volumes in the framebuffer's green byte with
    wrap-around blending (`COLCLAMP=0`, `FBMSK=ffff00ff`); ordinary GPU
    blending gets this wrong.[^sly2-recomp]
  - The full-screen "ghosting" motion blur is intentional.
- **Clock**: Sly 2 measures frame time from the COP0 `Count` register and
  clamps it to [1/60, 1/30] s: a variable timestep, so higher frame rates are
  feasible.[^sly2-recomp]
- **Audio and IOP**: 989snd on libsd in all three (our survey found the same
  nine embedded modules in Sly 3 as in Sly 2: [disc survey §5](disc-survey.md)).
  Sly 2 and 3 use the USB headset microphone in gameplay. `spiop`'s purpose is
  still unidentified.
- **FMV**: PSS = MPEG-2 program stream, video 640×480 (some 720×480), audio
  as SPU-ADPCM at 44.1 kHz in private stream 1 (`SShd`/`SSbd`), decoded with
  Sony's `libmpeg` and the IPU.
- **Threads**: Sly 1 saves to the memory card on its own EE thread with
  semaphores; Sly 2's save states serialise threads and kernel objects. No
  evidence of parallel work beyond that.
- **Anticrack (Sly 1)**: the game periodically checksums its own code at four
  places (`0x12B760`, `0x15EDB8`, `0x18F610`, `0x1E9B30`); a failed check
  silently sabotages play (failed climbing, vehicles at 40% speed, vanishing
  keys), some of it saved to the memory card.[^slymods-anticrack] A port must
  keep the original code bytes unmodified in guest RAM or neutralise all four
  checks. Unknown for Sly 2 and 3.

## 3. PCSX2 quirks

| Game | GameIndex settings[^gameindex] | What they point to |
| --- | --- | --- |
| Sly 1 | `halfPixelOffset: 5` ("post alignment"), `autoFlush: 1` ("shadow definition") | full-screen post passes and draws reading the frame being drawn |
| Sly 2 | `halfPixelOffset: 5` ("chromatic effect") | post-processing alignment |
| Sly 3 | `halfPixelOffset: 5`, `vuRoundMode: 0`, `vu0ClampMode: 3` | VU0 code sensitive to the PS2's float rules |

The Sly 3 VU settings are PCSX2 workarounds, not hardware behaviour ("Real
hardware doesn't clamp"[^pcsx2-7241]); bit-exact float emulation should make
them unnecessary. Community patches exist for widescreen (a `lui` immediate
`0x3FA6` → `0x3FDD` in Sly 1 and 2, `0x4420` → `0x4455` in Sly 3),
no-interlacing and no-motion-blur; there is no NTSC 60 fps patch.[^pnach][^slymods]

## 4. The PS2 recompilation ecosystem

- **PS2Recomp** (GPL-3.0): literal R5900 → C++ in the N64Recomp style, from a
  Ghidra function export and TOML configuration; runs original IOP modules;
  "Performance is very bad for VU and GS" by its own README; dozens of ports
  build on it.[^ps2recomp]
- **NativeEE** (MIT): title-neutral engine with recompiled IOP, VU1
  ahead-of-time compilation, save states, pad record/replay and frame-dump
  "judges".[^nativeee]
- Ideas commonly borrowed: N64Recomp's jump tables as `switch`, function
  lookup for indirect calls, overrides by link order, microcode
  recompilation[^n64recomp]; XenonRecomp's analysers emitting TOML and
  mid-function hooks[^xenonrecomp].
- **OpenGOAL** decompiles to source and hand-writes each VU1 renderer; it
  works because Jak is almost all GOAL. For C++ Sly the equivalents are
  ProjectCane and the matching decompilations.[^opengoal]

**Rust building blocks**

| Area | Candidates | Note |
| --- | --- | --- |
| R5900 decoding | `rabbitizer` (R5900 incl. MMI; no VU micro mode)[^rabbitizer] | the reference for phase 1 |
| PS2 float maths | **`ps2-float`** (git only, MIT/Apache-2.0): EE FPU, VU0, the SDK's fdlibm, bit-exact to PCSX2 recordings[^ps2-float] | an oracle for, or the basis of, our float semantics |
| Function-level oracle | **`ps2-oracle`** (MIT/Apache-2.0): R5900 interpreter that calls a game's original functions from the ISO[^ps2-oracle] | an independent check on our interpreter |
| ELF | `object`, `goblin` | |
| MPEG-2 | `oxideav-mpeg12video`, `na_mpeg2_decoder` (young) | candidates for `ps2-mpeg`; evaluate before writing our own |
| Memory card | `ps2-memcard` | |
| VIF | `vifterpreter` (1 star) | reference only |
| Missing | GS renderer, VU emulator, VAG/SPU-ADPCM decoder | ours to write; the VAG decoder is small |

## 5. Hardware references

ps2tek (incomplete, last updated 2022)[^ps2tek]; PCSX2's source (the ground
truth for behaviour); ps2sdk (IOP module interfaces); Play! (active, VU and
IOP HLE); DobieStation (readable, dormant since 2021);
ghidra-emotionengine-reloaded for Ghidra analysis of EE code.[^ghidra-ee]

## 6. Not confirmed

Sly 3's compiler, symbols and data layout; whether Sly 3's multiplayer is
split-screen; Komasia's 100% claim; the exact Sly 1 compiler build; what
`spiop` does; anticrack in Sly 2 and 3.

[^sly1]: TheOnlyZac/sly1.
[^decomp-dev]: decomp.dev, 2026-10-26.
[^openthievius]: OpenThievius/sly1-decomp, DISC.md and TOOLCHAIN.md.
[^projectcane]: theclub654/ProjectCane.
[^sly2-recomp]: menaman123/Sly-2-Recomp, README and engine notes.
[^nativeee]: menaman123/NativeEE.
[^komasia]: Komasia/sly2-decomp.
[^rogue-byte]: Blightwidow/rogue-byte.
[^bevy]: Campfirecrucifix/sly2-bevy-of-thieves.
[^slymods]: SlyMods wiki.
[^slymods-anticrack]: SlyMods: Sly 1 anticrack.
[^hiddenpalace]: Hidden Palace, Sly 1 May 21 2002 prototype.
[^gameindex]: PCSX2 GameIndex.yaml.
[^pcsx2-7241]: PCSX2 issue 7241.
[^pnach]: pcsx2_patches.
[^ps2recomp]: ran-j/PS2Recomp.
[^n64recomp]: N64Recomp.
[^xenonrecomp]: XenonRecomp.
[^opengoal]: OpenGOAL.
[^ps2-float]: FIZL0/ps2-float.
[^ps2-oracle]: FIZL0/ps2-oracle.
[^rabbitizer]: rabbitizer.
[^ghidra-ee]: ghidra-emotionengine-reloaded.
[^ps2tek]: ps2tek.
