//! A high-level emulation of the EE kernel: system calls by number, threads
//! with the kernel's strict-priority scheduling, semaphores, interrupt
//! handler chains, alarms and the SIF calls.
//!
//! Interrupt handlers run on the interrupted thread's registers, saved and
//! restored around them, with a return address the machine recognises
//! ([`INTERRUPT_RETURN`]). Threads that return from their entry function
//! land on [`THREAD_EXIT`].

use std::collections::{BTreeMap, VecDeque};

use ps2_analysis::syscall_name;
use ps2_core::{Cpu, Memory};

use crate::hw::Hardware;

/// Return address of interrupt and alarm handlers, in the kernel's memory.
pub const INTERRUPT_RETURN: u32 = 0x0000_1000;
/// Return address of a thread's entry function.
pub const THREAD_EXIT: u32 = 0x0000_1008;
/// Top of the stack interrupt handlers run on.
pub const INTERRUPT_STACK: u32 = 0x0007_F000;
/// Where the kernel keeps `argv` for the main thread.
pub const ARGS: u32 = 0x0007_F400;

/// Registers a thread switch saves.
#[derive(Clone)]
struct Context {
    gpr: [u128; 32],
    hi: u128,
    lo: u128,
    sa: u32,
    pc: u32,
    fpr: [u32; 32],
    fpu_acc: u32,
    fcr31: u32,
}

impl Context {
    fn save(cpu: &Cpu) -> Self {
        Self {
            gpr: cpu.gpr,
            hi: cpu.hi,
            lo: cpu.lo,
            sa: cpu.sa,
            pc: cpu.pc,
            fpr: cpu.fpr,
            fpu_acc: cpu.fpu_acc,
            fcr31: cpu.fcr31,
        }
    }

    fn restore(&self, cpu: &mut Cpu) {
        cpu.gpr = self.gpr;
        cpu.hi = self.hi;
        cpu.lo = self.lo;
        cpu.sa = self.sa;
        cpu.pc = self.pc;
        cpu.fpr = self.fpr;
        cpu.fpu_acc = self.fpu_acc;
        cpu.fcr31 = self.fcr31;
    }
}

/// What a thread is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Running or ready to run.
    Ready,
    /// Waiting on a semaphore.
    WaitSema(usize),
    /// In `SleepThread`.
    Sleeping,
    /// Suspended.
    Suspended,
    /// Created or exited, not started.
    Dormant,
}

/// An EE thread.
#[derive(Clone)]
pub struct Thread {
    /// Its state.
    pub status: Status,
    /// Current priority (lower runs first).
    pub priority: u32,
    initial_priority: u32,
    entry: u32,
    stack: u32,
    stack_size: u32,
    gp: u32,
    wakeups: u32,
    context: Option<Context>,
}

/// A semaphore.
#[derive(Clone, Debug)]
pub struct Sema {
    count: i32,
    max: i32,
    init: i32,
    attr: u32,
    option: u32,
    waiters: VecDeque<usize>,
}

/// A registered interrupt handler.
#[derive(Clone, Debug)]
struct Handler {
    id: u32,
    cause: u32,
    address: u32,
    arg: u32,
    enabled: bool,
}

/// An alarm: a handler called after a number of H-blanks.
#[derive(Clone, Debug)]
struct Alarm {
    id: u32,
    due: u64,
    time: u32,
    handler: u32,
    arg: u32,
}

/// Handlers still to run for the interrupt being serviced, and what was
/// running before it.
struct Interrupt {
    saved: Option<Context>,
    queue: VecDeque<(u32, [u32; 3])>,
}

/// What the machine must do after a kernel call.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Carry on.
    Continue,
    /// The program called `Exit` (or an equivalent).
    Exit(i32),
    /// A call the HLE does not implement.
    Unimplemented(String),
}

/// The kernel's state.
#[derive(Default)]
pub struct Kernel {
    /// Threads by id (index 0 unused).
    pub threads: Vec<Option<Thread>>,
    /// The running thread, or `None` when all wait.
    pub current: Option<usize>,
    /// Semaphores by id (index 0 unused).
    pub semas: Vec<Option<Sema>>,
    intc: Vec<Handler>,
    dmac: Vec<Handler>,
    next_handler_id: u32,
    alarms: Vec<Alarm>,
    next_alarm_id: u32,
    vsync_flag: Option<(u32, u32)>,
    heap_end: u32,
    interrupt: Option<Interrupt>,
    /// The kernel's SIF registers (`sceSifSetReg`), by number.
    pub sif_regs: BTreeMap<u32, u32>,
    /// Calls made, by number, for the report.
    pub calls: BTreeMap<i16, u64>,
    /// SIF DMA transfers handed to the IOP side, waiting to be taken.
    pub sif_dma: VecDeque<SifTransfer>,
    next_sif_dma: u32,
}

/// One SIF DMA transfer from EE to IOP memory.
#[derive(Clone, Debug)]
pub struct SifTransfer {
    /// Source in EE memory.
    pub src: u32,
    /// Destination in IOP memory.
    pub dest: u32,
    /// Bytes.
    pub size: u32,
    /// Attributes (bit 2: interrupt the IOP when done).
    pub attr: u32,
}

fn arg(cpu: &Cpu, n: u8) -> u32 {
    cpu.r32(4 + n)
}

fn ret(cpu: &mut Cpu, value: i64) {
    cpu.set64(2, value.cast_unsigned());
}

impl Kernel {
    /// Whether interrupt handlers are running.
    #[must_use]
    pub fn in_interrupt(&self) -> bool {
        self.interrupt.is_some()
    }

    /// Creates the main thread for a program about to start at the entry
    /// point, as `LoadExecPS2` leaves things.
    pub fn boot(&mut self) {
        self.threads = vec![
            None,
            Some(Thread {
                status: Status::Ready,
                priority: 0,
                initial_priority: 0,
                entry: 0,
                stack: 0,
                stack_size: 0,
                gp: 0,
                wakeups: 0,
                context: None,
            }),
        ];
        self.current = Some(1);
        self.semas = vec![None];
        self.next_handler_id = 1;
        self.next_alarm_id = 1;
        self.next_sif_dma = 1;
    }

    /// Runs the system call in `$v1`. `cpu.pc` already points past the
    /// `syscall`.
    #[allow(clippy::too_many_lines, reason = "one arm per call")]
    #[allow(
        clippy::match_same_arms,
        reason = "calls grouped by meaning, each with its comment"
    )]
    pub fn syscall(&mut self, cpu: &mut Cpu, mem: &mut Memory, hw: &mut Hardware) -> Effect {
        let number = cpu.r32(3) as i16;
        *self.calls.entry(number).or_default() += 1;
        let name = syscall_name(number).unwrap_or("?");
        let (a0, a1, a2, a3) = (arg(cpu, 0), arg(cpu, 1), arg(cpu, 2), arg(cpu, 3));
        tracing::trace!(target: "ee::syscall", "{name} ({number:#x}) a0={a0:#x} a1={a1:#x} a2={a2:#x} a3={a3:#x} from {:#x}", cpu.r32(31));
        if self.calls[&number] == 1 {
            tracing::debug!(target: "ee::syscall", "first {name} ({number:#x}) a0={a0:#x} a1={a1:#x} a2={a2:#x} a3={a3:#x}");
        }
        let current = self.current.unwrap_or(0);
        match number.unsigned_abs() {
            0x01 | 0x02 | 0x0D | 0x0E | 0x0F | 0x64 | 0x68 => ret(cpu, 0),
            0x04 => return Effect::Exit(a0 as i32),
            0x10 | 0x12 => {
                let id = self.next_handler_id;
                self.next_handler_id += 1;
                let handler = Handler {
                    id,
                    cause: a0,
                    address: a1,
                    arg: a3,
                    enabled: true,
                };
                let list = if number.unsigned_abs() == 0x10 {
                    &mut self.intc
                } else {
                    &mut self.dmac
                };
                // `next` < 0 appends; otherwise the handler goes first.
                if a2 as i32 >= 0 {
                    list.insert(0, handler)
                } else {
                    list.push(handler)
                }
                ret(cpu, id.into());
            }
            0x11 | 0x13 => {
                let list = if number.unsigned_abs() == 0x11 {
                    &mut self.intc
                } else {
                    &mut self.dmac
                };
                list.retain(|h| h.id != a1);
                ret(cpu, 0);
            }
            0x14 | 0x1A => {
                let was = hw.intc_mask & (1 << a0) != 0;
                hw.intc_mask |= 1 << a0;
                ret(cpu, i64::from(!was));
            }
            0x15 | 0x1B => {
                let was = hw.intc_mask & (1 << a0) != 0;
                hw.intc_mask &= !(1 << a0);
                ret(cpu, i64::from(was));
            }
            0x16 | 0x1C => {
                let was = hw.d_stat & (1 << (16 + a0)) != 0;
                hw.d_stat |= 1 << (16 + a0);
                ret(cpu, i64::from(!was));
            }
            0x17 | 0x1D => {
                let was = hw.d_stat & (1 << (16 + a0)) != 0;
                hw.d_stat &= !(1 << (16 + a0));
                ret(cpu, i64::from(was));
            }
            0x5C..=0x5F => {
                let list = if number.unsigned_abs() <= 0x5D {
                    &mut self.intc
                } else {
                    &mut self.dmac
                };
                let enable = number.unsigned_abs() & 1 == 0;
                for h in list.iter_mut().filter(|h| h.cause == a0 && h.id == a1) {
                    h.enabled = enable;
                }
                ret(cpu, 0);
            }
            0x18 | 0x1E | 0xFC | 0xFD => {
                let id = self.next_alarm_id;
                self.next_alarm_id += 1;
                let due = cpu.cycles + u64::from(a0 & 0xFFFF) * crate::HSYNC_CYCLES;
                self.alarms.push(Alarm {
                    id,
                    due,
                    time: a0,
                    handler: a1,
                    arg: a2,
                });
                ret(cpu, id.into());
            }
            0x19 | 0x1F | 0xFE | 0xFF => {
                self.alarms.retain(|a| a.id != a0);
                ret(cpu, 0);
            }
            0x20 => {
                // ee_thread_t: status, func, stack, stack_size, gp_reg,
                // initial_priority, current_priority, attr, option.
                let field = |mem: &mut Memory, i: u32| mem.read32(a0 + 4 * i);
                let thread = Thread {
                    status: Status::Dormant,
                    priority: field(mem, 5),
                    initial_priority: field(mem, 5),
                    entry: field(mem, 1),
                    stack: field(mem, 2),
                    stack_size: field(mem, 3),
                    gp: field(mem, 4),
                    wakeups: 0,
                    context: None,
                };
                self.threads.push(Some(thread));
                ret(cpu, (self.threads.len() - 1) as i64);
            }
            0x21 => {
                if let Some(slot) = self.threads.get_mut(a0 as usize) {
                    *slot = None;
                }
                ret(cpu, a0.into());
            }
            0x22 => {
                let Some(Some(thread)) = self.threads.get_mut(a0 as usize) else {
                    ret(cpu, -1);
                    return Effect::Continue;
                };
                let mut gpr = [0u128; 32];
                gpr[4] = u128::from(a1);
                gpr[28] = u128::from(thread.gp);
                gpr[29] = u128::from(thread.stack + thread.stack_size - 16);
                gpr[31] = u128::from(THREAD_EXIT);
                thread.context = Some(Context {
                    gpr,
                    hi: 0,
                    lo: 0,
                    sa: 0,
                    pc: thread.entry,
                    fpr: [0; 32],
                    fpu_acc: 0,
                    fcr31: 0,
                });
                thread.status = Status::Ready;
                thread.priority = thread.initial_priority;
                ret(cpu, a0.into());
                self.schedule(cpu);
            }
            0x23 | 0x24 => {
                if let Some(Some(thread)) = self.threads.get_mut(current) {
                    thread.status = Status::Dormant;
                }
                if number.unsigned_abs() == 0x24 {
                    self.threads[current] = None;
                }
                self.current = None;
                self.schedule(cpu);
            }
            0x25 | 0x26 => {
                if let Some(Some(thread)) = self.threads.get_mut(a0 as usize) {
                    thread.status = Status::Dormant;
                }
                ret(cpu, a0.into());
            }
            0x29 | 0x2A => {
                let id = if a0 == 0 { current } else { a0 as usize };
                let old = self
                    .threads
                    .get_mut(id)
                    .and_then(Option::as_mut)
                    .map_or(-1, |t| {
                        let old = t.priority;
                        t.priority = a1;
                        i64::from(old)
                    });
                ret(cpu, old);
                if number > 0 {
                    self.schedule(cpu);
                }
            }
            0x2B | 0x2C => {
                // Move the running thread behind others of its priority.
                if number > 0 {
                    self.rotate(cpu, a0);
                }
                ret(cpu, 0);
            }
            0x2F => ret(cpu, current as i64),
            0x30 | 0x31 => {
                let id = if a0 == 0 { current } else { a0 as usize };
                if let Some(Some(t)) = self.threads.get(id) {
                    let status = match t.status {
                        Status::Ready if Some(id) == self.current => 0x01,
                        Status::Ready => 0x02,
                        Status::WaitSema(_) | Status::Sleeping => 0x04,
                        Status::Suspended => 0x08,
                        Status::Dormant => 0x10,
                    };
                    for (i, v) in [
                        status,
                        t.entry,
                        t.stack,
                        t.stack_size,
                        t.gp,
                        t.initial_priority,
                        t.priority,
                        0,
                        0,
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        mem.write32(a1 + 4 * i as u32, v);
                    }
                }
                ret(cpu, 0);
            }
            0x32 => {
                if let Some(Some(thread)) = self.threads.get_mut(current) {
                    if thread.wakeups > 0 {
                        thread.wakeups -= 1;
                    } else {
                        thread.status = Status::Sleeping;
                    }
                }
                ret(cpu, current as i64);
                self.schedule(cpu);
            }
            0x33 | 0x34 => {
                if let Some(Some(thread)) = self.threads.get_mut(a0 as usize) {
                    if thread.status == Status::Sleeping {
                        thread.status = Status::Ready;
                    } else {
                        thread.wakeups += 1;
                    }
                }
                ret(cpu, a0.into());
                if number > 0 {
                    self.schedule(cpu);
                }
            }
            0x35 | 0x36 => {
                let count = self
                    .threads
                    .get_mut(a0 as usize)
                    .and_then(Option::as_mut)
                    .map_or(0, |t| std::mem::take(&mut t.wakeups));
                ret(cpu, count.into());
            }
            0x37 | 0x38 => {
                if let Some(Some(thread)) = self.threads.get_mut(a0 as usize) {
                    thread.status = Status::Suspended;
                }
                ret(cpu, a0.into());
                self.schedule(cpu);
            }
            0x39 | 0x3A => {
                if let Some(Some(thread)) = self.threads.get_mut(a0 as usize)
                    && thread.status == Status::Suspended
                {
                    thread.status = Status::Ready;
                }
                ret(cpu, a0.into());
                if number > 0 {
                    self.schedule(cpu);
                }
            }
            0x3C => {
                // SetupThread(gp, stack, stack_size, args, root): returns the
                // main thread's stack pointer and fills argc/argv.
                let size = a2;
                let stack = if a1 as i32 == -1 {
                    crate::RAM_TOP - size
                } else {
                    a1
                };
                if a3 != 0 {
                    mem.write32(a3, 1);
                    mem.write32(a3 + 4, ARGS);
                }
                if let Some(Some(main)) = self.threads.get_mut(current) {
                    (main.gp, main.stack, main.stack_size) = (a0, stack, size);
                }
                ret(cpu, i64::from(stack + size - 16));
            }
            0x3D => {
                self.heap_end = if a1 as i32 == -1 {
                    cpu.r32(29).saturating_sub(0x1_0000)
                } else {
                    a0 + a1
                };
                ret(cpu, self.heap_end.into());
            }
            0x3E => ret(cpu, self.heap_end.into()),
            0x40 => {
                // ee_sema_t: count, max_count, init_count, wait_threads, attr, option.
                let field = |mem: &mut Memory, i: u32| mem.read32(a0 + 4 * i) as i32;
                let sema = Sema {
                    count: field(mem, 2),
                    max: field(mem, 1),
                    init: field(mem, 2),
                    attr: field(mem, 4) as u32,
                    option: field(mem, 5) as u32,
                    waiters: VecDeque::new(),
                };
                self.semas.push(Some(sema));
                ret(cpu, (self.semas.len() - 1) as i64);
            }
            0x41 | 0x49 => {
                if let Some(slot) = self.semas.get_mut(a0 as usize) {
                    *slot = None;
                }
                ret(cpu, a0.into());
            }
            0x42 | 0x43 => {
                let woken = self.signal(a0 as usize);
                ret(cpu, a0.into());
                if woken && number > 0 {
                    self.schedule(cpu);
                }
            }
            0x44 => {
                let Some(Some(sema)) = self.semas.get_mut(a0 as usize) else {
                    ret(cpu, -1);
                    return Effect::Continue;
                };
                ret(cpu, a0.into());
                if sema.count > 0 {
                    sema.count -= 1;
                } else {
                    sema.waiters.push_back(current);
                    if let Some(Some(thread)) = self.threads.get_mut(current) {
                        thread.status = Status::WaitSema(a0 as usize);
                    }
                    self.schedule(cpu);
                }
            }
            0x45 | 0x46 => {
                let result = match self.semas.get_mut(a0 as usize) {
                    Some(Some(sema)) if sema.count > 0 => {
                        sema.count -= 1;
                        i64::from(a0)
                    }
                    _ => -1,
                };
                ret(cpu, result);
            }
            0x47 | 0x48 => {
                if let Some(Some(s)) = self.semas.get(a0 as usize) {
                    let values = [
                        s.count,
                        s.max,
                        s.init,
                        s.waiters.len() as i32,
                        s.attr as i32,
                        s.option as i32,
                    ];
                    for (i, v) in values.into_iter().enumerate() {
                        mem.write32(a1 + 4 * i as u32, v as u32);
                    }
                }
                ret(cpu, a0.into());
            }
            0x4B => {
                // OSD configuration: English, 4:3, RGB; everything else 0.
                mem.write32(a0, 1 << 16);
                ret(cpu, 0);
            }
            0x4C | 0x4D => ret(cpu, 0),
            0x4A | 0x4E | 0x4F => ret(cpu, 0),
            0x6E | 0x6F => {
                // OSD configuration, second block: all zero.
                for i in 0..a1 {
                    if number.unsigned_abs() == 0x6F {
                        mem.write8(a0 + i, 0);
                    }
                }
                ret(cpu, 0);
            }
            0x75 => {
                let mut text = Vec::new();
                for i in 0..512 {
                    let b = mem.read8(a0 + i);
                    if b == 0 {
                        break;
                    }
                    text.push(b);
                }
                tracing::info!(target: "ee::print", "{}", String::from_utf8_lossy(&text).trim_end());
                ret(cpu, 0);
            }
            0x80 => {
                mem.write32(a0, 0);
                mem.write32(a0 + 4, 0);
                ret(cpu, 0);
            }
            0x2D | 0x2E => {
                for sema in self.semas.iter_mut().flatten() {
                    sema.waiters.retain(|&w| w != a0 as usize);
                }
                if let Some(Some(thread)) = self.threads.get_mut(a0 as usize)
                    && matches!(thread.status, Status::WaitSema(_) | Status::Sleeping)
                {
                    thread.status = Status::Ready;
                }
                ret(cpu, a0.into());
                if number > 0 {
                    self.schedule(cpu);
                }
            }
            // SBUS interrupt handlers, the CPU timer, TLB, caches, the
            // scratchpad, PS mode: nothing to do, success.
            0x05
            | 0x08
            | 0x0A
            | 0x0B
            | 0x0C
            | 0x55..=0x59
            | 0x60..=0x62
            | 0x66
            | 0x67
            | 0x6B..=0x6D
            | 0x7D => ret(cpu, 0),
            0x63 => ret(cpu, i64::from(cpu.cop0[a0 as usize & 31])),
            0x70 => ret(cpu, hw.gs_imr as i64),
            0x71 => {
                let old = hw.gs_imr;
                hw.gs_imr = u64::from(a0);
                ret(cpu, old as i64);
            }
            0x73 => {
                self.vsync_flag = Some((a0, a1));
                ret(cpu, 0);
            }
            0x76 => ret(cpu, -1),
            0x77 => {
                // SifSetDma(transfers, count): each {src, dest, size, attr}.
                for i in 0..a1 {
                    let at = a0 + 16 * i;
                    let t = SifTransfer {
                        src: mem.read32(at),
                        dest: mem.read32(at + 4),
                        size: mem.read32(at + 8),
                        attr: mem.read32(at + 12),
                    };
                    for offset in (0..t.size).step_by(4) {
                        let word = mem.read32(t.src + offset);
                        let dest = (t.dest + offset) & 0x001F_FFFF;
                        mem.regions.iop_ram[dest as usize..dest as usize + 4]
                            .copy_from_slice(&word.to_le_bytes());
                    }
                    self.sif_dma.push_back(t);
                }
                let id = self.next_sif_dma;
                self.next_sif_dma += 1;
                ret(cpu, id.into());
            }
            0x78 => ret(cpu, 0),
            0x79 => {
                self.set_sif_reg(hw, a0, a1);
                ret(cpu, 0);
            }
            0x7A => ret(cpu, i64::from(self.sif_reg(hw, a0))),
            0x7C => ret(cpu, 0),
            // Kernel patches (SetSyscall installing a replacement, FindAddress
            // locating kernel code): the HLE kernel has nothing to patch.
            0x74 | 0x83 => ret(cpu, 0),
            // GetEntryAddress(syscall): where its handler lives. Kernel patches
            // chain to it; nothing runs there in the HLE kernel.
            0x5B => ret(cpu, i64::from(0x8000_2000 + 16 * (a0 & 0xFF))),
            0x5A => {
                // Copy(dest, src, size): the patch's code into kernel memory,
                // never over the HLE kernel's own (interrupt stack, argv).
                let physical = |a: u32| a & 0x1FFF_FFFF;
                let reserved = (INTERRUPT_STACK - 0x4000)..(ARGS + 0x400);
                let range = physical(a0)..physical(a0).saturating_add(a2);
                if range.start < reserved.end && reserved.start < range.end {
                    tracing::warn!(target: "ee::kernel", "Copy over the HLE kernel's memory refused: {a0:#x}+{a2:#x}");
                    ret(cpu, -1);
                    return Effect::Continue;
                }
                for i in 0..a2 {
                    let byte = mem.read8(a1 + i);
                    mem.write8(a0 + i, byte);
                }
                ret(cpu, 0);
            }
            0x7E => ret(cpu, 0),
            0x7F => ret(cpu, crate::RAM_TOP.into()),
            _ => {
                ret(cpu, 0);
                return Effect::Unimplemented(format!("syscall {name} ({number:#x})"));
            }
        }
        Effect::Continue
    }

    /// `sceSifGetReg`: hardware mailboxes for 1..=4, the kernel's software
    /// registers for the rest.
    fn sif_reg(&self, hw: &Hardware, reg: u32) -> u32 {
        match reg {
            1..=4 => hw.sbus[reg as usize - 1],
            _ => self.sif_regs.get(&reg).copied().unwrap_or(0),
        }
    }

    fn set_sif_reg(&mut self, hw: &mut Hardware, reg: u32, value: u32) {
        match reg {
            1 => hw.sbus[0] = value,
            2 => hw.sbus[1] = value,
            3 => hw.sbus[2] |= value,
            4 => hw.sbus[3] &= !value,
            _ => {
                self.sif_regs.insert(reg, value);
            }
        }
    }

    /// Signals a semaphore: wakes its first waiter or counts up. Returns
    /// whether a thread was woken.
    pub fn signal(&mut self, id: usize) -> bool {
        let Some(Some(sema)) = self.semas.get_mut(id) else {
            return false;
        };
        if let Some(waiter) = sema.waiters.pop_front() {
            if let Some(Some(thread)) = self.threads.get_mut(waiter) {
                thread.status = Status::Ready;
            }
            true
        } else {
            if sema.count < sema.max.max(1) {
                sema.count += 1;
            }
            false
        }
    }

    /// The highest-priority ready thread; the running one wins ties.
    fn pick(&self) -> Option<usize> {
        let ready = |id: usize| matches!(&self.threads[id], Some(t) if t.status == Status::Ready);
        let mut best = self.current.filter(|&id| ready(id));
        for id in 1..self.threads.len() {
            if !ready(id) {
                continue;
            }
            let priority = self.threads[id].as_ref().map_or(u32::MAX, |t| t.priority);
            let best_priority = best
                .and_then(|b| self.threads[b].as_ref())
                .map_or(u32::MAX, |t| t.priority);
            if priority < best_priority {
                best = Some(id);
            }
        }
        best
    }

    /// Switches to the thread that should run (outside interrupt context).
    pub fn schedule(&mut self, cpu: &mut Cpu) {
        if self.interrupt.is_some() {
            return;
        }
        let next = self.pick();
        if next == self.current {
            return;
        }
        if let Some(current) = self.current
            && let Some(Some(thread)) = self.threads.get_mut(current)
        {
            thread.context = Some(Context::save(cpu));
        }
        self.current = next;
        if let Some(id) = next {
            tracing::trace!(target: "ee::kernel", "switch to thread {id}");
            if let Some(Some(thread)) = self.threads.get(id)
                && let Some(context) = &thread.context
            {
                context.restore(cpu);
            }
        }
    }

    /// Moves the running thread behind the other ready threads of
    /// `priority`.
    fn rotate(&mut self, cpu: &mut Cpu, priority: u32) {
        let Some(current) = self.current else { return };
        let same: Vec<usize> = (1..self.threads.len())
            .filter(|&id| id != current && matches!(&self.threads[id], Some(t) if t.status == Status::Ready && t.priority == priority))
            .collect();
        if let Some(&next) = same.first() {
            if let Some(Some(thread)) = self.threads.get_mut(current) {
                thread.context = Some(Context::save(cpu));
            }
            self.current = Some(next);
            if let Some(Some(thread)) = self.threads.get(next)
                && let Some(context) = &thread.context
            {
                context.restore(cpu);
            }
        }
    }

    /// The cycle of the next alarm, if any.
    #[must_use]
    pub fn next_alarm(&self) -> Option<u64> {
        self.alarms.iter().map(|a| a.due).min()
    }

    /// Starts servicing pending interrupts: INTC causes, DMAC channels and
    /// due alarms, each acknowledged and its handlers queued. Returns false
    /// if nothing was pending.
    pub fn begin_interrupt(&mut self, cpu: &mut Cpu, hw: &mut Hardware) -> bool {
        let mut queue = VecDeque::new();
        let intc = hw.intc_pending();
        for cause in (0..15).filter(|c| intc & (1 << c) != 0) {
            hw.intc_stat &= !(1 << cause);
            for h in self.intc.iter().filter(|h| h.cause == cause && h.enabled) {
                queue.push_back((h.address, [cause, h.arg, 0]));
            }
        }
        let dmac = hw.dmac_pending();
        for channel in (0..10).filter(|c| dmac & (1 << c) != 0) {
            hw.d_stat &= !(1 << channel);
            for h in self.dmac.iter().filter(|h| h.cause == channel && h.enabled) {
                queue.push_back((h.address, [channel, h.arg, 0]));
            }
        }
        let now = cpu.cycles;
        let due: Vec<Alarm> = self
            .alarms
            .iter()
            .filter(|a| a.due <= now)
            .cloned()
            .collect();
        self.alarms.retain(|a| a.due > now);
        for alarm in due {
            queue.push_back((alarm.handler, [alarm.id, alarm.time, alarm.arg]));
        }
        if queue.is_empty() {
            return false;
        }
        let saved = self.current.map(|_| Context::save(cpu));
        self.interrupt = Some(Interrupt { saved, queue });
        self.next_handler(cpu);
        true
    }

    /// Starts the next queued handler, or ends the interrupt. Called when a
    /// handler returns to [`INTERRUPT_RETURN`].
    pub fn next_handler(&mut self, cpu: &mut Cpu) {
        let Some(interrupt) = self.interrupt.as_mut() else {
            return;
        };
        if let Some((address, args)) = interrupt.queue.pop_front() {
            cpu.pc = address;
            for (i, a) in args.into_iter().enumerate() {
                cpu.set32(4 + i as u8, a);
            }
            cpu.set64(29, u64::from(INTERRUPT_STACK));
            cpu.set64(31, u64::from(INTERRUPT_RETURN));
            return;
        }
        let interrupt = self.interrupt.take();
        if let Some(Some(saved)) = interrupt.map(|i| i.saved) {
            saved.restore(cpu);
        }
        // Handlers may have woken a higher-priority thread.
        self.schedule(cpu);
    }

    /// The `SetVSyncFlag` addresses, written at each V-blank.
    #[must_use]
    pub fn vsync_flag(&self) -> Option<(u32, u32)> {
        self.vsync_flag
    }

    /// A thread returned from its entry function.
    pub fn thread_returned(&mut self, cpu: &mut Cpu) {
        if let Some(current) = self.current
            && let Some(Some(thread)) = self.threads.get_mut(current)
        {
            thread.status = Status::Dormant;
        }
        self.current = None;
        self.schedule(cpu);
    }
}
