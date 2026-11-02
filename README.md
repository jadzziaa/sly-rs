# sly-rs

A Rust static recompilation of the PlayStation 2 Sly Cooper trilogy, built
from your own discs:

- Sly Cooper and the Thievius Raccoonus (USA, SCUS-97198)
- Sly 2: Band of Thieves (USA, SCUS-97316)
- Sly 3: Honor Among Thieves (USA, SCUS-97464)

The game's own code is translated into Rust ahead of time. A Rust runtime
stands in for the PS2's kernel, IOP, graphics, sound and movie hardware, and
all game data is read from your disc at run time.

**Status: phase 3 of 12 (interpreter) at its gate: Sly 1 boots to its sound driver.** Nothing playable yet. The plan,
its phases and the research behind it are in [docs/](docs/index.md), starting
with [docs/plan.md](docs/plan.md).

## No game content here

This repository contains no code or data from the games, no BIOS and no
code generated from the games. You need your own discs. Generated code is
written to `generated/` and your disc images stay wherever you keep them;
both are ignored by git.

## Building

You need Rust (the version is pinned in `rust-toolchain.toml`; rustup installs
it automatically).

```bash
cp sly-rs.example.toml sly-rs.toml   # then point it at your disc images
cargo run --release -p sly-app -- info
```

Commands so far:
- `sly-rs info` identifies each disc against the supported builds and prints
  its files, the executable's sections, VU microcode overlays and IOP modules.
  `--verify` also hashes the whole image.
- `sly-rs analyze` reports the functions, jump tables, kernel call stubs and
  VU overlays found in each executable. `--functions` lists the functions.
- `sly-rs disasm sly1 1f6c90 40` disassembles from an address, marking the
  functions found and their known names.
- `sly-rs run sly1 --fields 600` boots the game in the interpreter, headless,
  and reports kernel calls, threads, IOP traffic and the hottest functions;
  `--watch 1e1db8` records the arguments at an address.

## Development

```bash
cargo xtask ci           # format, clippy, rustdoc, tests, cargo deny, dependency rules
cargo xtask test-full    # the tests that need your discs, and the exhaustive ones
cargo xtask conformance-build   # rebuild conformance/conformance.elf (podman, ps2dev image)
cargo xtask conformance-record  # PCSX2's output for it, headless in gamescope
```

The conformance program is our own PS2 test executable. After changing it,
rebuild and re-record it, then review `sly-rs elf conformance/conformance.elf`
before writing that output to `conformance/expected.txt`. The test holds the
interpreter to that file ([docs/research/conformance.md](docs/research/conformance.md)).

`cargo xtask ci` runs `cargo-nextest` and `cargo-deny` when they are
installed, and skips them otherwise.

Workspace layout and rules are in [docs/plan.md](docs/plan.md) §5 and §7.
In short:

- `ps2-*` crates are game-agnostic and never depend on `sly-*` crates.
- `unsafe` is forbidden outside the crates that need it.
- Every phase ends in a measurable gate.

## Licence

GPL-3.0-or-later; see [LICENSE](LICENSE). Credits for the community work this
builds on are in [NOTICE.md](NOTICE.md).
