---
type: Finding
title: Recompiled code hands control back instead of switching stacks
description: Why sly-rs's generated code needs neither host threads nor coroutines for EE threads - the guest's stack lives in guest memory, so compiled code can always return to the machine and resume at any block - and the invariants that keep recompiled execution identical to the interpreter's, to the cycle.
tags: [finding, recompiler, phase-4, threads, determinism, design]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-02T23:57:09Z }
sources:
  - id: rec
    resource: crates/ps2-core/src/rec.rs
    title: Ctx, the table and exits
  - id: codegen
    resource: crates/ps2-codegen/src/lower.rs
    title: Lowering of functions, blocks and instructions
  - id: dispatcher
    resource: crates/ps2-ee/src/lib.rs
    title: Machine::run and run_compiled
  - id: plan
    resource: ../plan.md
    title: The plan, §5.3 (guest stack, EE threads) and open decision O4
  - id: test
    resource: conformance/rec/tests/recompiled.rs
    title: The recompiled conformance program in lockstep with the interpreter
verified:
  - { by: claude-code/claude-opus-5-5, at: 2026-11-02T23:57:09Z }
---

# Recompiled code hands control back instead of switching stacks

## The problem

The plan put the guest call stack on the host stack: a guest `jal` becomes a
Rust call. Sly runs real work on several EE threads, and the kernel switches
threads inside system calls and interrupts. If a switch happened while
compiled frames were on the host stack, those frames would belong to the
thread being switched out. The plan therefore expected one host stack per
guest thread, from host threads or stackful coroutines (decision O4), and a
kernel `Context` able to hold a parked host thread.[^plan]

## Why it is not needed

A compiled frame holds nothing the guest needs: registers live in `Cpu`, and
the guest's own stack, return addresses included, lives in guest memory. A
host frame only remembers *where in Rust* to continue. So:

- Generated code can give control back to the machine at any instruction
  boundary: it sets `cpu.pc` and `cpu.cycles` to where the interpreter would
  stand, sets `Ctx::exiting`, and returns. Every frame returns in turn.[^rec]
- The machine can resume at any block start, because each generated function
  is a loop over a `match` on the block address: it can start at any of its
  blocks.[^codegen]
- After a call, a frame continues only if the callee returned normally and
  `cpu.pc` is the frame's return address. Otherwise it returns too.
- When a frame's guest caller is no longer on the host stack, its return
  goes to the machine. The machine starts the caller's function at the return
  block. The guest call stack is rebuilt one level at a time, from guest
  memory.

System calls, interrupts and thread switches all happen in the machine, on an
empty host stack. A switch is the kernel's saved-register `Context` swap, the
same as under the interpreter. O4 is settled as neither option: there are no
host threads and no coroutines, and no `unsafe`.

## The invariants

1. **No guest state in host frames.** Generated code keeps only the next
   block address in a local. Every register access goes through `Cpu`.
2. **Every exit reaches the machine.** Exits set `exiting`, and call sites
   test it as well as the return address. Under recursion an exit can land
   on an outer frame's return block, so the address test alone would let
   that frame run on past a due interrupt.
3. **Blocks fit before the next event.** A block counts its instructions on
   entry and starts only if it ends by the deadline: the next event, or the
   end of the run. The interpreter would have done nothing between its
   instructions, so events and interrupts land at the same counts.
4. **Exits wherever the interpreter could act.**
   - *Before* instructions the machine runs itself: system calls, `break`,
     `eret`, VU calls, traps that fire, and anything the shared semantics
     report as not "next".
   - *After* a hardware register write that leaves an enabled interrupt
     pending, after `ei`, and after a write to COP0 Status.
5. **The clock is exact where it is read.** Slow memory paths and
   `mfc0 Count` subtract the block's remaining instructions. The
   interpreter's clock counts the instruction running, delay slots
   included.

## Measured

- The conformance program (with `flow.c`: branch-likely, a branch into a
  delay slot, `bgezal`, a jump table, calls through pointers, recursion past
  the 512-call host depth) runs recompiled. It prints the expected output,
  and its registers and RAM equal the interpreter's every 10,000 cycles.
  99.998% of its instructions run compiled.[^test]
- Sly 1 (the phase 4 gate): 300 fields recompiled and interpreted side by
  side, equal at every field (registers, RAM, scratchpad); 99.958% of
  instructions compiled. The rest are almost all system call instructions,
  which the machine runs itself.[^dispatcher] See
  [the plan's phase 4 status](../plan.md).

[^plan]: The plan, §5.3 (guest stack, EE threads) and open decision O4.
[^rec]: Ctx, the table and exits.
[^codegen]: Lowering of functions, blocks and instructions.
[^test]: The recompiled conformance program in lockstep with the interpreter.
[^dispatcher]: Machine::run and run_compiled.
