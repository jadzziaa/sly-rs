# sly-rs

A Rust static recompilation of the PlayStation 2 Sly Cooper trilogy, built
from your own discs:

- Sly Cooper and the Thievius Raccoonus (USA, SCUS-97198)
- Sly 2: Band of Thieves (USA, SCUS-97316)
- Sly 3: Honor Among Thieves (USA, SCUS-97464)

The game's own code is translated into Rust ahead of time. A Rust runtime
stands in for the PS2's kernel, IOP, graphics, sound and movie hardware, and
all game data is read from your disc at run time.

**Status: phase 0 of 12 (foundations).** Nothing playable yet. The plan,
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

`sly-rs info` identifies each disc against the supported builds. It then
prints the disc's files, the executable's sections, its VU microcode overlays
and its IOP modules. `--verify` also hashes the whole image.

## Development

```bash
cargo xtask ci           # format, clippy, rustdoc, tests, cargo deny, dependency rules
cargo xtask test-discs   # the tests that need your discs
```

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
