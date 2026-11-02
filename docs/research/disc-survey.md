---
type: Research Note
title: 'What the three discs contain: a survey'
description: Measured facts about the user's three Sly Cooper USA disc images - files, executables, sections, instruction mix, VU microcode overlays, IOP modules and where the game data hides - that size and shape the recompilation plan.
tags: [research, disc, elf, r5900, vu1, iop, data-layout]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-02T21:25:08Z }
sources:
  - id: isos
    resource: oracle/*/*.iso (the user's own discs, read-only)
    title: 'Sly Cooper and the Thievius Raccoonus, Sly 2: Band of Thieves, Sly 3: Honor Among Thieves (USA) disc images'
  - id: readelf
    resource: readelf -h/-l/-S on the three boot executables, 2026-11-02
    title: ELF headers, segments and sections
  - id: ophist
    resource: a throwaway Python script classifying every 32-bit word of .text by opcode, 2026-11-02
    title: Instruction-class histogram of each executable's .text
---

# What the three discs contain: a survey

Everything here was measured on the user's discs on 2026-11-02 without running
anything from them. Nothing below comes from the community; where it touches
community knowledge it says so. Statements marked *Interpretation* are
inferences from the measurements, not measurements, and are to be confirmed. The plan that uses these numbers is
[plan.md](../plan.md).

## 1. The images

| | Sly 1 | Sly 2 | Sly 3 |
| --- | --- | --- | --- |
| Title | Sly Cooper and the Thievius Raccoonus | Sly 2: Band of Thieves | Sly 3: Honor Among Thieves |
| Boot ELF (`SYSTEM.CNF`) | `SCUS_971.98` | `SCUS_973.16` | `SCUS_974.64` |
| Build date (ISO) | 2002-08-24 | 2004-08-12 | 2005-08-27 |
| Image size | 2,802,319,360 B (DVD-5) | 2,884,141,056 B (DVD-5) | 3,553,755,136 B (DVD-5) |
| Image SHA-1 | `347b54b4a0d8b2c2e6c18cdbde5f1cabcc45b5da` | `7241cd566c9ee94ae1ba294ea31ed8f63fd56999` | `9f89eba20011bda2ea8e839c96b9100cf7762fc6` |
| ELF SHA-1 | `57dc305db57932ad3f1122966cdb695d2e62a47a` | `de2af92a54de71e33edc8a160c31c0337a95e6f5` | `5941b7877e6bf5dfb8acb2d04a3c5059367f385b` |
| IOP reboot image | `IOPRP243.IMG` | `IOPRP300.IMG` | `IOPRP300.IMG` |

The hashes have not yet been compared with Redump's; the recompiler should
refuse executables whose SHA-1 it does not know (see the plan, §5.1).[^isos]

## 2. Where the game data is

None of the three keeps its data in ordinary ISO 9660 files.[^isos]

- **Sly 1**: the filesystem holds only the ELF, the IOP image and six IRX
  modules (1,401 sectors). The other ~2.6 GiB sit in sectors no directory
  entry points at; the first non-zero one (LBA 12,345) starts with a `VAGp`
  header (Sony ADPCM audio).
- **Sly 2**: data occupies the gap between `IOPRP300.IMG` and a 32 MiB file
  `Z6TAIL.DUP` at the end of the disc. *Interpretation:* `Z6TAIL.DUP` is
  filler that pushes the data outwards on the disc. The first data sector (LBA 3,232) is an MPEG-2 program
  stream pack header (`00 00 01 BA`): an FMV.
- **Sly 3**: one 3.49 GB file, `SLY3.WAD`, from LBA 3,232. It too begins with
  an MPEG-2 pack header.

*Interpretation:* the executable locates its data by sector number, from a
table of contents in the ELF or at a fixed sector. Consequence: the port must serve raw 2048-byte sectors by LBA, not files by name. The recompiled game locates and decodes its own archives, so the archive formats matter only to tools and enhancements ([landscape](landscape.md)).

## 3. The executables

All three are `ELF32 LSB MIPS`, flags `0x20924001` (`noreorder, 5900, eabi64,
mips3`), entry point `0x100008`, **section headers intact but no symbol
table** (stripped). A `.gcc_except_table` in Sly 1 shows C++ with exceptions
built by GCC.[^readelf]

| Section | Sly 1 | Sly 2 | Sly 3 |
| --- | ---: | ---: | ---: |
| `.text` (EE code) | 1,121,804 B = **280,451 instr.** | 1,416,688 B = **354,172** | 1,835,600 B = **458,900** |
| `.vutext` (VU microcode) | 21,840 B | 34,112 B | 50,496 B |
| `.DVP.overlay..*` sections | 13 | 21 | 29 |
| `.rodata` | 233,915 B | 304,356 B | 393,764 B |
| `.data` | 175,156 B | 199,088 B | 254,808 B |
| `.bss` | 3,871,492 B | 1,527,424 B | 1,474,720 B |
| `.irx` (IOP modules linked into the ELF) | none (on disc) | 474,176 B | 475,072 B |

`.text` starts at `0x100000` in all three. The VU microcode is laid out by
Sony's `dvp-ld` (measured while building phase 1, 2026-11-02):

- The `.DVP.overlay..<vu address>.<id>.<seq>.<n>` sections hold **only
  zeros**; their sizes are the sizes of the overlays.
- The code itself lives in `.vutext`, inside DMA packets (a DMA tag, VIF
  codes, `MPG` uploads). `.DVP.ovlytab` locates it: one 12-byte entry per
  overlay, (name offset in `.DVP.ovlystrtab`, EE address in `.vutext`, VU
  micro-memory address).
- Through that table: Sly 1 has 2,702 pairs in 13 overlays, Sly 2 4,220 in
  21, Sly 3 6,254 in 29; every pair decodes as a valid instruction (ps2-isa,
  checked by `sly-verify`), and each game has 38-39 program ends (E flag).

*Interpretation:* the large overlay groups (8 and 3 chunks of 0x800 bytes
in Sly 1, matching the decompilation's `VU1_GlobMpg` and `VU1_BlipMpg`) are
the VU1 renderers, and a port must translate or reimplement every one.
Unconfirmed: that no further microcode is loaded from disc data (to check
by logging VIF1 `MPG` uploads in PCSX2 across several levels).

## 4. Instruction mix

Opcode classes over every word of `.text` (data words in `.text`, such as jump
tables, inflate the rare classes slightly; `daddu rd, rs, $zero` is how EE GCC
writes `move`, which inflates "64-bit ALU").[^ophist]

| Class | Sly 1 | Sly 2 | Sly 3 | What it means for the port |
| --- | ---: | ---: | ---: | --- |
| 64-bit ALU (`dadd*`, `dsll*`, ...) | 11.95% | 11.39% | 11.11% | GPRs are 64-bit (128 with MMI); model them as such |
| `lq`/`sq` (128-bit load/store) | 10.32% | 8.75% | 8.45% | prologues save full 128-bit GPRs; `u128` register file |
| COP1 FPU | 7.91% | 5.13% | 5.55% | PS2 float semantics (no NaN/Inf, round-to-zero) |
| `ld`/`sd`/unaligned 64-bit | 1.95% | 5.00% | 4.97% | |
| **COP2 VU0 macro (vector ops)** | **2.82%** | **2.23%** | **2.10%** | ~8-10k inline vector instructions: VU0 macro mode is core, not a corner case |
| `lqc2`/`sqc2` | 2.32% | 1.68% | 1.62% | |
| COP2 moves/control (`qmfc2`, `ctc2`, `bc2`) | 0.49% | 0.68% | 0.58% | includes VU0 micro-mode calls (`vcallms`) |
| MMI (128-bit integer SIMD) | 0.22% | 0.45% | 0.44% | small but must be exact |
| branch-likely | 1.19% | 1.32% | 1.42% | delay slot nullified when not taken |
| `jal` | 16,216 | 24,386 | 31,608 | direct calls: function discovery seeds |
| `jalr` (indirect calls) | 1,553 | 1,652 | 3,295 | C++ virtual calls and callbacks: need a runtime dispatch table |
| `jr` non-`$ra` | 199 | 269 | 382 | switch jump tables and tail calls |
| `syscall` | 85 | 156 | 156 | EE kernel calls: HLE |
| COP0 / `cache` / `sync` | ~90 | ~130 | ~125 | kernel and cache maintenance: mostly no-ops |

Sly 3's code is 1.64x Sly 1's. If the three share an engine (the community
says they do; [landscape](landscape.md)), most of the recompiler and runtime
carries over and the per-game work is concentrated in VU1 microcode, IOP
modules and data formats.

## 5. IOP (sound, pad, memory card, disc) modules

- **Sly 1** ships them as files: `SIO2MAN`, `PADMAN`, `MCMAN`, `MCSERV`,
  `LIBSD`, `989SND` - all Sony or 989 Studios libraries; no game-specific IOP
  module is present.
- **Sly 2 and 3** link nine modules into the ELF's `.irx` section:
  `sio2man`, `padman`, `mcman`, `mcserv`, `libsd`, `usbd`, `lgaud` (Logitech
  USB headset audio), `989snd`, and **`spiop`**, which imports `cdvdman`,
  `snd989` and `lgaud` and is not a Sony library. *Interpretation:* Sucker
  Punch's own IOP module (disc streaming and audio). Its RPC interface must be
  reverse-engineered for an HLE.
- All three use Sony's **989snd** sound system (strings such as
  `c:\vss\p3/989snd.cpp`, `989snd (c)2000-2003 Sony Computer Entertainment
  America`) on top of `libsd`. Sly 2 and 3 name `modules\989nomid.irx` and
  contain the string "NO MIDI VERSION! No MIDI, AME, or basic VAG sounds";
  *interpretation:* they use a 989snd build without MIDI sequencing.
- The `IOPRP` images hold only standard Sony kernel modules (`CDVDMAN`,
  `CDVDFSV`, `FILEIO`, `SIFMAN`, `SIFCMD`, `LOADCORE`, ...).

## 6. Sony libraries visible from the EE side

Strings show the standard SDK: `libgraph` (`sceGsPutDrawEnv`,
`sceGsSyncPath`), `libcdvd` (`sceCd*`), `libsif` RPC, `libpad`, `libmc`,
`libipu`/`libmpeg` (`sceMpegGetPicture`: FMV through the IPU), kernel threads
and semaphores (`SceKernelDelayThread`). These are high-level-emulation
targets: identify them by signature and replace whole functions with native
code instead of recompiling their hardware pokes.

[^isos]: The user's disc images in `oracle/`, read with `bsdtar`, `7z` and a small ISO 9660 directory parser, 2026-11-02.
[^readelf]: `readelf -h`, `-l` and `-S` on the three boot executables, 2026-11-02.
[^ophist]: A throwaway Python script classifying every 32-bit word of each `.text` by opcode, 2026-11-02.
