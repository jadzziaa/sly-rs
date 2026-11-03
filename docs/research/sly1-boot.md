---
type: Finding
title: How Sly 1 boots in the interpreter
description: The start-up sequence of Sly 1 (SCUS-97198) as sly-rs runs it - kernel patches, threads, the SIF handshake, the IOP reboot, CD/DVD and module-loader RPC, the sound driver on the emulated IOP, DMA and GS events, the pad, the memory card check, the title and a new game - up to its first level.
tags: [finding, boot, sly1, kernel, sif, rpc, iop, sound, phase-3, phase-5]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-03T03:14:22Z }
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
  - { by: claude-code/claude-opus-5-5, at: 2026-11-03T03:29:05Z }
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
    kicking VIF0, VIF1, GIF and scratchpad DMA.
13. **Pad**: `libpad` asks for padman's version (it needs major 4), sends
    `INIT` and `OPEN`; with a pad reported stable the joypad state reaches
    Ready.
14. **Memory card check**: the splash screen waits for the save system
    (`g_save`, `0x264A40`). `sceMcInit` needs mcserv ≥ 0x20A and mcman ≥
    0x20E, or every later call fails with -100. With no card in the slot
    (`GetInfo` -10), the check completes and the game goes on to its title,
    the attract world (level 2).
15. **New game**: Cross at field 700 starts a transition to `jb_intro`
    (level 3); Start at field 900 plays a movie, which three hooks skip
    (`CMpeg::Start`, `sceMpegIsEnd`, `CMpeg::Finish`). The level then
    decompresses its data, builds geometry and draws text.
16. **First level frame.** Until phase 6 modelled VIF1, the machine
    stalled here (about field 1000). No DMA was kicked, and all four EE
    threads waited on semaphores, the main one in `OpenFrame` on
    `g_semaFrmMac` (`0x2623B4`). The frame loop needs VIF1's interrupt:
    `StartupFrame` registers `VU1_Interrupt` for INTC cause 5. The handler
    reads results from the slice list and writes STC to `VIF1_FBRST`
    (`0x10003C10`) to resume the chain. With VIF0, VIF1, both VUs and
    scratchpad DMA modelled, frames run; see
    [sly1-graphics.md](sly1-graphics.md).
17. **Title and new game.** The title is drawn over `jb_intro` itself:
    the logo, "Press START button for New Game", and the night scene with
    Sly. It takes Start from about field 2000 (Start at 1600 is ignored).
    Start then begins the level's intro: Bentley on the binocucom.

## Graphics, in passing

Frames go through scratchpad DMA, VIF0 and VU0, VIF1 and VU1 to
`ps2-gs`, which draws them in software. `sly-rs run --screenshot` shows the
dialog, the title and the level's intro;
[sly1-graphics.md](sly1-graphics.md) has the details.

## Where it is now

In the first level's intro, under the replay
`games/sly1/replays/new-game.toml` (the phase 5 gate), re-tuned and
re-recorded once frames ran. Its checkpoints are at field 600 (the dialog),
1500 (the title over `jb_intro`) and 2600 (the intro, confirmed by
screenshot).

The phase 3 gate test still runs this boot twice for 300 fields and checks
that both runs end with the same state hash, now including the IOP's
registers and RAM.[^gate]

[^run]: Runs of Sly 1 in the interpreter.
[^symbols]: Sly 1 function names.
[^ps2sdk]: ps2sdk SIF and CD/DVD sources.
[^gate]: The phase 3 gate test.
[^phase5]: Runs of Sly 1 with LIBSD and 989SND on the emulated IOP.
