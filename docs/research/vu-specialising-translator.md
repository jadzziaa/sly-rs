---
type: Proposal
title: A specialising VU1 translator
description: Why VU1 still costs 7.8 ns a pair after the float switch, and the design of a translator that emits per-pair code with registers in locals and flag timing resolved at generation time (as rac-rs does), with its budget, steps and checks.
tags: [proposal, vu, translation, performance, phase-7]
status: deprecated
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-03T23:07:08Z }
stale_after: 2027-02-01T00:00:00Z
sources:
  - id: measurements
    resource: vu-bench --translated-only on 6,000 recorded gameplay programs; callgrind on an x86-64-v3 build; the user's prologue recording, 2026-11-03
    title: VU1 measurements in gameplay
  - id: rac-rs
    resource: https://github.com/AsobiCodes/rac-rs (tools/vu/vu1gen.py, rac-rs/src/gsfull/vu1aot.rs), read from a local clone
    title: rac-rs's VU1 generator
---

# A specialising VU1 translator

**Carried out on 2026-11-03**: what was built, what changed from this
design and what it measured are in
[vu-translation.md](vu-translation.md). The main change: Sly 1's flag
reads cross blocks (the 3D loop's `fsand` reads a `msuby` in an earlier
block), so flags travel as four snapshots that every block carries in
locals, not as constants within a block; and waits move the flags on in
place instead of handing the pair to the interpreter.

## Where it stands

Gameplay is VU1-bound: about 1.4 million pairs a field, 84.5% of host
time.[^measurements] After PCSX2's float model and host single precision
([sly1-graphics.md](sly1-graphics.md)), translated microcode takes 7.8 ns a
pair and the prologue's workload runs at 67.6% of console speed. The
float math is now about 5% of the work; what remains is the generic
per-pair machinery the translations inherit by calling `micro::exec` for
every pair:

- the FMAC flag queue (push, landing check, landing) and the flag words:
  about a third of the VU work;
- pending Q and P checks, delay-slot and E-bit bookkeeping, the cycle
  count, the write list;
- the `match` on the pc for every pair.

Tried and dropped, as no faster: straight-line code per basic block over
`exec`, lazy landing of flags inside `exec`, adds in single precision with
an error correction before the float switch.

**Budget**: 16.7 ms a field for everything, so VU1 should take at most
about 5 ns a pair to leave the EE and GS room.

## Design

As rac-rs's generator does,[^rac-rs] adapted to sly-rs's simpler timing
model (results are ready at the next pair; no VF stalls are modelled):

1. **Blocks.** Cut each program into basic blocks (leaders: entries,
   branch targets, the pair after a delay slot). One function per program
   piece; one arm per block; a block runs straight on.
2. **Registers in locals.** `vf`, `acc`, `vi`, `q`, `p`, `i`, `r` are
   loaded into locals at function entry and written back at every exit;
   the compiler keeps them in vector registers. Each pair becomes the few
   statements it needs: `vf3 = merge(vf3, rtz::madd(acc, vf5, bc(vf7, 0)),
   0b0111)`; loads and stores index the data memory directly.
3. **Timing as constants.** Within a block the cycle is the block's entry
   cycle plus the pair's index, so a flag landing whose producer is in the
   same block is a constant: an `FSAND` four pairs after its `MSUBY` reads
   that `MSUBY`'s sign directly. Producers in an earlier block are read
   from a small runtime record (the last few flag results and their
   cycles), updated only where a block ends.
4. **Flags lazily.** A flag-setting pair keeps its raw result and `dest`;
   the MAC word is built only where an `FM*` instruction or the program's
   end needs it; the sticky Z and S bits accumulate with two vector
   operations.
5. **Q and P** latencies are constants within a block where the `DIV` or
   EFU instruction is in the block; otherwise a runtime ready cycle, as
   now.
6. **Bail to the interpreter** for what the generator does not handle
   (`JR`/`JALR` to a target outside the piece, a branch in a delay slot,
   rare instructions, the runaway guard): write the locals back, rebuild
   `micro::State` exactly (cycle, flag queue, Q and P), and continue in
   `exec`.

## Checks

- `vu-bench` compares the interpreter with translations on recorded
  programs: extend it to compare old translations, new translations and
  the interpreter, register for register, packet for packet, cycle count
  included.
- A unit test that bails in mid-block and continues in the interpreter.
- Lockstep and the replay hashes, unchanged: the semantics are the
  interpreter's.
- Speed: `vu-bench --translated-only` against the 5 ns budget, then the
  prologue's workload headless.

## Steps

1. The generator's skeleton: blocks, locals, write-back, bail; every pair
   through a per-op emitter that covers the upper FMAC operations, loads,
   stores, integer operations and branches, everything else bailing.
2. Flags lazily, with timing resolved inside blocks.
3. Q, P and the EFU.
4. Measure; then cover what still bails often.

[^measurements]: VU1 measurements in gameplay
[^rac-rs]: rac-rs's VU1 generator
