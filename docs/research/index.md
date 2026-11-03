<!-- okf index: generated; edit freely, or rerun `okf index` -->

# Finding

* [The conformance program against PCSX2](conformance.md) - Our own PS2 test program (FPU, integer, MMI, unaligned loads and stores, VU0 macro mode) run in the interpreter and in PCSX2's recompiler and interpreter - which differences were our bugs and fixed, which are PCSX2's, and why PCSX2 cannot settle the float and VU0 questions.
* [Function discovery in the three executables](function-discovery.md) - How ps2-analysis finds functions, jump tables and kernel stubs in the Sly executables from the code alone, measured against the Sly 1 decompilation (the phase 2 gate), two other decompilations' function lists, and rabbitizer - with every remaining disagreement explained.
* [Recompiled code hands control back instead of switching stacks](recompiled-control.md) - Why sly-rs's generated code needs neither host threads nor coroutines for EE threads - the guest's stack lives in guest memory, so compiled code can always return to the machine and resume at any block - and the invariants that keep recompiled execution identical to the interpreter's, to the cycle.
* [How Sly 1 boots in the interpreter](sly1-boot.md) - The start-up sequence of Sly 1 (SCUS-97198) as sly-rs runs it - kernel patches, threads, the SIF handshake, the IOP reboot, CD/DVD and module-loader RPC, the sound driver on the emulated IOP, DMA and GS events, the pad, the memory card check, the title and a new game - up to its first level.
* [How Sly 1's graphics reach the GS in sly-rs](sly1-graphics.md) - The path Sly 1's frames take through DMA, the scratchpad, VIF0/VIF1, VU0/VU1 and the GS as sly-rs models it, and the four missing pieces that each stopped the first level from drawing. (draft)

# Research Note

* [What the three discs contain: a survey](disc-survey.md) - Measured facts about the user's three Sly Cooper USA disc images - files, executables, sections, instruction mix, VU microcode overlays, IOP modules and where the game data hides - that size and shape the recompilation plan.
* [The Sly and PS2 recompilation landscape](landscape.md) - Prior art for porting the Sly trilogy (decompilations, a hand-written Sly 1 port, an existing C++ static recompilation of Sly 2), what is known about the Sly engine (disc layout, compression, scripting, rendering, audio, anticrack), PCSX2's quirks for these games, the PS2 recompilation ecosystem and Rust building blocks, and hardware references - as of 2026-11-02.
* [How rac-rs is built, and what carries over to Sly](rac-rs.md) - The architecture of rac-rs (Rust static recompilation of PS2 Ratchet & Clank, v0.4.3) - workspace, the recompilation pipeline, runtime and HLE, function replacement, verification, asset handling and hard-won lessons - with an assessment of which parts are generic PS2 infrastructure and which are Ratchet-specific.
