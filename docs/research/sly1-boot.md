---
type: Finding
title: How Sly 1 boots in the interpreter
description: The start-up sequence of Sly 1 (SCUS-97198) as sly-rs runs it - kernel patches, threads, the SIF handshake, the IOP reboot, CD/DVD and module-loader RPC, the sound driver running on the emulated IOP and loading its banks, DMA and GS FINISH events - up to the frame loop it runs now.
tags: [finding, boot, sly1, kernel, sif, rpc, iop, sound, phase-3, phase-5]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-03T01:00:08Z }
sources:
  - id: run
    resource: sly-rs run sly1 --fields 600, with SLY_RS_LOG=iop=debug and --watch 1e1db8, 2026-11-02
    title: Runs of Sly 1 in the interpreter
  - id: gate
    resource: crates/sly-verify/tests/interpreter.rs
    title: The phase 3 gate test
  - id: phase5
    resource: sly-rs run sly1 --fields 600 with SLY_RS_LOG=iop=debug, iop::hle=trace; sly1 lockstep sly1 --fields 300, 2026-11-03
    title: Runs of Sly 1 with LIBSD and 989SND on the emulated IOP
  - id: ps2sdk
    resource: https://github.com/ps2dev/ps2sdk
    title: 'ps2sdk: ee/kernel/src/sifcmd.c, sifrpc.c; iop/system/sifcmd; ee/rpc/cdvd (read for the protocol, not copied)'
  - id: symbols
    resource: games/sly1/symbols.tsv
    title: Sly 1 function names (TheOnlyZac/sly1, CC0)
verified:
  - { by: claude-code/claude-opus-5-5, at: 2026-11-03T01:00:08Z }
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
9. **Sound** (since phase 5): `LIBSD` and `989SND` run on the emulated
   IOP.[^phase5]
   - `LIBSD` registers its library (31 functions) and its start function
     returns 0. `989SND` links against it, registers its RPC servers
     `0x123456` and `0x123457` with its own receive buffers, and returns 0.
   - The EE binds both and calls function 0; `sceSdInit` initialises the
     SPU2's cores.
10. **Sound banks**: `snd_BankLoadByLoc` sends `0x123457` function 3 as an
    asynchronous RPC and spins on `FlushCache` until the reply overwrites
    -1 at `0x260E00`.
    - 989SND reads the bank itself with `cdvdman`: a 365-sector read for the
      first, 2-sector streaming reads for the second.
    - `libsd` moves it to SPU2 memory by DMA (channel 4: 749,312 and
      588,096 bytes). After each transfer its interrupt handler waits for
      core 0's STATX bit 7 before signalling 989SND.
    - Both banks are in by about field 16.
11. **First frame**: the game decompresses data it read through `cdvdfsv`,
    sets the GS CSR's FINISH enable, unmasks it in the IMR and sends a
    packet ending in a FINISH write. It then waits on a semaphore its GS
    interrupt handler signals.
    - FINISH arrives through both paths: a GIF chain (the first is 8,518
      quadwords over 234 tags) and VIF1 DIRECT packets.
    - DMA transfers complete at a modelled time: two cycles per quadword,
      eight per tag. The data goes nowhere yet.
12. **Frame loop**: from there the game runs one frame per field, each
    kicking VIF0, VIF1, GIF and scratchpad DMA (about 587 frames in 600
    fields), with its UI code among the busiest functions
    (`RepositionBlotDependents`).

## Graphics, in passing

DMA transfers are followed through memory and their GIF packets read only
for GS events (SIGNAL, FINISH, LABEL). Nothing is drawn: phase 6 renders
the packets.

## Where it is now

In a steady frame loop (step 12). What the frames show cannot be seen until
phase 6; telling the title screen apart needs an observable from the game's
state (a decompilation symbol or a RAM address), the next part of phase 5
along with the pad, the memory card and modelled disc timing.

The phase 3 gate test still runs this boot twice for 300 fields and checks
that both runs end with the same state hash, now including the IOP's
registers and RAM.[^gate]

[^run]: Runs of Sly 1 in the interpreter.
[^symbols]: Sly 1 function names.
[^ps2sdk]: ps2sdk SIF and CD/DVD sources.
[^gate]: The phase 3 gate test.
[^phase5]: Runs of Sly 1 with LIBSD and 989SND on the emulated IOP.
