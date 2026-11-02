---
type: Finding
title: How Sly 1 boots in the interpreter
description: The start-up sequence of Sly 1 (SCUS-97198) as the EE interpreter, kernel HLE and IOP side run it - kernel patches, threads, the SIF handshake, the IOP reboot, CD/DVD and module-loader RPC, IOP modules - and where it stops in phase 3 (the 989snd sound driver).
tags: [finding, boot, sly1, kernel, sif, rpc, phase-3]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-02T22:49:03Z }
verified:
  - { by: claude-code/claude-opus-5-5, at: 2026-11-02T22:48:00Z }
sources:
  - id: run
    resource: sly-rs run sly1 --fields 600, with SLY_RS_LOG=iop=debug and --watch 1e1db8, 2026-11-02
    title: Runs of Sly 1 in the interpreter
  - id: gate
    resource: crates/sly-verify/tests/interpreter.rs
    title: The phase 3 gate test
  - id: ps2sdk
    resource: https://github.com/ps2dev/ps2sdk
    title: 'ps2sdk: ee/kernel/src/sifcmd.c, sifrpc.c; iop/system/sifcmd; ee/rpc/cdvd (read for the protocol, not copied)'
  - id: symbols
    resource: games/sly1/symbols.tsv
    title: Sly 1 function names (TheOnlyZac/sly1, CC0)
---

# How Sly 1 boots in the interpreter

Observed on 2026-11-02 with `sly-rs run sly1`; function names from the
decompilation.[^run][^symbols] The interpreter runs at 160-180 million
instructions per second, about 0.6 times real time.

## The sequence

1. **crt0** (`_start`): `SetupThread` (main thread stack and argv),
   `SetupHeap`.
2. **Kernel patches.** Sony's `libkernel` patches the kernel:
   - 20 `SetSyscall`, 12 `GetEntryAddress` and 4 `Copy` calls, plus one
     `FindAddress`.
   - The HLE kernel has nothing to patch, so it accepts them and copies the
     code it is given.
3. **Threads.** Three threads are created and two started:
   - a priority-0 thread that waits on a semaphore at once;
   - one at priority 2;
   - the main thread, which drops to priority 4.

   V-blank (INTC 2/3) and SIF0 DMA interrupt handlers are registered and
   enabled.
4. **SIF handshake** (`sceSifInitCmd`, `sceSifInitRpc`):
   - The EE reads the IOP's packet buffer address from SMCOM.
   - It sends `INIT_CMD` (opt 0) carrying its own buffer (`0x6292C0`), then
     `INIT_CMD` (opt 1).
   - It then polls software register 0. The IOP side sets it with a
     `SET_SREG` packet, delivered by the SIF0 DMA interrupt.[^ps2sdk]
5. **CD/DVD start-up** (`StartupSif`, `FValidCdMmode`):
   - Binds `0x80000592` (`sceCdInit`) and `0x80000593`, then queries the
     disc type, which must be `0x14` (PS2 DVD).
   - Calls `0x80000593` function `0x22` (media mode).
   - If the type is wrong, the game halts in an infinite loop of `nop`s.
6. **IOP reboot** (`sceSifRebootIop`):
   - `RESET` with `rom0:UDNL cdrom0:\IOPRP243.IMG;1`, after which the EE
     clears the SBUS flags.
   - The IOP side reports itself booted again (SIFINIT, CMDINIT, BOOTEND)
     a quarter of a field later. The handshake and CD/DVD binds then repeat.
7. **Modules** (`sceSifLoadModule`):
   - Asks the loader (`0x80000006`) for its version, which must be `"2430"`.
     It retries forever otherwise.
   - Then loads `SIO2MAN`, `PADMAN`, `MCMAN`, `MCSERV`, `LIBSD` and
     `989SND` from `cdrom0:`.
8. **Pad**: binds `padman` (`0x80000100`, `0x80000101`). Function 1
   receives zeros and the game prints `libpad: Module version mismatch`.
9. **Sound**: binds `989snd` (`0x123456`, `0x123457`) and calls function 0.
   The reply is zeros, and the driver's EE side then loops forever printing
   `989snd.c: Sif says RPC isn't busy, but we still don't have returns from
   the IOP!`

## Graphics, in passing

Start-up kicks the GIF DMA channel five times. Phase 3 completes each kick at
once without moving data. A packet that sets GS state a later wait depends on
(`SIGNAL`/`FINISH` bits in the CSR, polled by `sceGsSyncV` and the like)
would go unnoticed until phase 6 replays these transfers.

## Where phase 3 stops

At step 9: the first wait on a subsystem not yet built. The plan recompiles
`989snd.irx` in phase 5 rather than imitating its protocol. No sectors have
been read yet: the game's first data reads come after sound start-up, so the
plan's expectation that disc reads precede the first wait did not hold. The
CD/DVD read server is written and unit-sized, but unexercised by the game so
far.

The gate test runs this boot twice for 300 fields. It checks that both runs
end with the same state hash (registers, clock, RAM, scratchpad) and that
every step above happened.[^gate]

[^run]: Runs of Sly 1 in the interpreter.
[^symbols]: Sly 1 function names.
[^ps2sdk]: ps2sdk SIF and CD/DVD sources.
[^gate]: The phase 3 gate test.
