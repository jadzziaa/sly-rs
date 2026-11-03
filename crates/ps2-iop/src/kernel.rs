//! The IOP kernel's objects, high-level emulated: threads with strict
//! priorities, semaphores, event flags, alarms on the IOP clock, and the
//! `sysmem` heap.
//!
//! A thread is a saved register set, as on the EE: switching threads swaps
//! the [`IopCpu`] contents. Priorities run from 1 (highest) to 126; each has
//! a FIFO ready queue. A thread made ready that outranks the running one
//! takes over at once; the one it preempts goes back to the head of its
//! queue. Semantics and error codes follow Sony's `threadman` as ps2sdk
//! documents it (`thbase.h`, `thsemap.h`, `thevent.h`, `kerr.h`).

use std::collections::{BTreeMap, VecDeque};

use ps2_core::iop::IopCpu;

/// Where a thread goes when its entry function returns.
pub const THREAD_EXIT: u32 = 0x0000_1000;
/// Where a module's start function returns to (the loader).
pub const MODULE_RETURN: u32 = 0x0000_1008;
/// Where a callback the kernel makes into guest code returns to.
pub const CALLBACK_RETURN: u32 = 0x0000_1010;
/// Where a thread in `sceSifRpcLoop` waits for its next request.
pub const RPC_LOOP: u32 = 0x0000_1018;
/// Where an RPC server function returns to.
pub const RPC_RETURN: u32 = 0x0000_1020;

/// Kernel error codes (`kerr.h`).
pub mod kerr {
    /// Interrupts already disabled.
    pub const CPUDI: i32 = -102;
    /// Illegal priority.
    pub const ILLEGAL_PRIORITY: i32 = -403;
    /// Unknown thread id.
    pub const UNKNOWN_THID: i32 = -407;
    /// Unknown semaphore id.
    pub const UNKNOWN_SEMID: i32 = -408;
    /// Unknown event flag id.
    pub const UNKNOWN_EVFID: i32 = -409;
    /// The thread is not dormant.
    pub const NOT_DORMANT: i32 = -414;
    /// The semaphore's count is zero.
    pub const SEMA_ZERO: i32 = -419;
    /// The semaphore's count would exceed its maximum.
    pub const SEMA_OVF: i32 = -420;
    /// The event flag's condition does not hold.
    pub const EVF_COND: i32 = -421;
    /// Another thread already waits on the event flag.
    pub const EVF_MULTI: i32 = -422;
    /// The wait pattern is zero.
    pub const EVF_ILPAT: i32 = -423;
    /// The object waited on was deleted.
    pub const WAIT_DELETE: i32 = -425;
    /// Not enough memory.
    pub const NO_MEMORY: i32 = -400;
}

/// Event flag wait modes.
pub mod wef {
    /// Any of the bits (otherwise all of them).
    pub const OR: u32 = 1;
    /// Clear the whole flag when the wait ends.
    pub const CLEAR: u32 = 0x10;
    /// Clear the waited bits when the wait ends.
    pub const CLEAR_PATTERN: u32 = 0x20;
}

/// The registers a thread starts with: at `pc`, with `args` in `$a0`...,
/// `$gp`, the stack pointer and the return address.
#[must_use]
pub fn start_context(pc: u32, args: &[u32], gp: u32, sp: u32, ra: u32) -> IopCpu {
    let mut gpr = [0; 32];
    for (i, &a) in args.iter().take(4).enumerate() {
        gpr[4 + i] = a;
    }
    gpr[28] = gp;
    gpr[29] = sp;
    gpr[31] = ra;
    IopCpu {
        gpr,
        pc,
        ..IopCpu::default()
    }
}

/// A thread's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Created, not started (or finished).
    Dormant,
    /// Waiting for the processor.
    Ready,
    /// On the processor.
    Running,
    /// Waiting for something.
    Waiting,
}

/// What a waiting thread waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// Nothing.
    None,
    /// A semaphore.
    Sema(usize),
    /// An event flag: the bits, the mode, where to store the flag.
    Event {
        /// Flag index.
        flag: usize,
        /// Pattern.
        bits: u32,
        /// `wef` mode.
        mode: u32,
        /// Where to store the flag's value, if anywhere.
        result: u32,
    },
    /// `SleepThread`.
    Sleep,
    /// `DelayThread`.
    Delay,
    /// Requests on an RPC queue.
    Rpc(u32),
    /// Something the machine completes (a disc read, an RPC to the EE).
    Machine,
}

/// A thread.
#[derive(Clone, Debug)]
pub struct Thread {
    /// Current priority.
    pub priority: u32,
    /// Priority at creation.
    pub init_priority: u32,
    /// State.
    pub status: Status,
    /// What it waits for.
    pub wait: Wait,
    /// Entry point.
    pub entry: u32,
    /// Stack base and size.
    pub stack: (u32, u32),
    /// `$gp` its code runs with.
    pub gp: u32,
    /// Attributes and option word from creation.
    pub attr: (u32, u32),
    /// Pending `WakeupThread` calls.
    pub wakeups: u32,
    /// Saved registers while not running.
    pub context: IopCpu,
}

/// A semaphore.
#[derive(Clone, Debug)]
pub struct Sema {
    /// Count.
    pub count: i32,
    /// Maximum count.
    pub max: i32,
    /// Attributes (bit 0: wake waiters by priority, not arrival).
    pub attr: u32,
    /// Waiting threads, in arrival order.
    pub waiters: VecDeque<usize>,
}

/// An event flag.
#[derive(Clone, Debug)]
pub struct EventFlag {
    /// Its bits.
    pub bits: u32,
    /// Attributes (bit 1: several threads may wait).
    pub attr: u32,
    /// Waiting threads.
    pub waiters: Vec<usize>,
}

/// A first-fit allocator over IOP RAM (`sysmem`), 256-byte granules.
#[derive(Clone, Debug)]
pub struct Heap {
    /// Blocks in address order: start, size, in use.
    blocks: Vec<(u32, u32, bool)>,
}

impl Heap {
    /// A heap over `[start, end)`.
    #[must_use]
    pub fn new(start: u32, end: u32) -> Self {
        Self {
            blocks: vec![(start, end - start, false)],
        }
    }

    /// Allocates `size` bytes: lowest free address (`mode` 0), highest
    /// (`mode` 1), or exactly at `at` (`mode` 2).
    pub fn alloc(&mut self, mode: u32, size: u32, at: u32) -> Option<u32> {
        let size = size.checked_add(0xFF)? & !0xFF;
        let size = size.max(0x100);
        let pick = match mode {
            1 => self.blocks.iter().rposition(|b| !b.2 && b.1 >= size),
            2 => self
                .blocks
                .iter()
                .position(|b| !b.2 && b.0 <= at && at + size <= b.0 + b.1),
            _ => self.blocks.iter().position(|b| !b.2 && b.1 >= size),
        }?;
        let (start, len, _) = self.blocks[pick];
        let place = match mode {
            1 => start + len - size,
            2 => at & !0xFF,
            _ => start,
        };
        let mut parts = Vec::new();
        if place > start {
            parts.push((start, place - start, false));
        }
        parts.push((place, size, true));
        if place + size < start + len {
            parts.push((place + size, start + len - place - size, false));
        }
        self.blocks.splice(pick..=pick, parts);
        Some(place)
    }

    /// Frees the block at `address`; false if there is none.
    pub fn free(&mut self, address: u32) -> bool {
        let Some(i) = self.blocks.iter().position(|b| b.0 == address && b.2) else {
            return false;
        };
        self.blocks[i].2 = false;
        // Merge with free neighbours.
        if i + 1 < self.blocks.len() && !self.blocks[i + 1].2 {
            self.blocks[i].1 += self.blocks[i + 1].1;
            self.blocks.remove(i + 1);
        }
        if i > 0 && !self.blocks[i - 1].2 {
            self.blocks[i - 1].1 += self.blocks[i].1;
            self.blocks.remove(i);
        }
        true
    }

    /// The largest free block.
    #[must_use]
    pub fn max_free(&self) -> u32 {
        self.blocks
            .iter()
            .filter(|b| !b.2)
            .map(|b| b.1)
            .max()
            .unwrap_or(0)
    }

    /// All free memory.
    #[must_use]
    pub fn total_free(&self) -> u32 {
        self.blocks.iter().filter(|b| !b.2).map(|b| b.1).sum()
    }
}

/// The kernel's state.
#[derive(Clone, Debug)]
pub struct Kernel {
    /// Threads by id − 1.
    pub threads: Vec<Option<Thread>>,
    /// The running thread.
    pub current: Option<usize>,
    /// Ready queues by priority.
    ready: Vec<VecDeque<usize>>,
    /// Bit `p` set while the ready queue of priority `p` is not empty.
    occupied: u128,
    /// Semaphores by id − 1.
    pub semas: Vec<Option<Sema>>,
    /// Event flags by id − 1.
    pub flags: Vec<Option<EventFlag>>,
    /// Threads to wake, by (cycle, order of setting).
    alarms: BTreeMap<(u64, u64), usize>,
    alarm_order: u64,
    /// Interrupts enabled (`CpuSuspendIntr` and friends).
    pub interrupts: bool,
    /// A callback runs in interrupt context: no thread switches until it
    /// returns.
    pub in_interrupt: bool,
    /// IOP RAM not used by the kernel.
    pub heap: Heap,
}

/// The lowest IOP RAM the heap hands out; below are the SIF packet buffer
/// and the Rust servers' receive buffers.
pub const HEAP_START: u32 = 0x0008_0000;
/// The end of IOP RAM.
pub const HEAP_END: u32 = 0x0020_0000;

impl Default for Kernel {
    fn default() -> Self {
        Self {
            threads: Vec::new(),
            current: None,
            ready: vec![VecDeque::new(); 128],
            occupied: 0,
            semas: Vec::new(),
            flags: Vec::new(),
            alarms: BTreeMap::new(),
            alarm_order: 0,
            interrupts: true,
            in_interrupt: false,
            heap: Heap::new(HEAP_START, HEAP_END),
        }
    }
}

impl Kernel {
    /// A thread by id.
    pub fn thread(&mut self, id: u32) -> Option<&mut Thread> {
        self.threads
            .get_mut((id as usize).checked_sub(1)?)
            .and_then(Option::as_mut)
    }

    /// Adds a thread; returns its id.
    pub fn add_thread(&mut self, thread: Thread) -> u32 {
        self.threads.push(Some(thread));
        self.threads.len() as u32
    }

    /// Puts thread `t` in its ready queue (at the back, or the front for a
    /// preempted thread).
    pub fn make_ready(&mut self, t: usize, front: bool) {
        let Some(thread) = self.threads[t].as_mut() else {
            return;
        };
        if thread.status == Status::Ready {
            return;
        }
        thread.status = Status::Ready;
        thread.wait = Wait::None;
        let priority = thread.priority.min(127);
        let queue = &mut self.ready[priority as usize];
        if front {
            queue.push_front(t);
        } else {
            queue.push_back(t);
        }
        self.occupied |= 1 << priority;
    }

    /// Ends thread `t`'s wait: it becomes ready with `result` in `$v0`.
    pub fn wake(&mut self, t: usize, result: i32) {
        if let Some(thread) = self.threads[t].as_mut() {
            thread.context.gpr[2] = result.cast_unsigned();
        }
        self.make_ready(t, false);
    }

    /// Takes thread `t` out of the ready queues.
    fn unready(&mut self, t: usize) {
        for (p, queue) in self.ready.iter_mut().enumerate() {
            queue.retain(|&x| x != t);
            if queue.is_empty() {
                self.occupied &= !(1 << p);
            }
        }
    }

    /// The highest priority with a ready thread.
    fn best(&self) -> Option<u32> {
        (self.occupied != 0).then(|| self.occupied.trailing_zeros())
    }

    /// Whether any thread is ready or running.
    #[must_use]
    pub fn runnable(&self) -> bool {
        self.current.is_some() || self.best().is_some()
    }

    /// Lets the highest-priority thread run: the running one keeps the
    /// processor unless a ready thread outranks it, or it stopped running.
    pub fn schedule(&mut self, cpu: &mut IopCpu) {
        if self.in_interrupt {
            return;
        }
        let current = self.current.and_then(|t| {
            let thread = self.threads[t].as_ref()?;
            Some((t, thread.status, thread.priority))
        });
        let best = self.best();
        match (current, best) {
            (Some((_, Status::Running, p)), Some(b)) if b < p => {}
            (Some((_, Status::Running, _)), _) => return,
            _ => {}
        }
        // Save the outgoing thread.
        if let Some((t, status, _)) = current {
            if let Some(thread) = self.threads[t].as_mut() {
                let cycles = thread.context.cycles;
                thread.context = cpu.clone();
                thread.context.cycles = cycles;
            }
            if status == Status::Running {
                self.make_ready(t, true);
            }
        }
        self.current = None;
        let next = self.best().and_then(|b| {
            let t = self.ready[b as usize].pop_front();
            if self.ready[b as usize].is_empty() {
                self.occupied &= !(1 << b);
            }
            t
        });
        if let Some(t) = next
            && let Some(thread) = self.threads[t].as_mut()
        {
            thread.status = Status::Running;
            let cycles = cpu.cycles;
            *cpu = thread.context.clone();
            cpu.cycles = cycles;
            self.current = Some(t);
        }
    }

    /// Makes the running thread wait for `wait`, resuming at `resume`.
    pub fn block(&mut self, cpu: &mut IopCpu, wait: Wait, resume: u32) {
        if let Some(t) = self.current
            && let Some(thread) = self.threads[t].as_mut()
        {
            cpu.pc = resume;
            thread.status = Status::Waiting;
            thread.wait = wait;
        }
    }

    /// Ends the running thread (its entry function returned).
    pub fn exit_current(&mut self) {
        if let Some(t) = self.current
            && let Some(thread) = self.threads[t].as_mut()
        {
            thread.status = Status::Dormant;
            thread.wait = Wait::None;
        }
        if let Some(t) = self.current {
            self.unready(t);
        }
    }

    /// Wakes thread `t` at `cycle`.
    pub fn set_alarm(&mut self, cycle: u64, t: usize) {
        self.alarm_order += 1;
        self.alarms.insert((cycle, self.alarm_order), t);
    }

    /// When the next alarm is due.
    #[must_use]
    pub fn next_alarm(&self) -> Option<u64> {
        self.alarms.keys().next().map(|k| k.0)
    }

    /// Wakes the threads whose alarms are due at `now`; true if any woke.
    pub fn fire_alarms(&mut self, now: u64) -> bool {
        let mut any = false;
        while let Some((&key, &t)) = self.alarms.iter().next() {
            if key.0 > now {
                break;
            }
            self.alarms.remove(&key);
            if self.threads[t]
                .as_ref()
                .is_some_and(|th| th.wait == Wait::Delay)
            {
                self.wake(t, 0);
                any = true;
            }
        }
        any
    }
}
