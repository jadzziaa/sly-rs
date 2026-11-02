---
type: Research Note
title: "How rac-rs is built, and what carries over to Sly"
description: The architecture of rac-rs (Rust static recompilation of PS2 Ratchet & Clank, v0.4.3) - workspace, the recompilation pipeline, runtime and HLE, function replacement, verification, asset handling and hard-won lessons - with an assessment of which parts are generic PS2 infrastructure and which are Ratchet-specific.
tags: [research, prior-art, rac-rs, static-recompilation, rust, ps2]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-02T20:20:00Z }
stale_after: 2027-02-01T00:00:00Z
sources:
  - id: rac-rs
    resource: https://github.com/AsobiCodes/rac-rs
    title: "AsobiCodes/rac-rs at v0.4.3 (commit 5b14154, 2026-10-31), read from a local clone without building or running it"
  - id: rac-arch
    resource: https://github.com/AsobiCodes/rac-rs/blob/main/ARCHITECTURE.md.bak
    title: "rac-rs ARCHITECTURE.md.bak (copied from the private development tree, dated 2026-10-21, partly out of date)"
  - id: project-cane
    resource: https://www.generationamiga.com/2026/08/31/sly-cooper-pc-port-is-taking-shape-as-project-cane-shows-new-gameplay/
    title: "Generation Amiga: Sly Cooper PC port is taking shape as Project Cane shows new gameplay (2026-08-31)"
---

# How rac-rs is built, and what carries over to Sly

Read from a clone of v0.4.3 without building or running it; four claims the
plan rests on (licence, generators run at setup, no EE thread scheduler, a
Ghidra-made function list) were re-checked in the source.[^rac-rs] Its design
document is useful but partly stale.[^rac-arch]

## 1. In one paragraph

rac-rs is **not one recompiled binary**. Install-time Python generators turn
the user's disc into ~430-460 generated Rust crates that run alongside a Rust
**R5900 interpreter**; anything the generated code cannot or may not run falls
back to the interpreter with the exact instruction count, so both produce the
same RAM. Around that sits a large native PC layer: a **"mirror renderer"**
that reads the game's object state from RAM every field and draws the 3D world
itself in wgpu from level files decoded at run time, with GS emulation kept
for the HUD, menus and VU1 effects. Licence: **GPL-3.0** (because it follows
PCSX2's behaviour closely).

## 2. Workspace

| Crate | Role |
| --- | --- |
| `ee-core` (no deps) | Stable interface for generated code: `#[repr(C)]` views of the CPU and memory, inlined register/MMI/FPU/VU0-macro operations and the memory fast path, `extern "Rust"` declarations of slow paths. Changing the interpreter does not rebuild the generated crates. |
| `ee-oracle` | Interpreter, hardware model, HLE kernel, BIOS-free cold boot, dispatcher for recompiled code, VU0, VIF0, GS capture, SPU2 voices, a Rust port of Sony's `989snd.irx`, memory-card server. |
| `rec-*` (generated, ~12,000 instructions each) | One crate per chunk of game code; depend only on `ee-core`. |
| `ee-game` (generated) | Registers every generated crate's function table. |
| `rac-rs`, `rac-rs/exe` | The port: renderer, audio, input, RAM readers, PC features, mods, movies, optional software GS. |

Graphics and windowing `wgpu` 30 + WGSL + `winit` 0.30; audio `cpal`; also
`rayon`, `bytemuck`, `gilrs` (Linux pads). No logging or error crate (stderr,
its own run log, `Result<_, String>`), ~240 `RAC_*` environment switches.
Release builds of generated crates use `opt-level = 3, codegen-units = 1`;
Windows adds PGO from checked-in profiles; no LTO.

## 3. The recompilation pipeline

- **When**: `setup.sh` runs `tools/recgen_iso.py` per level, a deduplication
  pass, then `vugen_iso.py` for VU microprograms, then `cargo build`. Nothing
  generated is committed and there is no `build.rs` generation. A full build is
  ~12 minutes on 24 threads; generated crates are ~97% of the compile work.
- **Input**: the ISO is parsed directly, its size and SHA-1 checked against a
  table; boot ELF segments and level overlays are placed into one flat 32 MB
  image per level.
- **Function discovery is an input, not a pass**: `tools/fn_bounds.tsv`,
  36,962 `entry end level` rows from an offline Ghidra pass. A hole-scan adds
  regions that decode cleanly and end in `jr`/`j`/`eret`, and every word that
  looks like a code pointer becomes a possible block label (one missed computed
  target once sent 25% of a level's EE time to the interpreter).
- **Lifting straight to Rust source, no IR**: one statement per instruction on
  `c: &mut Cpu`; GPRs `[u128; 32]`; loads and stores take a range-checked fast
  path to RAM or scratchpad and fall back to the interpreter; rare MMI/COP2
  forms call the interpreter's own decoder, so semantics match by construction.
- **Control flow**: dominator analysis emits labelled `loop`/block structure
  ("Beyond Relooper"); irreducible functions fall back to
  `loop { match c.pc { ... } }`. Each block begins with an instruction-budget
  check so interrupts and DMA events land at the exact instruction they would
  under interpretation.
- **Calls**: same-crate `jal` is a Rust call; `jalr` and everything else go
  through `call_dyn` (PC table lookup, interpreter if absent). `jr ra` is a
  `return`; other `jr` try the GCC switch-table pattern. The guest call stack
  is the host stack (game thread runs on a 256 MB stack).
- **Overlays and self-modifying code**: every generated function carries its
  code hash; a crate is used only if every function in it matches RAM, checked
  again on the `FlushCache` syscall. Stale frames unwind with
  `resume_unwind`.
- **Floating point**: PCSX2 semantics (round toward zero, denormals flushed,
  Inf/NaN as ±FLT_MAX, the missing guard bits of add/sub), implemented without
  switching MXCSR per instruction; a guard wraps runs of COP2 ops.
- **VU0**: macro mode inlined, with unused flag results skipped at generation
  time; micro mode interpreted with a pipeline model plus three ahead-of-time
  translations used only when the program bytes match.

## 4. Runtime and high-level emulation

- **Memory**: plain `Vec<u8>`s (32 MB RAM, 16 KB scratchpad, VU memories), no
  host virtual-memory "fastmem". One instruction = one cycle.
- **Kernel**: BIOS-free cold boot with an HLE kernel handling syscalls by
  number and INTC/DMAC handler lists; a hook file skips BIOS scans.
- **EE threads are not scheduled**: `StartThread` is logged and not run,
  `WaitSema` always succeeds. Ratchet works single-threaded on the EE.
- **IOP is not emulated**: each IOP service is answered at the SIF RPC level
  (CD reads straight from the ISO, a Rust port of `989snd.irx` emitting SPU2
  register writes replayed by a 48-voice mixer, a memory-card server on a
  PCSX2 `.ps2` image, pad state written by the host).
- **DMA**: most channels complete at once; the VIF1 chain is walked and
  completes after a delay so the game's interrupt handler sees the right
  stops.
- **Graphics**: the mirror renderer for the 3D world (depends on documented
  level formats, from Wrench and rac1-decomp); GS register model plus a
  software rasteriser for HUD and menus; a persistent VIF1 + VU1 model for VU1
  effects; an optional full software GS (`gsfull`: VIF1, VU1 interpreter and
  ahead-of-time VU1 translations, all three GIF paths, swizzled GS memory).
- **Movies**: no IPU emulation; the game's movie player is hooked and PSS
  files are played by a pure-Rust MPEG-2 decoder.
- **Threads on the host**: the EE runs on a worker up to three fields ahead of
  the display; winit/wgpu on the main thread; capture, VU1 effects, level
  prefetch and present on their own threads.

## 5. Replacing game code and verifying it

Five mechanisms, from interpreter stubs by address to `Hand`s (native Rust
replacements checked against the function's code hash, able to decline and
fall back) and generated "speed hands" (exact native rewrites of hot functions
that deoptimise on anything unusual, saving ~9% of game-logic CPU).

**Determinism is the organising principle**: compiled code, hands,
ahead-of-time VU code and the interpreter yield the same instruction count and
therefore the same RAM. Verification in the repository: a gate that runs from
PCSX2 savestate A to savestate B's frame counter and diffs RAM; feeding
recorded PCSX2 playthroughs (pad + RAM writes) field by field with RAM hashes
every 50 fields; side-by-side diff modes for native fast paths; a pad
record/replay format; a deterministic test clock; ~480 unit tests. Rule: PC
extras switched off must leave the RAM hashes untouched.

## 6. Assets and legal posture

- The user's ISO goes in `rom/`; data, audio and movies are read from it at
  run time; nothing is extracted. Generated code is never committed.
- Committed: addresses, hashes and structure (`fn_bounds*.tsv`, stub lists,
  disc maps, PGO profiles).
- Exception worth noting: ~63k lines of checked-in, instruction-level Rust
  translations of retail functions (the speed hands) and an older hand-port
  of game logic. sly-rs should not follow it there ([plan §3](../plan.md)).

## 7. Lessons worth taking

- Switching MXCSR per float op stalled the pipeline: emulate the rounding with
  ordinary SSE arithmetic instead.
- Cross-OS determinism broke on last-bit differences in `sinf`: use a pinned
  pure-Rust `libm` for every transcendental the game or a hook computes.
- Missed computed jump targets are the most expensive recompiler bug; treat
  "time spent in the interpreter" as a first-class metric.
- Compile time is a design constraint: ~12k instructions per crate,
  out-of-line memory helpers (half the LLVM work), shared cold slow paths,
  deduplicated templates, a stable `ee-core` ABI crate so generated crates
  never rebuild for runtime changes, preserved file mtimes.
- Functions carrying hooks are left to the interpreter, so a compiled call can
  never skip a hook.
- The Windows default heap degraded after level switches (2.7 → 7.7 ms per
  frame); the segment heap fixed it.

## 8. What carries over to Sly

**Generic (the ideas, and with a GPL-3.0 licence the code too)**: the
`ee-core`/interpreter split and its stable ABI; the interpreter; the
translator core (instruction lowering, structuring, hole discovery, switch
recovery, crate chunking); the hash-validated dispatcher with `poll`, `slow`,
`call_dyn` and `Hand`; ahead-of-time VU0/VU1 translation with interpreter
fallback; the full software GS; the SPU2 mixer and reverb; the MPEG-2 decoder;
the 989snd port (Sly uses 989snd too: [disc survey §5](disc-survey.md)); the
memory-card server; and the whole verification method.

**Ratchet-specific**: the disc and overlay loader, the function-bounds list,
stub and hook tables, RPC server IDs, every RAM address, the speed hands, and
the entire mirror renderer.

**Where Sly differs and the plan must not copy blindly**:

- **Threads.** Sly 2 and 3 strings name `SceKernelDelayThread`,
  `SceCdCallbackThread` and semaphores; if Sly runs real work on more than one
  EE thread, the "no scheduler" shortcut is wrong and an HLE scheduler is
  needed.
- **Renderer.** The mirror renderer depended on Wrench and rac1-decomp
  documenting the level formats. For Sly the generic path (VIF1 + VU1
  translation + a GS renderer) is the foundation; a mirror renderer is an
  optional later layer.
- **Function bounds.** rac-rs consumed a Ghidra list; sly-rs needs its own
  (from the Sly 1 decomp where it matches the retail build, otherwise from its
  own discovery pass).

A fan project named "Project Cane", a Sly Cooper PC port, was reported in
August 2026; whether it uses static recompilation is unconfirmed.[^project-cane]

[^rac-rs]: AsobiCodes/rac-rs at v0.4.3, local clone.
[^rac-arch]: rac-rs ARCHITECTURE.md.bak.
[^project-cane]: Generation Amiga, 2026-08-31.
