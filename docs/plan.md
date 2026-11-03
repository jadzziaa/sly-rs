---
type: Plan
title: 'sly-rs: plan for a Rust static recompilation of the Sly Cooper trilogy'
description: How to build a native PC port of the PS2 Sly Cooper trilogy from the user's own discs by static recompilation in Rust, bottom-up - goals, ground rules, approach, architecture, phases with exit gates, engineering practice, verification, performance budget, risks and open decisions.
tags: [plan, static-recompilation, rust, ps2, r5900, architecture, phases]
status: draft
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-03T01:30:46Z }
stale_after: 2027-05-01T00:00:00Z
sources:
  - id: request
    resource: human:august, conversation of 2026-11-02
    title: 'The request: scope a rac-rs-style Rust static recompilation of the Sly trilogy from the user''s own discs, planned bottom-up with Rust best practice before anything is built'
  - id: disc-survey
    resource: research/disc-survey.md
    title: 'What the three discs contain: a survey'
  - id: rac-rs
    resource: research/rac-rs.md
    title: How rac-rs is built, and what carries over to Sly
  - id: landscape
    resource: research/landscape.md
    title: The Sly and PS2 recompilation landscape
---

# sly-rs: plan for a Rust static recompilation of the Sly Cooper trilogy

A working plan, written before any code. The evidence is in three research
notes: [what the three discs contain](research/disc-survey.md),
[how rac-rs is built](research/rac-rs.md) and
[the Sly and PS2 recompilation landscape](research/landscape.md).

## 1. Summary

**Feasible, as a staged project with gates, Sly 1 first.** sly-rs would be
an offline recompiler that turns the user's own `SCUS` executables into Rust,
plus a Rust runtime that stands in for the PS2's kernel, IOP, graphics, sound
and movie hardware. All game data is read from the disc at run time.

What makes it tractable:

- **The code is modest and clean**: 280k (Sly 1) to 459k (Sly 3) EE
  instructions of GCC-compiled C++ with section headers intact, and only one
  game-specific IOP module (`spiop`, Sly 2 and 3).[^disc-survey]
- **Sly 1's function list and names are free**: a CC0 matching
  decompilation targets exactly the user's executable, with ~5,500 names from
  a prototype's debug symbols.[^landscape]
- **The method is proven twice**: rac-rs runs Ratchet & Clank this way in
  Rust,[^rac-rs] and Sly-2-Recomp boots Sly 2 to gameplay with the same idea
  in C++.[^landscape]

What makes it hard:

- **The graphics path** (VIF1 → VU1 microcode → GS) is the largest and least
  forgiving subsystem; the Sly games use GS features that naive GPU rendering
  gets wrong (antialiased one-pixel outlines, wrap-around blending, drawing
  from the frame being drawn).
- **Exactness everywhere**: PS2 float rules, EE threads and interrupt timing,
  and Sly 1's anticrack, which sabotages play if its own code bytes change.
- **Scale**: three games for one person. The plan answers with gates: each
  phase ends in a working, testable tool, and Sly 2 and 3 start only after
  Sly 1 is complete.

**Prior art changes the pitch, not the plan.** Sly 1 is already playable
natively through ProjectCane (a hand-written C++ port), and Sly 2 through
Sly-2-Recomp (a C++ static recompilation); Sly 3 has nothing. sly-rs's place
is one faithful-by-construction Rust codebase for all three, deterministic
and fast, and the only port of Sly 3. Whether to build it anyway is the first
open decision (§11, O0).

## 2. Goals and non-goals

| # | Goal | Measured by |
| --- | --- | --- |
| G1 | Sly 1, then Sly 2, then Sly 3 run natively on Linux (then Windows) from the user's own discs, with no BIOS and no emulator | each game completable start to finish on recorded replays |
| G2 | The port behaves like the PS2 game | the oracles in §8: our interpreter, PCSX2, the Sly 1 decompilation |
| G3 | Lean and fast | the budget in §9: frame time, start-up, memory, binary size, build time |
| G4 | Enhancements only after parity: internal resolution, widescreen, frame-rate options, modern input, saves as files | each behind an option, off by default until verified; off means bit-identical RAM |
| G5 | The PS2 layers are game-agnostic crates; Sly knowledge lives in per-game crates and data files | no `sly-*` dependency in any `ps2-*` crate, enforced in CI |

**Non-goals**: a general PS2 emulator; games other than these three; PAL or
Japanese releases until the USA ports are done; the USB headset (`lgaud`) and
other USB devices; any online feature; shipping any byte of game code or data.

## 3. Ground rules: assets, legal and repository hygiene

- **The repository holds no game bytes**: no executable, no disc data, no
  BIOS, no generated code (it is derived from the executable), no extracted
  assets, no captured frames of the game in test fixtures, and no
  instruction-by-instruction hand translations of game functions (rac-rs
  checks some in; sly-rs does not: [rac-rs §6](research/rac-rs.md)). Committed
  per-game data is limited to addresses, sizes, hashes, names and structure
  layouts.
- **The discs stay where the user keeps them.** They are in `oracle/` at
  the root of the repository (not `~/oracle`): phase 0 adds `oracle/` and `generated/` to `.gitignore` before
  the first commit. Tools find discs through `sly-rs.toml` or `SLY_RS_DISC_*`
  environment variables, never through a path in source.
- **One known build per game.** The tools accept only images and executables
  whose SHA-1 is in their table (USA 1.00: [disc survey §1](research/disc-survey.md)).
  Every per-game address is keyed to that build.
- **Community data with attribution**: symbol names, type layouts and format
  notes from community projects are used under their licences and credited in
  `NOTICE.md`. The Sly 1 decompilation is CC0 and NativeEE / Sly-2-Recomp are
  MIT / Apache-2.0: usable. ProjectCane and the Komasia Sly 2 decompilation
  carry no licence and OpenThievius is AGPL-3.0: read for facts, never copied
  ([landscape §1](research/landscape.md)).
- **Guest code bytes are never modified.** Sly 1 checksums its own code and
  sabotages play when it changes ([landscape §2](research/landscape.md)).
  Patches (widescreen, bug fixes) are applied in generated code or hooks;
  guest RAM always holds the original bytes.
- **Licence: GPL-3.0-or-later** (decided, §11 O1), which lets the project
  port code from rac-rs and PCSX2.
- This is not legal advice. The plan follows the pattern of existing
  recompilation projects: the user supplies the game, the project supplies
  only original code.

## 4. Approach

Five principles, each taken from what worked in rac-rs or forced by the discs.

1. **Recompile the game's EE code ahead of time; emulate nothing else at the
   instruction level that can be replaced at a higher level.** The EE's
   280k-459k instructions ([disc survey §3](research/disc-survey.md)) become
   Rust source, including the Sony SDK libraries linked into it; only the SDK
   functions whose work belongs to the host (disc, pad, memory card, movies,
   kernel calls) are hooked. The BIOS, the EE kernel and the IOP's system
   modules are high-level emulations (HLE) in Rust. The VU1
   microprograms (a fixed set of `.DVP` overlays inside each ELF) and the IOP
   sound modules the game ships (`libsd`, `989snd`, `spiop`) are recompiled
   ahead of time too, each with an interpreter behind it.
2. **An interpreter first, and always.** One crate defines the semantics of
   every instruction; the interpreter and the generated code both call it.
   The interpreter is the recompiler's equivalence oracle, the fallback for
   any code the analysis missed, and the way to run the game before the
   recompiler exists.
3. **Determinism by construction.** Time is the guest instruction count
   (one instruction = one EE cycle, as in rac-rs). Interrupts, DMA
   completions and thread switches happen at exact counts. The same inputs
   give the same RAM, bit for bit, whether code runs interpreted, recompiled
   or replaced by a native hook. Every test in §8 rests on this.
4. **Generic first, game-specific on top.** The faithful path for graphics
   (DMA → VIF1 → VU1 → GIF → GS) comes before any native re-rendering;
   rac-rs's "mirror renderer" was possible only because Ratchet's level
   formats were already documented ([rac-rs §8](research/rac-rs.md)).
5. **Bottom-up with gates.** Each phase (§6) ends in a measurable gate and a
   tool that is useful on its own. Nothing above a layer is started until the
   layer's gate passes.

**Sly 1 first**, for reasons measured on the discs: the smallest code (280k
instructions against 354k and 459k), the fewest VU microcode overlays (13
against 21 and 29; two real programs per the decompilation), only stock Sony
IOP modules as plain files (Sly 2 and 3 add Sucker Punch's own `spiop`
module, USB and headset drivers), and a CC0 decompilation that targets the
user's exact executable, with names
([landscape §1](research/landscape.md)). Sly 2 and 3 then arrive as deltas on
a working engine, with Sly-2-Recomp's notes as a guide for Sly 2.

## 5. Architecture

### 5.1 Data flow

```text
user's ISO ──► ps2-disc (sector reader, SHA-1 gate)
                 │
                 ├─► sly-recomp (offline, once per disc):
                 │     ELF ─► ps2-analysis (functions, CFG, jump tables)
                 │         ─► ps2-codegen ─► generated/<game>/rec-NN crates (Rust)
                 │     VU microcode (.vutext, via .DVP.ovlytab) ─► VU1 codegen ─► generated/<game>/vu crate
                 │     IRX (libsd, 989snd, spiop) ─► R3000 codegen ─► generated/<game>/iop crate
                 │
                 └─► sly (the game binary, at run time):
                       ps2-core ◄── generated crates (their only dependency)
                       ps2-ee: interpreter, memory map, EE kernel HLE, scheduler,
                               dispatcher, hooks
                       ps2-iop: SIF and RPC; HLE servers (cdvd, pad, memory card);
                                IOP kernel HLE running the recompiled sound modules
                       ps2-dma ─► ps2-vu (VU1) ─► ps2-gs (GS memory, rasteriser)
                       ps2-spu2 (voices, mixer)     ps2-mpeg (FMV)
                       sly-app: winit window, wgpu presentation, cpal, gilrs
```

### 5.2 Workspace

```text
sly-rs/
├─ Cargo.toml            workspace: members, [workspace.package], [workspace.dependencies], [workspace.lints]
├─ rust-toolchain.toml   pinned stable toolchain
├─ deny.toml  rustfmt.toml  clippy.toml  .config/nextest.toml  .cargo/config.toml
├─ crates/
│  ├─ ps2-disc        ISO 9660 + raw sector access by LBA, image identification
│  ├─ ps2-isa         R5900 and VU instruction decoders, disassembler (no deps, no_std)
│  ├─ ps2-core        stable ABI for all generated code (EE, IOP, VU): register and memory
│  │                  views, inline semantics (ALU, MMI, PS2 FPU, VU0 macro, VU micro),
│  │                  slow-path declarations (no deps)
│  ├─ ps2-ee          interpreter, memory map and MMIO, EE kernel HLE and thread
│  │                  scheduler, BIOS-free boot, recompiled-code dispatcher, hooks
│  ├─ ps2-iop         SIF and SIF RPC; HLE servers for cdvd, pad, memory card; IOP kernel
│  │                  HLE (threads, semaphores, timers, interrupts) for recompiled IRX
│  ├─ ps2-dma         DMAC channels, VIF0/VIF1, GIF path arbitration
│  ├─ ps2-vu          VU0 micro mode and VU1: interpreter, runtime for translated programs
│  ├─ ps2-gs          GS registers, local memory and swizzling, CRTC, software rasteriser
│  ├─ ps2-gs-wgpu     hardware GS renderer on wgpu (phase 9)
│  ├─ ps2-spu2        SPU2 voices: ADPCM, ADSR, interpolation, reverb, mixer
│  ├─ ps2-mpeg        PSS demultiplexer, MPEG-2 video decoder, ADPCM audio
│  ├─ ps2-analysis    ELF loading, function discovery, CFG, switch recovery, SDK signatures
│  ├─ ps2-codegen     EE, IOP (R3000) and VU lowering to Rust source, structuring, chunking
│  ├─ sly-formats     Sly archives and formats, read-only (phase 12; not needed to run)
│  ├─ sly-common      what the three games share: hooks, RPC servers, patches
│  ├─ sly1  sly2  sly3   per game: hashes, bounds, hooks, configuration
│  └─ sly-app         the binary: window, presentation, audio, input, config, CLI
├─ xtask/             cargo xtask recomp | ci | test-full | bench | dist
├─ games/sly1/…       committed data: functions.tsv, symbols.toml, hooks.toml (addresses only)
├─ generated/         gitignored: generated crates per game, with a stamp file
├─ docs/              this bundle (OKF)
└─ oracle/            gitignored: the user's discs
```

Rules:

- **Generated crates depend on `ps2-core` alone**, which has no
  dependencies and changes rarely; changing the interpreter, renderer or app
  never recompiles generated code ([rac-rs §7](research/rac-rs.md)).
- **`unsafe` is confined** to `ps2-core` (the memory fast path and the CPU
  view), the presentation layer's FFI and nowhere else; every other crate has
  `#![forbid(unsafe_code)]`. Each `unsafe` block carries a `// SAFETY:`
  comment and a test that exercises its boundary.
- **Generation is an explicit step, not `build.rs`**: `cargo xtask recomp
  --game sly1` reads the disc, checks its hash, and writes crates into
  `generated/` only when their content changed (preserving modification
  times, so cargo rebuilds only what moved). A stamp records the disc hash,
  generator version and inputs. `build.rs` code generation would hide millions
  of lines of output, rerun unpredictably and slow every build.
- The game binary links the generated crates of the games it was built for
  (cargo features `sly1`, `sly2`, `sly3`).

### 5.3 Key runtime decisions

| Area | Decision | Why |
| --- | --- | --- |
| Registers | GPRs `[u128; 32]`, FPRs `[f32; 32]` as bits, VU0 `vf[32]` as `[f32; 4]`, `vi[16]` | the EE's 128-bit GPRs are used constantly (`lq`/`sq` are 8-10% of instructions) |
| Memory | one owned allocation per region (32 MiB RAM, 16 KiB scratchpad, VU memories); range-checked fast path inline, MMIO and mirrors out of line | simple, portable, measured enough in rac-rs; host virtual-memory tricks only if profiling demands them |
| Scratchpad | modelled exactly, including scratchpad DMA | Sly 1 uses scratchpad DMA; Sly 2 decompresses level data in it |
| Guest code | the original ELF bytes stay in guest RAM, untouched; patches live in generated code or hooks | Sly 1's anticrack checksums its code |
| Floating point | PCSX2's semantics (round toward zero, no denormals, Inf/NaN as ±FLT_MAX, missing guard bits) implemented with ordinary SSE arithmetic, no MXCSR switching; `ps2-float` evaluated as implementation or oracle | physics must match the console; Sly 3's VU0 code is known to be sensitive; MXCSR switching stalled rac-rs |
| Transcendentals | a pinned pure-Rust `libm` everywhere | last-bit differences broke rac-rs's cross-OS replays |
| Control flow | per-function Rust `fn`; structured loops and blocks from dominators; dispatch-loop fallback for irreducible graphs; `jalr` and unknown targets through a PC table | rac-rs's proven design |
| Guest stack | guest calls are Rust calls on the host stack, but host frames hold no guest state: compiled code returns to the machine for every system call, interrupt and thread switch, and resumes at any block (decided in phase 4, [finding](research/recompiled-control.md)) | direct Rust calls between generated functions, with one host stack and no `unsafe` |
| EE threads | an HLE kernel with a **real scheduler**: strict priorities and the EE kernel's rotation rules; a thread switch is the kernel's saved-register swap, always on an empty host stack (see Guest stack); switches only at syscalls and interrupt points | Sly's executables carry kernel thread and semaphore machinery; rac-rs's "no scheduler" shortcut cannot be assumed |
| Idle | a thread waiting on a semaphore that only an interrupt can signal lets the clock jump to the next event | no busy-waiting on the host; VSync waits cost nothing |
| IOP, system side | Sony's system and driver modules (`cdvdman`, `fileio`, `sifcmd`, `sio2man`, `padman`, `mcman`/`mcserv`) answered at the RPC level in Rust; `usbd` and `lgaud` answer "no device" | known interfaces; their work is hardware access the host does differently (sectors from the image, host pads, card files) |
| IOP, sound side | the modules the game ships for sound and streaming (`libsd`, `989snd`, `spiop`) **recompiled** from their IRX (R3000, MIPS I) like the EE code, running on an IOP kernel HLE over a SPU2 register model, on the same instruction clock (1 IOP cycle per 8 EE cycles) | exact by construction; no need to reverse-engineer `spiop`; Sly-2-Recomp does the same, rac-rs instead hand-ported 989snd |
| Graphics | DMA → VIF1 → VU1 (translated microprograms) → GIF → GS; software rasteriser first (exact, deterministic, testable), wgpu hardware renderer for resolution scaling after parity | faithful and generic; the per-game "mirror" approach is a later option, not a foundation |
| FMV | hook Sony's `libmpeg` functions and decode PSS with a Rust MPEG-2 decoder | no IPU emulation needed (rac-rs did the same) |
| Audio | the recompiled 989snd/libsd drive a SPU2 model (registers, voices, ADPCM, ADSR, reverb, mixer); `cpal` output | 989snd is used by all three games |
| Hooks | native replacements keyed by address *and* code hash, allowed to decline and fall back; hooked functions stay interpreted | rac-rs's `Hand`; a compiled call can never skip a hook |

## 6. Phases and exit gates

Each phase lists its work, what it reuses, and its gate. Estimates are not
given: the gates are the schedule.

### Phase 0 - Foundations

- `git init`; `.gitignore` (`oracle/`, `generated/`, `target/`) before the
  first commit; licence (O1), `NOTICE.md`, `README.md`.
- Workspace skeleton (§5.2) with `[workspace.lints]`, `rust-toolchain.toml`,
  `deny.toml`, `rustfmt.toml`, nextest profile; `cargo xtask ci` running fmt,
  clippy, tests, `cargo deny` and the dependency-direction check (G5).
- `ps2-disc`: open an image (positioned reads; memory mapping optional),
  ISO 9660 root directory, raw sectors by LBA, SHA-1 identification against a
  table of the three USA images.
- `sly-app info <disc>`: identifies the game and prints the facts in
  [the disc survey](research/disc-survey.md) (sections, sizes, modules), so the
  survey becomes a reproducible command.
- **Gate**: CI green on an empty-disc machine; `info` identifies all three
  discs; disc tests skip cleanly when no disc is configured.
- **Status, 2026-11-02: gate passed.**
  - What was built: `ps2-disc`, `sly-common` (the known-builds table, kept
    there until the per-game crates have more to hold), `sly-app` with
    `sly-rs info`, and `xtask`.
  - Results: `info` reproduces the disc survey. `cargo xtask test-full`
    confirms all three executables and full-image SHA-1s (9 GB hashed in
    5 s). 13 unit tests pass.
  - Gaps: `cargo deny` is wired into `cargo xtask ci` but skipped until
    `cargo-deny` is installed. There is no hosted CI yet (O7). Since
    2026-11-03, with the repository public, GitHub Actions runs
    `cargo xtask ci` on every push to `main` and every pull request
    (`.github/workflows/ci.yml`); the disc tests stay local.

### Phase 1 - Instruction set

- `ps2-isa`: decoders for the R5900 (MIPS III base, 64-bit ALU, MMI, COP0,
  COP1, COP2 macro mode) and the VU micro instruction pair (upper/lower).
  Decoding is table-driven into a compact `Copy` type; no allocation; total
  (every word decodes to an instruction or `Invalid`).
- Disassembler in the conventional syntax.
- Tests: exhaustive totality over all 2^32 words (release-mode test, minutes),
  golden disassembly snapshots (`insta`) of hand-picked words, and a diff of
  every `.text` word of the three ELFs against `rabbitizer` (R5900 including
  MMI) as a dev-dependency; VU micro instructions against PCSX2's VU
  disassembler output on the `.DVP` overlays.
- **Gate**: zero unexplained disagreements with the references over the three
  executables; decode throughput benchmarked.
- **Status, 2026-11-02: gate passed.**
  - What was built: `ps2-isa` (no dependencies, `no_std`) with 403 R5900
    operations (base, MMI, COP0, the single-precision FPU, COP2 macro mode)
    and VU micro mode (95 upper, 70 lower operations). The disc-dependent
    checks live in a new crate, `sly-verify`.
  - EE against rabbitizer, exhaustively over all 2^32 words (3 s): validity
    and mnemonic agree. The explained differences:
    - rabbitizer accepts base-MIPS instructions the R5900 lacks: doubles,
      `ll`/`sc`, 64-bit multiply and divide, the extra FPU compares.
    - It lacks the R5900's three-operand `multu`.
    - It rejects `syscall` codes.
    - It ignores `sync`'s stype bits.
    - It names `sqrt.s` `c1`.
  - Full text against rabbitizer on every word of the three executables'
    `.text`: 1,093,523 words, 12 invalid for both, the rest identical. The
    explained differences: rabbitizer swaps `vadda`/`vmsuba`'s sources, and
    the `sqrt.s` name.
  - VU against PCSX2's opcode tables, slot by slot. Two slips in PCSX2's
    disassembler are noted: `SUBx` for `SUBAx`, `CLIP` for `CLIPw`. All
    13,176 microcode pairs of the three games decode as valid.
  - Throughput on random words, one core: 100 M EE decodes/s, 20 M
    disassembled lines/s, 105 M VU pairs/s.
  - Deviations from the plan:
    - The VU reference is PCSX2's tables, because its disassembler has no
      headless mode.
    - No `insta` snapshots: unit tests compare strings directly, and the
      1.09 M-word comparison with rabbitizer serves as the snapshot.
  - Finding: the microcode is not in the `.DVP.overlay` sections, which
    are zero placeholders, but in `.vutext`
    ([disc survey §3](research/disc-survey.md)).

### Phase 2 - Analysis

- `ps2-analysis`: load ELF segments (`object` crate) into a 32 MiB image;
  function bounds from `games/<game>/functions.tsv` plus discovery (`jal`
  targets, `jalr` candidate tables, prologue patterns, hole scan accepting only
  regions that decode cleanly and terminate, code-pointer words);
  basic blocks and CFG with delay slots and branch-likely; GCC switch-table
  recovery (`sltiu` bound + `lui`/`addiu`/`lw`); identification of Sony SDK
  functions by masked byte signatures (so `libgraph`, `libcdvd`, `libmpeg`,
  `libpad`, `libmc` and the kernel stubs get names and HLE hooks).
- Boundary heuristics handle both code styles in these executables: game code
  (16-byte `sq` saves, few tail calls) and Sony's SDK libraries (8-byte `sd`
  saves, `j` tail calls).
- Sly 1: function bounds and names imported from the CC0 decompilation's
  `symbol_addrs.txt` (it targets the user's exact executable), demangled; the
  discovery pass cross-checks it. Sly 2 and 3: the discovery pass plus a
  headless Ghidra export with ghidra-emotionengine-reloaded (O3), compared
  with the public Sly 2 function lists as a read-only check.
- Reports: coverage of `.text` by functions, unexplained bytes, indirect-jump
  sites without known targets.
- Move the VU overlay extraction (`.DVP.ovlytab` → `.vutext`), written in
  `sly-verify` during phase 1, into `ps2-analysis`: it is generic `dvp-ld`
  layout knowledge that phase 6 feeds to the VU translator.
- Second opinion on discovery: rabbitizer's `is_function_call`,
  `is_return` and `is_jumptable_jump` heuristics, compared on the same 1.09 M
  words as phase 1's disassembly check.
- **Gate**: Sly 1 function boundaries agree with the decompilation's for ≥ 99%
  of functions, every disagreement explained; < 0.1% of `.text` unexplained;
  the same reports produced for Sly 2 and 3.

- **Status, 2026-11-02: gate passed** (details:
  [research/function-discovery.md](research/function-discovery.md)).
  - What was built: `ps2-analysis`, with the memory image, VU overlays
    (moved from `sly-verify`), jump tables, blind function discovery, control
    flow graphs, syscall stubs and fingerprints. Two new commands, `sly-rs
    analyze` and `sly-rs disasm`. Sly 1's decompilation symbols are imported
    as `games/sly1/symbols.tsv` (CC0, `cargo xtask import-sly1-symbols`).
  - Discovery runs without symbols.
    - It finds 3,947 of the decompilation's 3,949 Sly 1 function starts
      (99.95%).
    - The 2 misses are `switch` case blocks that the decompilation's Ghidra
      pass split off as functions. They are asserted as such.
    - Every byte of `.text` is accounted for: 16 bytes of data per game, the
      rest functions and padding.
  - Independent read-only checks against the other decompilations' lists
    (not stored in the repository):
    - Sly 1 against OpenThievius: 4,769 of 4,770 starts agree. The one
      difference is leading padding.
    - Sly 2 against Komasia: 6,811 of 6,816 agree.
  - Jump tables: 196, 266 and 379 recovered, almost all bounded by their
    range checks. Each game has 3 indirect `jr`s left, all in C-library
    `longjmp`/unwind code.
  - Every function gets a control-flow graph whose edges stay inside it:
    54k, 73k and 97k blocks.
  - Classification of calls, returns and jump-table jumps matches
    rabbitizer's on every valid word.
  - Deviations from the plan:
    - No Ghidra export (O3). Discovery from the code alone agrees with two
      independent decompilations, so the tool-time Java dependency is not
      needed yet.
    - Naming SDK functions in Sly 2 and 3 by exact fingerprint carries only
      64 and 47 of Sly 1's names, because the SDK (2.4 to 3.0) and the
      engine changed. Fuzzier matching is deferred to when hooks need the
      names (phase 3 onwards).
    - Kernel stubs are named by syscall number: 77 of 80 in Sly 1, 141 of
      151 in Sly 2 and 3.

### Phase 3 - Machine model and interpreter

- `ps2-core`: register file, memory views and the inline semantics of every
  instruction, including PS2 FPU rules, MMI and VU0 macro mode with flags.
- `ps2-ee`: interpreter over `ps2-core`; the memory map (RAM, mirrors,
  scratchpad, VU memories, MMIO dispatch); the instruction clock and an event
  queue (VBlank, timers, DMA completion); INTC/DMAC interrupts; the EE kernel
  HLE by syscall number (threads, semaphores, alarms, interrupt handlers,
  `FlushCache`, `SetGsCrt`, SIF DMA); BIOS-free boot (load the ELF, set up
  the stack and arguments as the BIOS would, skip the IOP reboot).
- `ps2-iop` scaffold: SIF RPC bind/call/status; the cdvd server already reads
  sectors by LBA synchronously through `ps2-disc`, since Sly 1 reads its
  hidden data right after start-up; other servers log and answer "ok".
- Conformance: small test programs written by us, built with the open
  ps2sdk toolchain, exercising FPU, MMI and VU0 edge cases; run under PCSX2 to
  record expected results, then under our interpreter. These programs are our
  own code and can live in the repository. `ps2-float` (bit-exact to PCSX2
  recordings) is a second reference for the float rules, and `ps2-oracle` (an
  independent R5900 interpreter) a second reference for whole functions.
- **Gate**: Sly 1 runs headless in the interpreter from its entry point
  through C++ static constructors, IOP and SIF setup and its first disc reads,
  as far as its first wait on a subsystem not yet built (documented); every
  syscall and RPC on the way is logged and named; two runs give identical RAM
  hashes.

- **Status, 2026-11-02: gate passed; conformance done; float rules
  unverified** (details: [research/sly1-boot.md](research/sly1-boot.md),
  [research/conformance.md](research/conformance.md)).
  - What was built:
    - `ps2-core`: CPU state, the memory map, every R5900 operation, PS2
      float rules, MMI, and VU0 macro mode with its flags.
    - `ps2-ee`: the interpreter at 160-180 M instructions/s, V-blank events,
      INTC/DMAC/GS CSR/timer/SBUS registers, and the kernel HLE (threads,
      semaphores, interrupt handlers, alarms, kernel patches, SIF calls).
    - `ps2-iop`: SIF commands and RPC, the CD/DVD servers (reads by LBA)
      and the module loader.
    - `sly-rs run`, with hot-function sampling and PC watchpoints.
  - Gate: Sly 1 boots through crt0, the kernel patches, threads, the SIF
    handshake, the IOP reboot, CD/DVD and loader RPC and the loading of six
    modules. It stops at the 989snd sound driver's first call: its first
    wait on a subsystem not yet built. Two 300-field runs give the same
    state hash. Every syscall is named; RPC servers are named by module.
  - Deviation: no sectors are read before that wait. Sly 1 reads its data
    only after sound start-up, so the CD/DVD read server is written but not
    yet exercised by the game.
  - Conformance: our own test program (`conformance/`, 7,290 cases: FPU,
    integer, MMI, unaligned loads and stores, VU0) runs in the interpreter
    and in PCSX2. PCSX2 runs headless, in its recompiler and interpreter
    modes.
    - It found three MMI bugs (`plzcw`, `phmadh`/`phmsbh`, `pdivbw`), now
      fixed. Every integer, MMI and load/store line now matches PCSX2 in at
      least one mode.
    - `cargo xtask ci` holds the output to the committed transcript.
  - PCSX2 is not a float or VU0 reference: it uses x86 SSE arithmetic.
    Differences remain on guard bits (add, sub, msub and the VU adds) and on
    VU div/sqrt rounding. The float rules in `ps2-core` are unverified until
    results from a real console exist.
    - Contrary to the floating-point row of section 5.3, our rules are not
      PCSX2's: PCSX2's default mode has no guard-bit model and chops VU
      division.
  - Outstanding: comparisons with `ps2-float` and `ps2-oracle`.
  - What "two runs agree" shows: nothing yet reads host time or randomness,
    so it guards against future nondeterminism rather than proving much
    today. The real determinism test is phase 4's lockstep of interpreter
    and recompiled code.
  - For phase 4 (superseded): the concern was that a saved register set
    works only while code is interpreted. Phase 4's generated code hands
    control back before any switch, so the register set is all a thread
    needs ([finding](research/recompiled-control.md)).

### Phase 4 - Recompiler

- `ps2-codegen`: lowering of every instruction to Rust over `ps2-core`;
  structured control flow with a dispatch-loop fallback; the PC table for
  `jalr`; per-block instruction-budget checks so events land at exact counts;
  interpreter fallback for anything unsupported; crates of ~10-12k
  instructions; compile-time measures from rac-rs (out-of-line memory slow
  paths, shared cold helpers).
- `cargo xtask recomp --game sly1`; `ps2-ee` dispatcher choosing recompiled
  code, hooks or the interpreter per function.
- Differential tests: record function-entry states (registers and memory
  reads) during an interpreter run, replay each through the recompiled
  function, compare all writes and the instruction count; lockstep mode that
  runs both side by side for N frames.
- **Gate**: Sly 1 reaches the same point as phase 3 with identical RAM hashes
  at every checkpoint; ≥ 99.9% of executed instructions run in recompiled
  code; full regeneration and build time recorded (§9).

- **Status, 2026-11-03: gate passed** (design:
  [research/recompiled-control.md](research/recompiled-control.md)).
  - What was built:
    - `ps2-core::rec`: `Ctx`, the state generated code runs on; the PC
      table; exits with reasons.
    - `ps2-codegen`: each function becomes a loop over a `match` on its
      blocks. ALU, shifts, loads and stores, branches and common FPU
      operations are written out; the rest call the shared semantics.
    - The dispatcher in `ps2-ee::Machine::run`: generated code where the
      table has it, the interpreter elsewhere, with statistics.
    - `cargo xtask recomp <game> [--build | --check]`: writes a separate
      Cargo workspace under `generated/<game>/`, with crates of about 12,000
      instructions and a binary that is `sly-app`'s command line plus the
      generated code (`run`, `lockstep`).
  - Gate (`cargo xtask recomp sly1 --check`):
    - Sly 1 runs 300 fields recompiled and interpreted side by side.
      Registers, RAM and the scratchpad are equal at every field, so it
      reaches the same point as phase 3.
    - 99.958% of instructions run compiled. The rest are almost all the
      system call instructions of the sound driver's wait loop
      (`FlushCache`, `Deci2Call`, `sceSifGetReg`).
  - Measurements (§9):
    - Generation: 4,806 functions, 24 crates, 641,509 lines, 0.6 s.
    - Full release build of the generated workspace: 48 s on the reference
      machine. A regeneration that changes nothing writes no files and
      rebuilds nothing. After a change to `sly-app`, the rebuild takes
      29.7 s and rebuilds no generated crate (target: 30 s).
    - The recompiled machine runs about 2.4 billion instructions a second
      in this wait loop (0.6 s for 300 fields; the interpreter takes 10.8 s).
  - The measure, refined in phase 5: system call instructions are counted
    apart. The kernel's emulation runs them by design, so they say nothing
    about code the recompiler missed. With the sound driver running, the
    game spins on `FlushCache` while its banks load. Counting those calls
    would put the share at 97.7%; leaving them out gives 99.990%.
  - In CI, without a disc: the conformance program is recompiled at build
    time (`conformance/rec`, an exception to the no-`build.rs` rule for our
    own test program). It must print the expected output, stay in lockstep
    with the interpreter (registers and RAM every 10,000 cycles) and run at
    least 99.9% compiled. A snapshot test pins the shape of generated code.
  - Deviations:
    - The plan's per-function replay tests (record entry states, replay
      through the generated function) are not built. Whole-machine
      lockstep, which narrows a difference to a window of a few cycles,
      covers the same ground for now.
    - Hooks are not built yet: no function needs one before phase 5.
  - Open:
    - Generated code assumes the executable's code never changes in RAM.
      Nothing checks that at run time yet (rac-rs re-validates on
      `FlushCache`). Lockstep would show it as a difference.
    - Recompiled code may run differently once more of the game runs. Run
      the gate again whenever the game gets further.

### Phase 5 - IOP, disc, pad and memory card

- cdvd server: sector reads by LBA from the image, asynchronous completion and
  callbacks at modelled times. No Sly format knowledge is needed to run: the
  recompiled game finds, decrypts and decompresses its own archives; sly-rs
  only serves sectors.
- DMA channels the game kicks (VIF1, GIF, IPU) complete as sinks at modelled
  times so its interrupt handlers run; their data goes nowhere until phase 6.
- Pad (host state into the DualShock 2 report), memory card (Sony's server
  over a PCSX2-compatible `.ps2` card image, so saves move between the two),
  `libmpeg` hook that skips movies.
- The IOP runtime: R3000 decoding and lowering (a subset of the EE work) over
  the IOP view in `ps2-core`, an R3000 interpreter as its oracle and fallback,
  IOP RAM (2 MiB), the IOP DMA controller (SPU2 and SIF channels), and the
  `ps2-iop` kernel HLE providing the library exports the modules import
  (`intrman`, `thbase`, `thsemap`, `sysmem`, `loadcore`, `sifman`, `sifcmd`,
  `cdvdman`, `sysclib`, `stdio`), IOP threads and timers on the shared
  instruction clock. `libsd`
  and `989snd` are recompiled and loaded as the game loads them, so the game
  talks to its real sound driver from the start; no stand-in has to guess the
  driver's RPC protocol.
- A silent SPU2: the register file, its 2 MiB of sound memory (filled by IOP
  DMA, the path sample banks and streams take: disc → IOP RAM → SPU2 memory)
  and each voice's state (play position, loop and end flags, interrupts)
  advance in time so the driver and streams behave, but no samples are
  produced yet.
- Anticrack check: a test that runs Sly 1's four code checksums and asserts
  they pass.
- Replay format: pad input per field plus RAM-hash checkpoints.
- **Gate**: Sly 1 headless reaches the title screen, starts a new game and
  loads the first level under a scripted pad replay; two runs give identical
  hashes.

- **Status, 2026-11-03: gate passed; three items outstanding** (details:
  [research/sly1-boot.md](research/sly1-boot.md)).
  - Built, through the advisor's sub-gates (a)-(d):
    - (a) The IOP runtime: R3000 semantics in `ps2-core::iop`, an IRX
      loader with relocation and linking, and a kernel HLE.
      - The HLE: threads with priorities, semaphores, event flags, alarms,
        interrupt handlers, hardware timers, `sysmem`, `sysclib`, `stdio`,
        `sifman`, the server side of `sifcmd`, `cdvdman` reads.
      - Our own IOP conformance module (`conformance/iop`) prints exactly
        what it prints on PCSX2, whose IOP runs Sony's own kernel.
    - (b) `LIBSD` starts. (c) `989SND` starts and registers its RPC
      servers. (d) The game's sound calls get real replies, and both sound
      banks load into SPU2 memory through a modelled DMA.
    - DMA transfers complete at modelled times with the GS events their
      packets ask for, so the game runs one frame per field.
    - `padman` (the protocol ps2sdk's `libpad` documents, version 4.3 like
      the disc's module): a DualShock 2 in port 0 whose record is written
      into EE memory each V-blank.
    - `mcserv` (MCSERV 2.0f numbering): no card inserted yet.
    - Hooks (plan §5.3): native replacements by address, left out of
      generated code so a compiled call always reaches them. Movies are
      skipped by three hooks around `CMpeg::Execute`'s decoding, so the
      game's own bookkeeping around a movie still runs.
    - Observables read from the game's memory (`sly-common::state`):
      interface, joypad, save system, world and level.
    - Replays (`games/sly1/replays/*.toml`): presses per field and
      checkpoints of world, level and state hash; `sly-rs replay` checks or
      records them.
  - Gate (`new_game` disc test): from power-on the replay reaches the title
    (the attract world), starts a new game and loads the first level
    (`jb_intro`). Two runs give the recorded hash at every checkpoint.
    - The recompiled build gives the same hashes. Lockstep with the
      replay's input is equal for 2,000 fields, 99.990% recompiled.
    - The anticrack checks run and pass: `g_anticrack_frm` stays 0 and the
      code in RAM is the executable's, byte for byte.
    - Caveat: "title" and "first level" are the game state's world ids
      (decompilation, `include/game.h`); nothing is drawn yet to confirm it
      by eye.
  - Outstanding, carried to the phases that need them:
    - A memory card image (PCSX2-compatible `.ps2`), for saving.
    - One disc drive model with modelled read times shared by `cdvdfsv` and
      `cdvdman`: reads complete at once for now.
    - Recompiling the IOP's modules: they run on the interpreter, which is
      fast enough (the IOP idles most of the time).
    - The SPU2 has no voices yet (phase 8).

### Phase 6 - Graphics

- `ps2-dma`: DMAC chains for VIF1, GIF and scratchpad; `ps2-vu`: VIF1 unpack
  and commands, VU1 interpreter with the pipeline behaviour games rely on,
  XGKICK; ahead-of-time translation of every `.DVP` overlay, checked against
  the bytes the game actually uploads (`MPG`), interpreter for anything else;
  GIF paths 1-3.
- `ps2-gs`: privileged and general registers, local memory with every pixel
  storage format and swizzle, CLUTs, the software rasteriser (triangles,
  sprites, lines with `AA1` antialiasing for the toon outlines; texture
  filtering; alpha blending with `COLCLAMP` wrap-around and `FBMSK`, which
  Sly 2's shadows count with; depth, destination-alpha and frame tests; fog;
  drawing from the frame being drawn), CRTC output with field handling;
  `sly-app` presents the frame through wgpu.
- Oracles: GS dumps recorded in PCSX2 from the user's game and replayed through
  `ps2-gs`, compared with PCSX2's software renderer; VU1 interpreter against
  its translations on recorded VIF1 streams.
- Check early: whether any microcode reaches VU1 that is not one of the ELF's
  overlays (log every `MPG` in PCSX2 across several levels).
- **Gate**: Sly 1's title screen and first level draw; replayed GS dumps match
  PCSX2's software renderer within a stated per-pixel tolerance.

### Phase 7 - Playable Sly 1

- Window and pacing at 59.94 Hz fields; interlaced field handling; gamepad
  (`gilrs`) and keyboard mapping; saves through the memory card; settings file;
  pause on focus loss.
- **Gate**: the first episode is playable start to finish, with a save and
  reload, on a recorded replay that reproduces.

### Phase 8 - Sound and movies

- `ps2-spu2` produces sound: ADPCM decoding, ADSR, Gaussian interpolation,
  reverb, the mixer, `cpal` output with bounded latency, decoupled from the
  emulated clock by a resampling buffer.
- `ps2-mpeg`: PSS demultiplexing, MPEG-2 video, audio sync; the `libmpeg`
  hook now plays movies.
- **Gate**: every FMV plays in sync; sound effects and music match PCSX2
  recordings of the same replay by ear and by waveform comparison of isolated
  effects.

### Phase 9 - Sly 1 complete and fast

- Whole-game replays (recorded once, then regression tests with hash
  checkpoints); profiling (Tracy through `tracing`); hot functions replaced by
  verified native hooks only when profiles demand it; PGO for release builds.
- `ps2-gs-wgpu`: a hardware GS renderer with a texture cache and resolution
  scaling, verified against the software renderer on the same GS dumps.
- First enhancements behind options (widescreen, internal resolution).
- Windows build.
- **Gate**: Sly 1 completed on replay; the §9 budget met; options off leave
  RAM hashes unchanged.

### Phase 10 - Sly 2

- Hashes, discovery and bounds; the nine IOP modules linked into the ELF's
  `.irx` section, loaded from memory (`sceSifLoadModuleBuffer`); `spiop`
  recompiled like `989snd`; `lgaud`/`usbd` answered as "no device" (the
  microphone features degrade as on a console without a headset); the 21
  overlays translated; anticrack-like checks searched for; Sly-2-Recomp's notes (shadow counting, motion blur, clock,
  widescreen) used as a guide.
- **Gate**: Sly 2 completed on replay within budget.

### Phase 11 - Sly 3

- The same deltas plus its larger code and 29 overlays, the VU0 code known
  to be sensitive to float rules, and Sly 3's
  extra modes (offline multiplayer, the anaglyph 3D sections). No public
  decompilation or symbols exist: Sly 3 relies entirely on sly-rs's own
  analysis and on what Sly 2 taught.
- **Gate**: Sly 3 completed on replay within budget.

### Phase 12 - Enhancements

- Frame-rate options (interpolation or native, decided per game from how its
  logic is tied to the field rate), texture-pack loading compatible with
  PCSX2's naming, optional native renderers for specific VU1 programs where
  the generic path limits quality (Sly 1 has only two real VU1 programs).
- `sly-formats`: readers for the archives and level formats (Sly 1's WAC/WAL,
  Sly 2's descriptors and LZ, `SLY3.WAD`), needed only by tools, texture
  packs and native renderers, never to run the games.

## 7. Rust engineering practice

- **Toolchain**: stable, pinned in `rust-toolchain.toml`; edition 2024;
  `rust-version` declared; `resolver = "3"`.
- **Lints** in `[workspace.lints]`: `unsafe_code = "forbid"` (overridden only
  in the crates of §5.2), `missing_docs` on public items of `ps2-*` crates,
  clippy `all` denied, a curated subset of `pedantic` (casts, truncation,
  `needless_pass_by_value`, `redundant_closure`), `unwrap_used`/`expect_used`
  denied outside tests and generated code. Generated crates carry one
  `#![allow(...)]` header instead.
- **Errors**: `thiserror` enums in libraries, `anyhow` in `xtask` and the
  binary's start-up; no panics on bad input data (fuzzed parsers).
- **Logging and profiling**: `tracing` with one target per subsystem
  (`ee::syscall`, `iop::cdvd`, `gs::draw`, ...), `tracing-subscriber` with an
  environment filter, `tracing-tracy` behind a feature. No `println!`.
- **Configuration**: one `sly-rs.toml` (`serde` + `toml`), overridable by
  `clap` flags and a small, documented set of environment variables (rac-rs
  grew ~240; the lesson is to keep switches in typed config).
- **Data layout**: `#[repr(C)]` and `bytemuck` for anything mapped from guest
  memory; no unaligned references; integer newtypes for guest addresses
  (`GuestAddr(u32)`) so host and guest pointers cannot mix.
- **Dependencies**: few, mainstream, audited by `cargo deny` (licences,
  advisories, duplicates). Expected set: `object`, `sha1`, `memmap2`,
  `thiserror`, `anyhow`, `tracing`, `serde`, `toml`, `clap`, `wgpu`, `winit`,
  `cpal`, `gilrs`, `bytemuck`, `libm`; dev: `proptest`, `insta`, `divan` or
  `criterion`, `cargo-nextest`, `cargo-fuzz`.
- **Build profiles**: `release` with `lto = "thin"`, `codegen-units = 16`
  for hand-written crates and `opt-level = 3, codegen-units = 1` for
  generated crates via profile overrides; a `dev` profile that still
  optimises generated crates (`opt-level = 1`) so debug builds run at playable
  speed; `profiling` profile with debug info; `dist` profile (fat LTO, one
  codegen unit, stripped) for what is shipped. Linker: Rust's default `lld`
  on Linux.
- **Tests**: unit tests next to code; integration tests per crate;
  property tests for instruction semantics; snapshot tests for disassembly and
  generated code shape; fuzzing for every parser that sees disc data; tests that
  need a disc, and exhaustive ones, behind an `#[ignore]` tag run by
  `cargo xtask test-full`.
- **Documentation**: rustdoc on every public item of `ps2-*`; design
  decisions and findings as concepts in this bundle; one finding per surprise.
- **Commits**: small, each passing `cargo xtask ci`; conventional prefixes by
  crate (`ps2-isa: ...`).

## 8. Verification strategy

Six oracles, from cheapest to most expensive.

| Oracle | Checks | When |
| --- | --- | --- |
| Unit and property tests | instruction semantics, decoders, parsers, swizzles | every commit (CI) |
| Conformance programs (our own PS2 test ELFs), `ps2-float` | FPU, MMI, VU0/VU1 edge cases against results recorded on PCSX2 | every commit (results committed) |
| `ps2-oracle`, the Sly 1 decompilation | an independent interpreter for whole functions; source-level meaning of Sly 1 code when behaviour is in doubt | when a function disagrees |
| Interpreter vs. recompiled code | per-function replays and lockstep runs, equal writes and instruction counts | every regeneration (local, needs disc) |
| Replays with RAM-hash checkpoints | determinism and regressions across whole play sessions | before every merge to main (local) |
| PCSX2 | GS dumps for the rasteriser, savestates for game-state comparisons at checkpoints, RPC and `MPG` traces, audio recordings | when a layer is first built, and for any disagreement |

PCSX2 does not time the machine the way sly-rs does (its cycle model is not
"one instruction, one cycle"), so whole-RAM equality with PCSX2 is not
expected; comparisons with it are at the level of game state (positions,
flags, counters at the same frame of a replay), GS output and RPC traffic.
Whole-RAM equality is expected between sly-rs's own execution modes.

## 9. Performance budget

On the reference machine (Ryzen 9 9950X, Radeon RX 9070 XT, Linux). Targets,
to be revised with measurements from phase 4.

| Measure | Target |
| --- | --- |
| EE game logic per field (Sly 1) | ≤ 2 ms |
| Instructions run by the interpreter in steady play | ≤ 0.5% |
| Software GS at native resolution | ≤ 4 ms per field, multi-threaded |
| Hardware GS at 4K | ≥ 120 fields per second of GPU headroom |
| Start-up to the first logo | ≤ 2 s |
| Resident memory | ≤ 1 GiB |
| Regenerate + full release build | ≤ 10 min |
| Incremental build after a runtime change | ≤ 30 s (no generated crate rebuilt) |

## 10. Risks

| Risk | Likelihood | Impact | Mitigation |
| --- | --- | --- | --- |
| The GS (formats, blending, texture cache) takes longer than everything else | high | high | software rasteriser first against GS dumps; hardware renderer only after parity |
| VU1 timing and pipeline subtleties | medium | high | interpreter with pipeline model; every translation diffed against it |
| Sly relies on EE threads and timing the HLE scheduler gets wrong | medium | high | a real scheduler from phase 3; instruction-exact events; PCSX2 traces when behaviour differs |
| Build times of generated code | high | medium | stable ABI crate, chunking, change-only writes, measured from phase 4 |
| Function discovery without symbols (Sly 2, 3) | medium | medium | Ghidra export, discovery pass, interpreter fallback, interpreter-time metric |
| `spiop` is undocumented | medium | low | recompiled, not reimplemented; its RPC traffic is understood only as far as debugging needs, with PCSX2 traces |
| Float differences change physics | medium | medium | PCSX2 float semantics from day one, conformance programs, `ps2-float` as a second reference |
| Anticrack sabotage (Sly 1; unknown in 2 and 3) shows up as gameplay bugs, some saved to the card | medium | high | guest code bytes never modified (§3); a test that runs the four checks; search Sly 2 and 3 for similar checks |
| EE/IOP timing (989snd's tick, stream reads, RPC completion) | medium | medium | one instruction clock for both processors; IOP modules recompiled rather than imitated |
| Scope: three games, one person | high | high | Sly 1 first; every phase ends in a usable tool; Sly 2/3 only after Sly 1 is complete |
| Prior art moves faster (Sly-2-Recomp extends to Sly 1 or 3) | medium | medium | the differentiators are Rust, determinism, speed and one codebase; borrow its permissive notes; revisit O0 at each gate |
| Legal exposure | low-medium | high | the ground rules of §3 |

## 11. Open decisions

Each has a recommendation; none blocks phase 0 except O0 and O1, both decided
on 2026-11-02: **O0 (a)**, build sly-rs; **O1 GPL-3.0-or-later**.

| # | Decision | Options | Recommendation |
| --- | --- | --- | --- |
| O0 | Build sly-rs, given ProjectCane (Sly 1) and Sly-2-Recomp (Sly 2) | (a) build it as planned; (b) contribute to Sly-2-Recomp / NativeEE and extend it to Sly 1 and 3 in C++; (c) port only Sly 3 | **(a)**, if the aims include Rust, a lean deterministic runtime and all three games in one codebase; (b) is the fastest route to playing Sly 2 and 3 natively |
| O1 | Licence | GPL-3.0-or-later; MIT OR Apache-2.0 | **GPL-3.0-or-later**: it may take code from rac-rs (GPL-3.0) and PCSX2 (GPL-3.0-or-later) as well as from the MIT, Apache-2.0 and CC0 sources; a permissive licence would rule out copying from the GPL projects |
| O2 | How much rac-rs code to reuse | fork its `ee-core` and interpreter; reimplement using it as the design reference, porting selected pieces | **reimplement**, in the crate boundaries of §5.2 and with tests at each gate; port its float emulation, SPU2 reverb and MPEG-2 decoder where that saves weeks (needs O1 = GPL) |
| O3 | Function bounds for Sly 2 and 3 | own discovery pass only; plus a headless Ghidra export | **both**: Ghidra (a tool-time dependency only) cross-checked by our pass |
| O4 | Host mechanism for guest threads | host threads handing over a baton; stackful coroutines (`corosensei`) | **neither**, decided in phase 4: generated code hands control back to the machine instead ([finding](research/recompiled-control.md)) |
| O5 | Platforms | Linux; Linux then Windows; plus macOS | **Linux, then Windows** from phase 9; macOS untested |
| O6 | Order after Sly 1 | Sly 2 then 3; Sly 3 then 2 | **Sly 2 then 3**: Sly 3's engine descends from Sly 2's, and Sly 2 has public notes to check against |
| O7 | Repository hosting | private until a gate; public from phase 0 | **private until phase 4's gate**, then public, with the ground rules of §3 audited before the first push. Decided 2026-11-03: public after the audit (no game bytes or generated code in the history; commit emails rewritten to the GitHub no-reply address) |

## 12. What the user provides

- The three discs (in `oracle/`).
- For PCSX2 as an oracle: the user's own BIOS. Done on 2026-11-02: PCSX2
  2.9.152 is set up with the USA v2.20 BIOS (`ps2-0220a-20050620.bin`, SHA-1
  `48d0445d…`); copies of it and of a Japanese v2.00 BIOS
  (`ps2-0200j-20040614.bin`) are in `oracle/ps2-bios/`. The port itself
  needs no BIOS and never reads these files.
- Recorded play sessions for replays, once phase 7 makes the game playable.

[^disc-survey]: What the three discs contain: a survey ([research/disc-survey.md](research/disc-survey.md)).
[^rac-rs]: How rac-rs is built ([research/rac-rs.md](research/rac-rs.md)).
[^landscape]: The Sly and PS2 recompilation landscape ([research/landscape.md](research/landscape.md)).
