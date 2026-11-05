//! The IOP kernel's libraries, high-level emulated: what a module's import
//! of `thbase`, `sysclib`, `stdio`, ... does, written in Rust. Function
//! numbers and semantics follow ps2sdk's headers and its free
//! reimplementations of the kernel (read for facts; nothing is copied).
//!
//! A function that is not written yet stops the machine with its name and
//! call site rather than guessing.

use ps2_core::iop::IopCpu;

use crate::core::{Callback, CallbackKind, GuestServer, HwTimer, IopCore};
use crate::format::format;
use crate::irx;
use crate::kernel::{self, RPC_LOOP, Thread, Wait, kerr, wef};

/// What a kernel call does.
pub(crate) enum Hle {
    /// Returns this in `$v0`.
    Return(u32),
    /// Continues at this address instead of returning.
    Goto(u32),
    /// The calling thread waits; its `$v0` is set when the wait ends.
    Block(Wait),
    /// Not written yet.
    Unimplemented,
}

fn ok(value: i32) -> Hle {
    Hle::Return(value.cast_unsigned())
}

/// The libraries emulated here (their imports are intercepted).
const LIBRARIES: &[(&str, &[(u16, &str)])] = &[
    (
        "intrman",
        &[
            (4, "RegisterIntrHandler"),
            (5, "ReleaseIntrHandler"),
            (6, "EnableIntr"),
            (7, "DisableIntr"),
            (8, "CpuDisableIntr"),
            (9, "CpuEnableIntr"),
            (17, "CpuSuspendIntr"),
            (18, "CpuResumeIntr"),
            (23, "QueryIntrContext"),
        ],
    ),
    (
        "loadcore",
        &[
            (4, "FlushIcache"),
            (5, "FlushDcache"),
            (6, "RegisterLibraryEntries"),
            (7, "ReleaseLibraryEntries"),
        ],
    ),
    (
        "sysmem",
        &[
            (4, "AllocSysMemory"),
            (5, "FreeSysMemory"),
            (6, "QueryMemSize"),
            (7, "QueryMaxFreeMemSize"),
            (8, "QueryTotalFreeMemSize"),
            (14, "Kprintf"),
        ],
    ),
    ("stdio", &[(4, "printf"), (6, "putchar"), (7, "puts")]),
    (
        "sysclib",
        &[
            (4, "setjmp"),
            (5, "longjmp"),
            (10, "memchr"),
            (11, "memcmp"),
            (12, "memcpy"),
            (13, "memmove"),
            (14, "memset"),
            (15, "bcmp"),
            (16, "bcopy"),
            (17, "bzero"),
            (19, "sprintf"),
            (20, "strcat"),
            (21, "strchr"),
            (22, "strcmp"),
            (23, "strcpy"),
            (27, "strlen"),
            (28, "strncat"),
            (29, "strncmp"),
            (30, "strncpy"),
            (32, "strrchr"),
            (34, "strstr"),
            (36, "strtol"),
            (38, "strtoul"),
        ],
    ),
    (
        "thbase",
        &[
            (4, "CreateThread"),
            (5, "DeleteThread"),
            (6, "StartThread"),
            (7, "StartThreadArgs"),
            (8, "ExitThread"),
            (9, "ExitDeleteThread"),
            (10, "TerminateThread"),
            (14, "ChangeThreadPriority"),
            (16, "RotateThreadReadyQueue"),
            (20, "GetThreadId"),
            (22, "ReferThreadStatus"),
            (24, "SleepThread"),
            (25, "WakeupThread"),
            (26, "iWakeupThread"),
            (33, "DelayThread"),
            (34, "GetSystemTime"),
            (39, "USec2SysClock"),
            (40, "SysClock2USec"),
        ],
    ),
    (
        "thsemap",
        &[
            (4, "CreateSema"),
            (5, "DeleteSema"),
            (6, "SignalSema"),
            (7, "iSignalSema"),
            (8, "WaitSema"),
            (9, "PollSema"),
            (11, "ReferSemaStatus"),
        ],
    ),
    (
        "thevent",
        &[
            (4, "CreateEventFlag"),
            (5, "DeleteEventFlag"),
            (6, "SetEventFlag"),
            (7, "iSetEventFlag"),
            (8, "ClearEventFlag"),
            (9, "iClearEventFlag"),
            (10, "WaitEventFlag"),
            (11, "PollEventFlag"),
        ],
    ),
    (
        "timrman",
        &[
            (4, "AllocHardTimer"),
            (5, "ReferHardTimer"),
            (6, "FreeHardTimer"),
            (20, "SetTimerHandler"),
            (22, "SetupHardTimer"),
            (23, "StartHardTimer"),
            (24, "StopHardTimer"),
        ],
    ),
    (
        "sifman",
        &[
            (5, "sceSifInit"),
            (7, "sceSifSetDma"),
            (8, "sceSifDmaStat"),
            (29, "sceSifCheckInit"),
            (32, "sceSifSetDmaIntr"),
        ],
    ),
    (
        "sifcmd",
        &[
            (14, "sceSifInitRpc"),
            (17, "sceSifRegisterRpc"),
            (19, "sceSifSetRpcQueue"),
            (22, "sceSifRpcLoop"),
            (23, "sceSifGetOtherData"),
        ],
    ),
    (
        "ioman",
        &[
            (4, "open"),
            (5, "close"),
            (6, "read"),
            (7, "write"),
            (8, "lseek"),
        ],
    ),
    (
        "cdvdman",
        &[
            (6, "sceCdRead"),
            (8, "sceCdGetError"),
            (11, "sceCdSync"),
            (15, "sceCdStop"),
            (37, "sceCdCallback"),
            (39, "sceCdBreak"),
        ],
    ),
];

/// Whether `library` is emulated here.
pub(crate) fn provides(library: &str) -> bool {
    LIBRARIES.iter().any(|(name, _)| *name == library)
}

/// A kernel function's name, for messages.
pub(crate) fn function_name(library: &str, index: u16) -> &'static str {
    LIBRARIES
        .iter()
        .find(|(name, _)| *name == library)
        .and_then(|(_, functions)| functions.iter().find(|(i, _)| *i == index))
        .map_or("?", |(_, name)| name)
}

/// IOP cycles per microsecond (36.864 MHz).
const CYCLES_PER_MS: u64 = 36_864;

/// Argument `n` of the current call (the fifth and later are on the stack).
fn arg(core: &mut IopCore, ram: &mut [u8], n: u32) -> u32 {
    if n < 4 {
        core.cpu.gpr[4 + n as usize]
    } else {
        let sp = core.cpu.gpr[29];
        core.memory(ram).read32(sp + 16 + 4 * (n - 4))
    }
}

/// A NUL-terminated string in IOP memory (at most 4 KiB).
fn string(core: &mut IopCore, ram: &mut [u8], address: u32) -> Vec<u8> {
    let mut mem = core.memory(ram);
    (0..4096)
        .map(|i| mem.read8(address.wrapping_add(i)))
        .take_while(|&b| b != 0)
        .collect()
}

/// Formats with arguments from `first` on.
#[allow(clippy::naive_bytecount, reason = "format strings are short")]
fn formatted(core: &mut IopCore, ram: &mut [u8], fmt: u32, first: u32) -> Vec<u8> {
    let fmt_bytes = string(core, ram, fmt);
    // Enough arguments for every conversion and `*` (at most 64).
    let count = (fmt_bytes.iter().filter(|&&b| b == b'%').count() * 2).min(64) as u32;
    let args: Vec<u32> = (0..count).map(|k| arg(core, ram, first + k)).collect();
    let mut it = args.into_iter();
    let ram: &[u8] = ram;
    format(
        &fmt_bytes,
        || it.next().unwrap_or(0),
        |address| {
            ram.get((address & 0x1F_FFFF) as usize..)
                .unwrap_or_default()
                .iter()
                .take(4096)
                .take_while(|&&b| b != 0)
                .copied()
                .collect()
        },
    )
}

/// Runs `library`'s function `index` for the current thread.
#[allow(
    clippy::too_many_lines,
    clippy::match_same_arms,
    reason = "one arm per kernel function"
)]
pub(crate) fn call(
    core: &mut IopCore,
    ram: &mut [u8],
    ee: &mut [u8],
    library: &str,
    index: u16,
) -> Hle {
    let (a0, a1, a2, a3) = (
        core.cpu.gpr[4],
        core.cpu.gpr[5],
        core.cpu.gpr[6],
        core.cpu.gpr[7],
    );
    tracing::trace!(target: "iop::hle", "cycle {}: {library} {} ({a0:#x}, {a1:#x}, {a2:#x}, {a3:#x}) from {:#x}", core.cpu.cycles, function_name(library, index), core.cpu.gpr[31]);
    match (library, index) {
        // intrman
        ("intrman", 17) => {
            let was = core.kernel.interrupts;
            core.kernel.interrupts = false;
            core.memory(ram).write32(a0, u32::from(was));
            ok(if was { 0 } else { kerr::CPUDI })
        }
        ("intrman", 18) => {
            core.kernel.interrupts = a0 != 0;
            ok(0)
        }
        ("intrman", 4) => {
            let gp = core.cpu.gpr[28];
            core.handlers.insert(a0, (a2, a3, gp));
            ok(0)
        }
        ("intrman", 5) => {
            core.handlers.remove(&a0);
            ok(0)
        }
        ("intrman", 6 | 7) => {
            let enable = index == 6;
            if enable {
                core.enabled.insert(a0);
            } else {
                core.enabled.remove(&a0);
            }
            if !enable && a1 != 0 {
                core.memory(ram).write32(a1, a0);
            }
            ok(0)
        }
        ("intrman", 23) => ok(i32::from(core.kernel.in_interrupt)),
        ("intrman", 8) => {
            core.kernel.interrupts = false;
            ok(0)
        }
        ("intrman", 9) => {
            core.kernel.interrupts = true;
            ok(0)
        }

        // loadcore
        ("loadcore", 4 | 5) => ok(0),
        ("loadcore", 6) => match irx::exports(ram, a0) {
            Some((name, _, functions)) => {
                core.register_library(name, functions);
                ok(0)
            }
            None => ok(-1),
        },

        // sysmem
        ("sysmem", 4) => Hle::Return(core.kernel.heap.alloc(a0, a1, a2).unwrap_or(0)),
        ("sysmem", 5) => ok(if core.kernel.heap.free(a0) { 0 } else { -1 }),
        ("sysmem", 6) => Hle::Return(ps2_core::iop::RAM_SIZE as u32),
        ("sysmem", 7) => Hle::Return(core.kernel.heap.max_free()),
        ("sysmem", 8) => Hle::Return(core.kernel.heap.total_free()),

        // stdio
        ("stdio", 4) => {
            let text = formatted(core, ram, a0, 1);
            core.console.extend_from_slice(&text);
            Hle::Return(text.len() as u32)
        }
        ("stdio", 6) => {
            core.console.push(a0 as u8);
            Hle::Return(a0)
        }
        ("stdio", 7) => {
            let text = string(core, ram, a0);
            core.console.extend_from_slice(&text);
            core.console.push(b'\n');
            ok(0)
        }

        // sysclib
        ("sysclib", 12 | 13 | 16) => {
            let (dst, src) = if index == 16 { (a1, a0) } else { (a0, a1) };
            let bytes: Vec<u8> = {
                let mut mem = core.memory(ram);
                (0..a2).map(|i| mem.read8(src.wrapping_add(i))).collect()
            };
            let mut mem = core.memory(ram);
            for (i, b) in bytes.into_iter().enumerate() {
                mem.write8(dst.wrapping_add(i as u32), b);
            }
            core.cpu.cycles += u64::from(a2 / 4);
            Hle::Return(a0)
        }
        ("sysclib", 14 | 17) => {
            let (dst, value, n) = if index == 17 {
                (a0, 0, a1)
            } else {
                (a0, a1 as u8, a2)
            };
            let mut mem = core.memory(ram);
            for i in 0..n {
                mem.write8(dst.wrapping_add(i), value);
            }
            core.cpu.cycles += u64::from(n / 4);
            Hle::Return(a0)
        }
        ("sysclib", 11 | 15) => {
            let mut mem = core.memory(ram);
            let diff = (0..a2)
                .map(|i| i32::from(mem.read8(a0 + i)) - i32::from(mem.read8(a1 + i)))
                .find(|&d| d != 0)
                .unwrap_or(0);
            ok(diff)
        }
        ("sysclib", 19) => {
            let text = formatted(core, ram, a1, 2);
            let mut mem = core.memory(ram);
            for (i, &b) in text.iter().chain(std::iter::once(&0)).enumerate() {
                mem.write8(a0 + i as u32, b);
            }
            Hle::Return(text.len() as u32)
        }
        ("sysclib", 22 | 29) => {
            let limit = if index == 29 { a2 } else { u32::MAX };
            let mut mem = core.memory(ram);
            let mut diff = 0;
            for i in 0..limit {
                let (x, y) = (mem.read8(a0.wrapping_add(i)), mem.read8(a1.wrapping_add(i)));
                if x != y || x == 0 {
                    diff = i32::from(x) - i32::from(y);
                    break;
                }
            }
            ok(diff)
        }
        ("sysclib", 23 | 30) => {
            let limit = if index == 30 { a2 } else { u32::MAX };
            let mut mem = core.memory(ram);
            let mut i = 0;
            let mut ended = false;
            while i < limit {
                let b = if ended {
                    0
                } else {
                    mem.read8(a1.wrapping_add(i))
                };
                mem.write8(a0.wrapping_add(i), b);
                ended |= b == 0;
                i += 1;
                if ended && index == 23 {
                    break;
                }
            }
            Hle::Return(a0)
        }
        ("sysclib", 27) => Hle::Return(string(core, ram, a0).len() as u32),
        ("sysclib", 36 | 38) => {
            let text = string(core, ram, a0);
            let (value, used) = strtol(&text, a2);
            if a1 != 0 {
                core.memory(ram).write32(a1, a0 + used as u32);
            }
            Hle::Return(value)
        }

        // thbase
        ("thbase", 4) => {
            let mut mem = core.memory(ram);
            let (attr, option, entry, stack_size, priority) = (
                mem.read32(a0),
                mem.read32(a0 + 4),
                mem.read32(a0 + 8),
                mem.read32(a0 + 12),
                mem.read32(a0 + 16),
            );
            if !(1..=126).contains(&priority) {
                return ok(kerr::ILLEGAL_PRIORITY);
            }
            let Some(stack) = core.kernel.heap.alloc(1, stack_size, 0) else {
                return ok(kerr::NO_MEMORY);
            };
            let gp = core.cpu.gpr[28];
            let id = core.kernel.add_thread(Thread {
                priority,
                init_priority: priority,
                status: kernel::Status::Dormant,
                wait: Wait::None,
                entry,
                stack: (stack, stack_size),
                gp,
                attr: (attr, option),
                wakeups: 0,
                context: IopCpu::default(),
            });
            Hle::Return(id)
        }
        ("thbase", 6) => {
            let Some(thread) = core.kernel.thread(a0) else {
                return ok(kerr::UNKNOWN_THID);
            };
            if thread.status != kernel::Status::Dormant {
                return ok(kerr::NOT_DORMANT);
            }
            let context = kernel::start_context(
                thread.entry,
                &[a1],
                thread.gp,
                (thread.stack.0 + thread.stack.1 - 16) & !7,
                kernel::THREAD_EXIT,
            );
            thread.priority = thread.init_priority;
            thread.context = context;
            core.kernel.make_ready(a0 as usize - 1, false);
            ok(0)
        }
        ("thbase", 20) => Hle::Return(core.kernel.current.map_or(0, |t| t as u32 + 1)),
        ("thbase", 24) => {
            let Some(t) = core.kernel.current else {
                return ok(0);
            };
            let thread = core.kernel.threads[t].as_mut().map(|th| {
                let pending = th.wakeups > 0;
                th.wakeups = th.wakeups.saturating_sub(1);
                pending
            });
            if thread == Some(true) {
                ok(0)
            } else {
                Hle::Block(Wait::Sleep)
            }
        }
        ("thbase", 25 | 26) => {
            let t = a0 as usize;
            let Some(thread) = core.kernel.thread(a0) else {
                return ok(kerr::UNKNOWN_THID);
            };
            if thread.wait == Wait::Sleep && thread.status == kernel::Status::Waiting {
                core.kernel.wake(t - 1, 0);
            } else {
                thread.wakeups += 1;
            }
            ok(0)
        }
        ("thbase", 33) => {
            let Some(t) = core.kernel.current else {
                return ok(0);
            };
            let cycles = u64::from(a0) * CYCLES_PER_MS / 1000;
            core.kernel.set_alarm(core.cpu.cycles + cycles.max(1), t);
            Hle::Block(Wait::Delay)
        }
        ("thbase", 39) => {
            // The system clock is the IOP's own (36.864 MHz); a 64-bit
            // count as { low, high }.
            let cycles = u64::from(a0) * CYCLES_PER_MS / 1000;
            let mut mem = core.memory(ram);
            mem.write32(a1, cycles as u32);
            mem.write32(a1 + 4, (cycles >> 32) as u32);
            ok(0)
        }
        ("thbase", 34) => {
            let now = core.cpu.cycles;
            let mut mem = core.memory(ram);
            mem.write32(a0, now as u32);
            mem.write32(a0 + 4, (now >> 32) as u32);
            ok(0)
        }

        // thsemap
        ("thsemap", 4) => {
            let mut mem = core.memory(ram);
            let (attr, initial, max) = (mem.read32(a0), mem.read32(a0 + 8), mem.read32(a0 + 12));
            core.kernel.semas.push(Some(kernel::Sema {
                count: initial.cast_signed(),
                max: max.cast_signed(),
                attr,
                waiters: std::collections::VecDeque::new(),
            }));
            Hle::Return(core.kernel.semas.len() as u32)
        }
        ("thsemap", 5) => {
            let Some(sema) = sema_slot(core, a0).and_then(Option::take) else {
                return ok(kerr::UNKNOWN_SEMID);
            };
            for t in sema.waiters {
                core.kernel.wake(t, kerr::WAIT_DELETE);
            }
            ok(0)
        }
        ("thsemap", 6 | 7) => {
            let Some(sema) = sema_slot(core, a0).and_then(Option::as_mut) else {
                return ok(kerr::UNKNOWN_SEMID);
            };
            if let Some(t) = sema.waiters.pop_front() {
                core.kernel.wake(t, 0);
                ok(0)
            } else if sema.count >= sema.max {
                ok(kerr::SEMA_OVF)
            } else {
                sema.count += 1;
                ok(0)
            }
        }
        ("thsemap", 8 | 9) => {
            let current = core.kernel.current;
            let Some(sema) = sema_slot(core, a0).and_then(Option::as_mut) else {
                return ok(kerr::UNKNOWN_SEMID);
            };
            if sema.count > 0 {
                sema.count -= 1;
                ok(0)
            } else if index == 9 {
                ok(kerr::SEMA_ZERO)
            } else if let Some(t) = current {
                sema.waiters.push_back(t);
                Hle::Block(Wait::Sema(a0 as usize - 1))
            } else {
                ok(kerr::SEMA_ZERO)
            }
        }

        // thevent
        ("thevent", 4) => {
            let mut mem = core.memory(ram);
            let (attr, bits) = (mem.read32(a0), mem.read32(a0 + 8));
            core.kernel.flags.push(Some(kernel::EventFlag {
                bits,
                attr,
                waiters: Vec::new(),
            }));
            Hle::Return(core.kernel.flags.len() as u32)
        }
        ("thevent", 5) => {
            let Some(flag) = flag_slot(core, a0).and_then(Option::take) else {
                return ok(kerr::UNKNOWN_EVFID);
            };
            for t in flag.waiters {
                core.kernel.wake(t, kerr::WAIT_DELETE);
            }
            ok(0)
        }
        ("thevent", 6 | 7) => {
            let Some(flag) = flag_slot(core, a0).and_then(Option::as_mut) else {
                return ok(kerr::UNKNOWN_EVFID);
            };
            flag.bits |= a1;
            set_event_flag(core, ram, a0 as usize - 1);
            ok(0)
        }
        ("thevent", 8 | 9) => {
            let Some(flag) = flag_slot(core, a0).and_then(Option::as_mut) else {
                return ok(kerr::UNKNOWN_EVFID);
            };
            flag.bits &= a1;
            ok(0)
        }
        ("thevent", 10 | 11) => {
            let current = core.kernel.current;
            if a1 == 0 {
                return ok(kerr::EVF_ILPAT);
            }
            let Some(flag) = flag_slot(core, a0).and_then(Option::as_mut) else {
                return ok(kerr::UNKNOWN_EVFID);
            };
            if matches(flag.bits, a1, a2) {
                let bits = flag.bits;
                clear_on_wait(flag, a1, a2);
                if a3 != 0 {
                    core.memory(ram).write32(a3, bits);
                }
                ok(0)
            } else if index == 11 {
                ok(kerr::EVF_COND)
            } else if !flag.waiters.is_empty() && flag.attr & 2 == 0 {
                ok(kerr::EVF_MULTI)
            } else if let Some(t) = current {
                flag.waiters.push(t);
                Hle::Block(Wait::Event {
                    flag: a0 as usize - 1,
                    bits: a1,
                    mode: a2,
                    result: a3,
                })
            } else {
                ok(kerr::EVF_COND)
            }
        }
        // sifman: transfers to the EE complete at once.
        ("sifman", 5) => ok(0),
        ("sifman", 29) => ok(1),
        ("sifman", 7 | 32) => {
            for i in 0..a1 {
                let at = a0 + 16 * i;
                let mut mem = core.memory(ram);
                let (src, dest, size) = (mem.read32(at), mem.read32(at + 4), mem.read32(at + 8));
                let bytes: Vec<u8> = (0..size).map(|k| mem.read8(src.wrapping_add(k))).collect();
                tracing::debug!(target: "iop::sif", "dma to EE {dest:#x}, {size} bytes: {:x?}", &bytes[..bytes.len().min(32)]);
                copy_to_ee(ee, dest, &bytes);
                core.cpu.cycles += u64::from(size / 16);
            }
            if index == 32 && a2 != 0 {
                core.callbacks.push_back(Callback {
                    func: a2,
                    args: [a3, 0, 0, 0],
                    gp: core.cpu.gpr[28],
                    kind: CallbackKind::Plain,
                });
            }
            core.dma_ids += 1;
            Hle::Return(core.dma_ids)
        }
        ("sifman", 8) => ok(-1),

        // sifcmd: the server side of RPC.
        ("sifcmd", 14) => ok(0),
        ("sifcmd", 19) => {
            core.queues
                .insert(a0, (a1 as usize - 1, std::collections::VecDeque::new()));
            ok(0)
        }
        ("sifcmd", 17) => {
            let queue = arg(core, ram, 6);
            tracing::debug!(target: "iop::rpc", "server {a1:#x} registered: function {a2:#x}, buffer {a3:#x}, queue {queue:#x}");
            core.servers.insert(
                a1,
                GuestServer {
                    func: a2,
                    buffer: a3,
                    queue,
                },
            );
            ok(0)
        }
        ("sifcmd", 22) => Hle::Goto(RPC_LOOP),
        ("sifcmd", 23) => {
            // sceSifGetOtherData(rd, EE source, IOP destination, size, mode).
            let size = a3;
            let bytes: Vec<u8> = (0..size)
                .map(|k| {
                    ee.get(((a1 + k) & 0x01FF_FFFF) as usize)
                        .copied()
                        .unwrap_or(0)
                })
                .collect();
            let mut mem = core.memory(ram);
            for (k, b) in bytes.into_iter().enumerate() {
                mem.write8(a2 + k as u32, b);
            }
            core.cpu.cycles += u64::from(size / 16);
            ok(0)
        }

        // cdvdman, for modules that read the disc themselves. Reads complete
        // at once (as the EE's do through cdvdfsv), then the callback runs.
        ("cdvdman", 6) => {
            let Some(data) = core.disc.as_ref().and_then(|d| d.read(a0, a1)) else {
                tracing::warn!(target: "iop::cdvd", "module read past the disc: {a1} sectors at {a0}");
                return ok(0);
            };
            let mut mem = core.memory(ram);
            for (k, &b) in data.iter().enumerate() {
                mem.write8(a2 + k as u32, b);
            }
            core.sectors_read += u64::from(a1);
            core.cpu.cycles += data.len() as u64 / 16;
            tracing::debug!(target: "iop::cdvd", "cycle {}: module read: {a1} sectors at LBA {a0} into {a2:#x}", core.cpu.cycles);
            let (func, gp) = core.cd_callback;
            if func != 0 {
                core.callbacks.push_back(Callback {
                    func,
                    args: [1, 0, 0, 0],
                    gp,
                    kind: CallbackKind::Plain,
                });
            }
            ok(1)
        }
        ("cdvdman", 8 | 11) => ok(0),
        ("cdvdman", 15 | 39) => ok(1),
        ("cdvdman", 37) => {
            let old = core.cd_callback.0;
            core.cd_callback = (a0, core.cpu.gpr[28]);
            Hle::Return(old)
        }

        // timrman: hardware timers on the IOP clock.
        ("timrman", 4) => match timer_cycles(a0, a2) {
            Some(cycles_per_tick) => {
                core.timers.push(Some(HwTimer {
                    cycles_per_tick,
                    compare: 0,
                    handler: 0,
                    arg: 0,
                    gp: 0,
                    due: None,
                }));
                Hle::Return(core.timers.len() as u32)
            }
            None => ok(-1),
        },
        ("timrman", 6) => match timer_slot(core, a0) {
            Some(slot) => {
                *slot = None;
                ok(0)
            }
            None => ok(-1),
        },
        ("timrman", 20) => {
            let gp = core.cpu.gpr[28];
            match timer_slot(core, a0).and_then(Option::as_mut) {
                Some(timer) => {
                    timer.compare = a1;
                    timer.handler = a2;
                    timer.arg = a3;
                    timer.gp = gp;
                    ok(0)
                }
                None => ok(-1),
            }
        }
        ("timrman", 22) => match (
            timer_slot(core, a0).and_then(Option::as_mut),
            timer_cycles(a1, a3),
        ) {
            (Some(timer), Some(cycles)) => {
                timer.cycles_per_tick = cycles;
                ok(0)
            }
            _ => ok(-1),
        },
        ("timrman", 23) => {
            let now = core.cpu.cycles;
            match timer_slot(core, a0).and_then(Option::as_mut) {
                Some(timer) => {
                    timer.due = Some(now + u64::from(timer.compare.max(1)) * timer.cycles_per_tick);
                    ok(0)
                }
                None => ok(-1),
            }
        }
        ("timrman", 24) => match timer_slot(core, a0).and_then(Option::as_mut) {
            Some(timer) => {
                timer.due = None;
                ok(0)
            }
            None => ok(-1),
        },
        _ => Hle::Unimplemented,
    }
}

/// IOP cycles per tick of a hardware timer counting `source` (1: the
/// system clock; 4: H-lines) divided by `prescale`.
fn timer_cycles(source: u32, prescale: u32) -> Option<u64> {
    let base = match source {
        1 => 1,
        // An NTSC H-line: 294.912 MHz × 1001 / 60,000 × 2 / 525 EE cycles, / 8.
        4 => 2_340,
        _ => return None,
    };
    let prescale = match prescale {
        0 | 1 => 1,
        8 | 16 | 256 => u64::from(prescale),
        _ => return None,
    };
    Some(base * prescale)
}

fn timer_slot(core: &mut IopCore, id: u32) -> Option<&mut Option<HwTimer>> {
    core.timers.get_mut((id as usize).checked_sub(1)?)
}

/// Copies `bytes` into EE RAM at `dest` (SIF DMA to the EE).
fn copy_to_ee(ee: &mut [u8], dest: u32, bytes: &[u8]) {
    for (k, &b) in bytes.iter().enumerate() {
        if let Some(slot) = ee.get_mut(((dest as usize) + k) & 0x01FF_FFFF) {
            *slot = b;
        }
    }
}

fn sema_slot(core: &mut IopCore, id: u32) -> Option<&mut Option<kernel::Sema>> {
    core.kernel.semas.get_mut((id as usize).checked_sub(1)?)
}

fn flag_slot(core: &mut IopCore, id: u32) -> Option<&mut Option<kernel::EventFlag>> {
    core.kernel.flags.get_mut((id as usize).checked_sub(1)?)
}

/// Whether `bits` satisfy a wait for `pattern` in `mode`.
fn matches(bits: u32, pattern: u32, mode: u32) -> bool {
    if mode & wef::OR != 0 {
        bits & pattern != 0
    } else {
        bits & pattern == pattern
    }
}

fn clear_on_wait(flag: &mut kernel::EventFlag, pattern: u32, mode: u32) {
    if mode & wef::CLEAR != 0 {
        flag.bits = 0;
    } else if mode & wef::CLEAR_PATTERN != 0 {
        flag.bits &= !pattern;
    }
}

/// Wakes the threads an event flag's new bits satisfy, in wait order.
fn set_event_flag(core: &mut IopCore, ram: &mut [u8], f: usize) {
    let waiters = core.kernel.flags[f]
        .as_ref()
        .map(|fl| fl.waiters.clone())
        .unwrap_or_default();
    for t in waiters {
        let Some(Wait::Event {
            bits, mode, result, ..
        }) = core.kernel.threads[t].as_ref().map(|th| th.wait)
        else {
            continue;
        };
        let Some(flag) = core.kernel.flags[f].as_mut() else {
            return;
        };
        if matches(flag.bits, bits, mode) {
            let value = flag.bits;
            clear_on_wait(flag, bits, mode);
            flag.waiters.retain(|&w| w != t);
            if result != 0 {
                core.memory(ram).write32(result, value);
            }
            core.kernel.wake(t, 0);
        }
    }
}

/// `strtol`/`strtoul`: the value and the bytes used.
fn strtol(text: &[u8], base: u32) -> (u32, usize) {
    let mut i = text.iter().take_while(|b| b.is_ascii_whitespace()).count();
    let negative = match text.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let mut base = base;
    let hex_prefix = text.get(i) == Some(&b'0') && matches!(text.get(i + 1), Some(b'x' | b'X'));
    if (base == 0 || base == 16) && hex_prefix {
        i += 2;
        base = 16;
    } else if base == 0 {
        base = if text.get(i) == Some(&b'0') { 8 } else { 10 };
    }
    let start = i;
    let mut value: u32 = 0;
    while let Some(d) = text.get(i).and_then(|&b| char::from(b).to_digit(base)) {
        value = value.wrapping_mul(base).wrapping_add(d);
        i += 1;
    }
    if i == start {
        return (0, 0);
    }
    (
        if negative {
            value.wrapping_neg()
        } else {
            value
        },
        i,
    )
}
