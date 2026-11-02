# Notices and credits

sly-rs is licensed under GPL-3.0-or-later. It contains no code or data from
the Sly Cooper games; Sly Cooper and the games' content belong to Sony
Interactive Entertainment and Sucker Punch Productions.

## Work this project builds on

Each entry says how it is used. Entries are added when their material
enters the repository, not before.

| Project | Licence | Use |
| --- | --- | --- |
| [AsobiCodes/rac-rs](https://github.com/AsobiCodes/rac-rs) | GPL-3.0 | Design reference for the recompiler and runtime architecture (no code copied yet) |
| [TheOnlyZac/sly1](https://github.com/TheOnlyZac/sly1) | CC0-1.0 | Planned (phase 2): Sly 1 function addresses and names |
| [menaman123/NativeEE](https://github.com/menaman123/NativeEE), [Sly-2-Recomp](https://github.com/menaman123/Sly-2-Recomp) | MIT, Apache-2.0 | Reference for Sly 2 internals (no code copied yet) |
| [PCSX2](https://github.com/PCSX2/pcsx2) | GPL-3.0-or-later | Reference for hardware behaviour; oracle for tests. Its VU opcode tables (`pcsx2/DebugTools/DisVUmicro.h`) are transcribed as test data in `crates/ps2-isa/src/vu/tests.rs` |
| [Decompollaborate/rabbitizer](https://github.com/Decompollaborate/rabbitizer) | MIT | Test-only dependency: the independent R5900 decoder `ps2-isa` is checked against |

Projects read for facts only, never copied: ProjectCane and
Komasia/sly2-decomp (no licence), OpenThievius/sly1-decomp (AGPL-3.0).
The full survey is in [docs/research/landscape.md](docs/research/landscape.md).
