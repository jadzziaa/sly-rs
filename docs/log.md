# Update log

## 2026-11-02
* **Update**: Hosted CI: GitHub Actions runs `cargo xtask ci` on pushes to main and pull requests ([plan](plan.md), phase 0 status).
* **Update**: Decided O7: the repository goes public after auditing its history against the [ground rules](plan.md); NOTICE.md credits ps2sdk and the ps2dev toolchain.
* **Update**: [Phase 4](plan.md) built and its gate passed: Sly 1 runs recompiled in lockstep with the interpreter for 300 fields, 99.958% compiled. [Why the generated code needs no stack switching](research/recompiled-control.md) for EE threads (O4 settled as neither option).
* **Update**: Conformance: [our test program](research/conformance.md) run in the interpreter and, headless, in PCSX2's recompiler and interpreter; three MMI bugs fixed; PCSX2 found to be no reference for float and VU0 rules, which stay unverified. [Plan](plan.md) phase 3 status updated.
* **Update**: [Phase 3](plan.md) gate passed: the interpreter, kernel HLE and IOP side boot Sly 1 to its sound driver deterministically; [the boot sequence](research/sly1-boot.md) recorded. Conformance programs outstanding.
* **Update**: [Phase 2](plan.md) built and its gate passed: ps2-analysis finds 99.95% of the Sly 1 decompilation's function starts blind, every miss explained; [function discovery](research/function-discovery.md) records the method and the measurements.
* **Update**: [Phase 1](plan.md) built and its gate passed: ps2-isa (R5900 and VU micro decoding, disassembly), checked exhaustively against rabbitizer and against PCSX2's VU tables; [disc survey](research/disc-survey.md) corrected: the VU microcode is in .vutext, not in the .DVP.overlay sections.
* **Update**: Decided O0 (build sly-rs) and O1 (GPL-3.0-or-later); recorded the user's BIOS setup; [phase 0](plan.md) built and its gate passed: ps2-disc, sly-common, sly-rs info, xtask.
* **Creation**: The working [plan](plan.md) (draft) for a Rust static recompilation of the Sly trilogy, with three research notes behind it: [what the three discs contain](research/disc-survey.md), [how rac-rs is built](research/rac-rs.md) and [the Sly and PS2 recompilation landscape](research/landscape.md).
* **Initialization**: Started the bundle (OKF v0.2).
