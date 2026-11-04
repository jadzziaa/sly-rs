<!-- okf index: generated; edit freely, or rerun `okf index` -->

# Finding

* [The conformance program against PCSX2](conformance.md) - Our own PS2 test program (FPU, integer, MMI, unaligned loads and stores, VU0 macro mode) run in the interpreter and in PCSX2's recompiler and interpreter - which differences were our bugs and fixed, which are PCSX2's, and why PCSX2 cannot settle the float and VU0 questions.
* [Function discovery in the three executables](function-discovery.md) - How ps2-analysis finds functions, jump tables and kernel stubs in the Sly executables from the code alone, measured against the Sly 1 decompilation (the phase 2 gate), two other decompilations' function lists, and rabbitizer - with every remaining disagreement explained.
* [Recompiled code hands control back instead of switching stacks](recompiled-control.md) - Why sly-rs's generated code needs neither host threads nor coroutines for EE threads - the guest's stack lives in guest memory, so compiled code can always return to the machine and resume at any block - and the invariants that keep recompiled execution identical to the interpreter's, to the cycle.
* [How Sly 1 boots in the interpreter](sly1-boot.md) - The start-up sequence of Sly 1 (SCUS-97198) as sly-rs runs it - kernel patches, threads, the SIF handshake, the IOP reboot, CD/DVD and module-loader RPC, the sound driver on the emulated IOP, DMA and GS events, the pad, the memory card check, the title and a new game - up to its first level.
* [How Sly 1's graphics reach the GS in sly-rs](sly1-graphics.md) - The path Sly 1's frames take through DMA, the scratchpad, VIF0/VIF1, VU0/VU1 and the GS as sly-rs models it, and the four missing pieces that each stopped the first level from drawing. (draft)
* [VU microcode translated ahead of time](vu-translation.md) - How sly-rs translates the VU microcode overlays into Rust at recompile time, each pair as its own statements with the VU's registers, pipeline and flags in locals, how the translations are matched against what the game uploads and checked against the interpreter bit for bit, and what they gain on Sly 1.

# Proposal

* [A GPU GS on wgpu compute shaders](gpu-gs.md) - The design for sly-rs's GPU renderer - the GS's local memory in a GPU buffer and the pixel pipeline in compute shaders, as paraLLEl-GS does, sharing one integer arithmetic with the software GS so that the two can be compared bit for bit on GS dumps - with the facts checked, the milestones and their gates, and the decision it needs.
* [A specialising VU1 translator](vu-specialising-translator.md) - Why VU1 still costs 7.8 ns a pair after the float switch, and the design of a translator that emits per-pair code with registers in locals and flag timing resolved at generation time (as rac-rs does), with its budget, steps and checks. (deprecated)

# Research Note

* [What the three discs contain: a survey](disc-survey.md) - Measured facts about the user's three Sly Cooper USA disc images - files, executables, sections, instruction mix, VU microcode overlays, IOP modules and where the game data hides - that size and shape the recompilation plan.
* [The Sly and PS2 recompilation landscape](landscape.md) - Prior art for porting the Sly trilogy (decompilations, a hand-written Sly 1 port, an existing C++ static recompilation of Sly 2), what is known about the Sly engine (disc layout, compression, scripting, rendering, audio, anticrack), PCSX2's quirks for these games, the PS2 recompilation ecosystem and Rust building blocks, and hardware references - as of 2026-11-02.
* [How rac-rs is built, and what carries over to Sly](rac-rs.md) - The architecture of rac-rs (Rust static recompilation of PS2 Ratchet & Clank, v0.4.3) - workspace, the recompilation pipeline, runtime and HLE, function replacement, verification, asset handling and hard-won lessons - with an assessment of which parts are generic PS2 infrastructure and which are Ratchet-specific.
