---
type: Finding
title: "Function discovery in the three executables"
description: How ps2-analysis finds functions, jump tables and kernel stubs in the Sly executables from the code alone, measured against the Sly 1 decompilation (the phase 2 gate), two other decompilations' function lists, and rabbitizer - with every remaining disagreement explained.
tags: [finding, analysis, functions, jump-tables, phase-2]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-02T21:55:00Z }
verified:
  - { by: claude-code/claude-opus-5-5, at: 2026-11-02T21:55:00Z }
sources:
  - id: tests
    resource: "crates/sly-verify/tests/analysis.rs and ee_disassembly.rs, run with cargo xtask test-full on 2026-11-02"
    title: "The phase 2 gate tests"
  - id: toz
    resource: https://github.com/TheOnlyZac/sly1
    title: "TheOnlyZac/sly1 config/symbol_addrs.txt (CC0), imported as games/sly1/symbols.tsv"
  - id: openthievius
    resource: https://github.com/OpenThievius/sly1-decomp
    title: "OpenThievius/sly1-decomp config/us/functions.tsv (AGPL-3.0; compared locally, not stored)"
  - id: komasia
    resource: https://github.com/Komasia/sly2-decomp
    title: "Komasia/sly2-decomp config/functions.csv (no licence; compared locally, not stored)"
---

# Function discovery in the three executables

Measured on 2026-11-02 with `ps2-analysis` running blind (no symbols), as
`sly-rs analyze` reports it and the gate tests assert it.[^tests]

## Method

1. **Jump tables first.** Every `jr` not on `$ra` is evaluated backwards over
   32 instructions: the register it jumps through must come from a load whose
   base is a constant (the table) plus one unknown (the scaled index). The
   nearest `sltiu` bounds the table; without one, entries are read while they
   look like nearby code.
2. **Strong starts.** The entry point, every `jal` target, and code addresses
   in data or built with `lui`/`addiu`. A pointer counts only when it can
   start a function: 8-byte aligned, after `nop` padding or a return, jump or
   `eret` (data integers fall into `.text` by chance), and not a case label.
3. **Linear sweep.** A function runs until a return, `j` (a tail call:
   GCC jumps within a function with `b`), unconditional backward branch or
   `eret` that no pending forward branch or `switch` case reaches past.
   `nop`s between functions are padding. A pointer start inside a function
   (a branch passes over it) is ignored.
4. **Orphans.** Sweep-found code that only branches back into the function
   before it is merged into that function (blocks GCC left after the last
   return).

## Results

| | Sly 1 | Sly 2 | Sly 3 |
| --- | ---: | ---: | ---: |
| Functions | 4,806 | 6,827 | 8,938 |
| from `jal` / pointers / sweep | 2,688 / 1,903 / 214 | 3,863 / 2,679 / 284 | 5,205 / 3,417 / 315 |
| `.text` in functions / padding / data (bytes) | 1,112,960 / 8,828 / 16 | 1,402,964 / 13,708 / 16 | 1,816,644 / 18,940 / 16 |
| Jump tables (bounded) | 196 (194) | 266 (265) | 379 (379) |
| `jr` without a table | 3 | 3 | 3 |
| Basic blocks | 54,390 | 72,713 | 96,846 |
| Branch targets in delay slots | 1 | 1 | 1 |
| Syscall stubs (named) | 80 (77) | 151 (141) | 151 (141) |

The unresolved `jr`s are C-library `longjmp`/unwinding code jumping through
saved registers, not `switch`es. Unnamed syscall numbers are reserved ones
(0x0, 0x3, 0x9, 0x3B, 0x3F, 0x49, 0xFD, -0x53, -0x5A, -0xFE): phase 3 logs
them.

## Against the references

- **TheOnlyZac/sly1** (the gate's reference, 3,949 named starts):[^toz] 3,947
  found (99.95%). The two misses, `0x193f88` and `0x193fb8`, are case blocks
  of the jump table at `0x193f80` that the decompilation's Ghidra pass split
  off as `FUN_` functions. They use the switch's stack frame and branch to its
  epilogue. The gate asserts every miss is a recovered case label.
- **OpenThievius** (Sly 1, 4,770 starts; compared locally, not
  stored):[^openthievius] 4,769 agree. Its extra start is the two `nop`s
  before `_start`, which discovery counts as padding.
  - Ours has 37 starts it lacks. 21 of them are named functions in
    TheOnlyZac's list.
  - The rest are prologue-bearing functions after `noreturn` calls or
    `longjmp`, and a few two-instruction dead tails after tail calls.
- **Komasia** (Sly 2, 6,816 starts; compared locally, not stored):[^komasia]
  6,811 agree.
- **rabbitizer**: our classification of calls, returns and jump-table jumps
  matches its own on every valid word of the three executables.

## Naming

Exact fingerprints (code hashed with addresses masked) carry only 64 of Sly
1's names to Sly 2 and 47 to Sly 3, among them `memcmp`, `sqrtf`,
`scePadGetState` and `CTextBox::SetTextColor`. The SDK moved from 2.4 to 3.0
and the engine from "P2" to "P3"/"P4", so little code is byte-identical.

[^tests]: The phase 2 gate tests.
[^toz]: TheOnlyZac/sly1.
[^openthievius]: OpenThievius/sly1-decomp.
[^komasia]: Komasia/sly2-decomp.
