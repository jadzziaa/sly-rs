//! The IOP processor running modules: the interpreter over
//! `ps2_core::iop`, module loading and linking, and the calls modules make
//! into the high-level emulated kernel ([`crate::hle`]).
//!
//! Imports from kernel libraries are intercepted at their stubs: when the
//! PC reaches one, the library function runs in Rust and returns to `$ra`.
//! Imports from libraries other modules export (`989snd` uses `libsd`) are
//! linked as the real loader does: the stub becomes a jump to the function.

use std::collections::{BTreeMap, VecDeque};

use ps2_core::Regions;
use ps2_core::iop::{IopCpu, IopMemory, IopOutcome, execute};

use crate::hle::{self, Hle};
use crate::irx::{Irx, IrxError};
use crate::kernel::{
    self, CALLBACK_RETURN, Kernel, MODULE_RETURN, RPC_LOOP, RPC_RETURN, THREAD_EXIT, Thread, Wait,
};
use crate::spu2::IopHardware;

/// The priority a module's start function runs at (the loader's thread,
/// below the modules' own threads).
pub(crate) const LOADER_PRIORITY: u32 = 104;
/// IOP cycles an emulated kernel call costs.
pub(crate) const HLE_COST: u64 = 20;

/// A loaded module.
#[derive(Clone, Debug)]
pub struct Module {
    /// Its name.
    pub name: String,
    /// Where it was loaded from.
    pub path: String,
    /// Its base address and size.
    pub base: u32,
    /// Its size in memory.
    pub size: u32,
    /// What its start function returned, once it has.
    pub result: Option<u32>,
}

/// A module whose start function finished.
#[derive(Clone, Copy, Debug)]
pub struct Started {
    /// Which load (the token given to [`IopCore::load`]).
    pub token: u64,
    /// The module's id.
    pub id: u32,
    /// What its start function returned.
    pub result: u32,
}

/// A guest function the kernel calls from interrupt context.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Callback {
    pub(crate) func: u32,
    pub(crate) args: [u32; 4],
    pub(crate) gp: u32,
    pub(crate) kind: CallbackKind,
}

/// What happens when a callback returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallbackKind {
    /// Nothing.
    Plain,
    /// A timer handler: its result is the next compare value (0 stops).
    Timer(usize),
}

/// A hardware timer allocated with `AllocHardTimer`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HwTimer {
    /// IOP cycles per tick (source and prescale).
    pub(crate) cycles_per_tick: u64,
    pub(crate) compare: u32,
    pub(crate) handler: u32,
    pub(crate) arg: u32,
    pub(crate) gp: u32,
    /// When it next reaches its compare value, while running.
    pub(crate) due: Option<u64>,
}

/// An RPC server a module registered with `sceSifRegisterRpc`.
#[derive(Clone, Copy, Debug)]
pub struct GuestServer {
    /// Its function: `void *f(int function, void *buffer, int size)`.
    pub func: u32,
    /// Its receive buffer, where the EE's data arrives.
    pub buffer: u32,
    /// Its queue (`sceSifSetRpcQueue`).
    pub queue: u32,
}

/// An EE call to a guest server, until it is answered.
#[derive(Clone, Debug)]
pub struct RpcRequest {
    /// The server.
    pub sid: u32,
    /// The function number.
    pub function: u32,
    /// Bytes of data the EE sent.
    pub size: u32,
    /// The `RPC_END` packet's words 4..=7 and the EE's receive buffer and
    /// size.
    pub reply: ([u32; 4], u32, u32),
}

/// The IOP processor and its kernel.
pub struct IopCore {
    /// The CPU.
    pub cpu: IopCpu,
    scratchpad: Vec<u8>,
    /// Hardware: the SPU2 and its DMA channels.
    pub hw: IopHardware,
    /// The kernel.
    pub kernel: Kernel,
    /// Kernel import stubs, by address: library and function index.
    stubs: BTreeMap<u32, (String, u16)>,
    /// Libraries modules registered, by name: function addresses.
    libraries: BTreeMap<String, Vec<u32>>,
    /// Modules, by id − 1.
    pub modules: Vec<Module>,
    /// Loads whose start function runs, by loader thread.
    loading: BTreeMap<usize, (u64, u32)>,
    /// Start functions that finished, for the loader's replies.
    pub started: VecDeque<Started>,
    /// What modules printed.
    pub console: Vec<u8>,
    /// Kernel calls made, by library and function.
    pub calls: BTreeMap<(String, u16), u64>,
    /// Callbacks waiting to run.
    pub(crate) callbacks: VecDeque<Callback>,
    /// The state a running callback interrupted, and the callback's kind.
    interrupted: Option<(IopCpu, CallbackKind)>,
    /// The interrupt stack's top.
    interrupt_stack: u32,
    /// Hardware timers by id − 1.
    pub(crate) timers: Vec<Option<HwTimer>>,
    /// RPC servers modules registered, by id.
    pub servers: BTreeMap<u32, GuestServer>,
    /// RPC queues: the serving thread and the requests waiting.
    pub(crate) queues: BTreeMap<u32, (usize, VecDeque<RpcRequest>)>,
    /// The request each serving thread is answering.
    serving: BTreeMap<usize, RpcRequest>,
    /// Answered requests and the IOP address of their reply data.
    pub answered: VecDeque<(RpcRequest, u32)>,
    /// SIF DMA transfers made (their ids).
    pub(crate) dma_ids: u32,
    /// Interrupt handlers by line: function, argument, `$gp`.
    pub(crate) handlers: BTreeMap<u32, (u32, u32, u32)>,
    /// Interrupt lines enabled (the controller's mask: independent of the
    /// handlers, and set before them by `libsd`).
    pub(crate) enabled: std::collections::BTreeSet<u32>,
    /// The disc, for `cdvdman`'s reads into IOP memory.
    pub disc: Option<std::rc::Rc<dyn crate::cdvd::Sectors>>,
    /// `sceCdCallback`'s function and the `$gp` it runs with.
    pub(crate) cd_callback: (u32, u32),
    /// Sectors `cdvdman` read for modules.
    pub sectors_read: u64,
    /// The Rust 989snd, when it plays sound in place of the disc's driver.
    pub(crate) sound: Option<Box<ps2_snd989::Driver>>,
    /// When the Rust driver's next tick is due.
    pub(crate) sound_due: u64,
    /// The Rust driver's replies, waiting as long as the disc's driver
    /// would take.
    pub(crate) sound_replies: VecDeque<SoundReply>,
    /// Its replies whose time has come, for the SIF to send.
    pub(crate) sound_ready: VecDeque<SoundReply>,
}

/// A reply of the Rust 989snd: when it is due, the `RPC_END` packet and
/// the data for the EE.
pub(crate) type SoundReply = (u64, Vec<u32>, Option<(u32, Vec<u8>)>);

impl Default for IopCore {
    fn default() -> Self {
        Self::new()
    }
}

impl IopCore {
    /// An IOP with no modules.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cpu: IopCpu::default(),
            scratchpad: vec![0; 1024],
            hw: IopHardware::default(),
            kernel: Kernel::default(),
            stubs: BTreeMap::new(),
            libraries: BTreeMap::new(),
            modules: Vec::new(),
            loading: BTreeMap::new(),
            started: VecDeque::new(),
            console: Vec::new(),
            calls: BTreeMap::new(),
            callbacks: VecDeque::new(),
            interrupted: None,
            interrupt_stack: 0,
            timers: Vec::new(),
            servers: BTreeMap::new(),
            queues: BTreeMap::new(),
            serving: BTreeMap::new(),
            answered: VecDeque::new(),
            dma_ids: 0,
            handlers: BTreeMap::new(),
            enabled: std::collections::BTreeSet::new(),
            disc: None,
            cd_callback: (0, 0),
            sectors_read: 0,
            sound: None,
            sound_due: 0,
            sound_replies: VecDeque::new(),
            sound_ready: VecDeque::new(),
        }
    }

    /// Raises interrupt `line`: its handler runs, if one is registered and
    /// enabled.
    pub fn raise(&mut self, line: u32) {
        if !self.enabled.contains(&line) {
            tracing::debug!(target: "iop::intr", "line {line:#x} raised but disabled");
            return;
        }
        if let Some(&(func, arg, gp)) = self.handlers.get(&line) {
            self.callbacks.push_back(Callback {
                func,
                args: [arg, 0, 0, 0],
                gp,
                kind: CallbackKind::Plain,
            });
        }
    }

    /// Queues an EE call to a guest server and wakes its thread.
    pub fn request(&mut self, request: RpcRequest) -> bool {
        let Some(server) = self.servers.get(&request.sid).copied() else {
            return false;
        };
        let Some((t, queue)) = self.queues.get_mut(&server.queue) else {
            return false;
        };
        let t = *t;
        queue.push_back(request);
        if self.kernel.threads[t]
            .as_ref()
            .is_some_and(|th| th.wait == Wait::Rpc(server.queue))
        {
            self.kernel.wake(t, 0);
        }
        true
    }

    /// Whether server `sid`'s thread has nothing queued and is not
    /// answering a request (its receive buffer is free).
    pub(crate) fn rpc_idle(&self, sid: u32) -> bool {
        let Some(server) = self.servers.get(&sid) else {
            return false;
        };
        self.queues
            .get(&server.queue)
            .is_some_and(|(t, q)| q.is_empty() && !self.serving.contains_key(t))
    }

    /// Registers a library a module exports.
    pub fn register_library(&mut self, name: String, functions: Vec<u32>) {
        tracing::debug!(target: "iop::hle", "library {name} registered ({} functions)", functions.len());
        self.libraries.insert(name, functions);
    }

    /// Loads the IRX `data` (from `path`) into `ram`, links it and starts
    /// it: its start function runs in a loader thread, and [`Self::started`]
    /// receives `token` when it returns.
    ///
    /// # Errors
    ///
    /// If the module does not parse, does not fit, or imports something
    /// nothing provides.
    pub fn load(
        &mut self,
        ram: &mut [u8],
        data: &[u8],
        path: &str,
        token: u64,
    ) -> Result<u32, IrxError> {
        let irx = Irx::parse(data)?;
        let size = irx.image.len() as u32;
        let base = self
            .kernel
            .heap
            .alloc(0, size, 0)
            .ok_or_else(|| IrxError(format!("{path}: no room for {size} bytes")))?;
        let (mut image, imports) = irx.load(base)?;
        for import in &imports {
            if hle::provides(&import.library) {
                self.stubs
                    .insert(import.stub, (import.library.clone(), import.index));
            } else if let Some(functions) = self.libraries.get(&import.library) {
                let target = functions
                    .get(usize::from(import.index))
                    .copied()
                    .ok_or_else(|| {
                        IrxError(format!(
                            "{path}: {} has no function {}",
                            import.library, import.index
                        ))
                    })?;
                // j target, as the real loader links it.
                let at = (import.stub - base) as usize;
                let jump = 0x0800_0000 | ((target >> 2) & 0x03FF_FFFF);
                image[at..at + 4].copy_from_slice(&jump.to_le_bytes());
            } else {
                return Err(IrxError(format!(
                    "{path}: imports {} (function {}), which no module provides",
                    import.library, import.index
                )));
            }
        }
        let at = (base & 0x1F_FFFF) as usize;
        ram[at..at + image.len()].copy_from_slice(&image);
        self.modules.push(Module {
            name: irx.name.clone(),
            path: path.to_owned(),
            base,
            size,
            result: None,
        });
        let id = self.modules.len() as u32;
        tracing::debug!(target: "iop::hle", "module {} ({path}) at {base:#x}, {size} bytes, {} imports", irx.name, imports.len());

        // argv: { path, NULL } and the path, on the heap.
        let argv = self
            .kernel
            .heap
            .alloc(0, 8 + path.len() as u32 + 1, 0)
            .ok_or_else(|| IrxError("no room for arguments".to_owned()))?;
        let a = (argv & 0x1F_FFFF) as usize;
        ram[a..a + 4].copy_from_slice(&(argv + 8).to_le_bytes());
        ram[a + 4..a + 8].copy_from_slice(&0u32.to_le_bytes());
        ram[a + 8..a + 8 + path.len()].copy_from_slice(path.as_bytes());
        ram[a + 8 + path.len()] = 0;

        let stack_size = 0x1000;
        let stack = self
            .kernel
            .heap
            .alloc(1, stack_size, 0)
            .ok_or_else(|| IrxError("no room for the loader's stack".to_owned()))?;
        let context = kernel::start_context(
            base + irx.entry,
            &[1, argv],
            base + irx.gp,
            stack + stack_size - 16,
            MODULE_RETURN,
        );
        let thread = self.kernel.add_thread(Thread {
            priority: LOADER_PRIORITY,
            init_priority: LOADER_PRIORITY,
            status: kernel::Status::Dormant,
            wait: Wait::None,
            entry: base + irx.entry,
            stack: (stack, stack_size),
            gp: base + irx.gp,
            attr: (0, 0),
            wakeups: 0,
            context,
        });
        let t = thread as usize - 1;
        self.loading.insert(t, (token, id));
        self.kernel.make_ready(t, false);
        Ok(id)
    }

    /// Whether the IOP has something to run before `until` (IOP cycles).
    #[must_use]
    pub fn busy(&self, until: u64) -> bool {
        self.kernel.runnable()
            || self.interrupted.is_some()
            || !self.callbacks.is_empty()
            || self.next_alarm().is_some_and(|a| a <= until)
    }

    /// When the IOP next has something to do, if it is idle: an alarm or a
    /// timer.
    #[must_use]
    pub fn next_alarm(&self) -> Option<u64> {
        let timer = self.timers.iter().flatten().filter_map(|t| t.due).min();
        [
            self.kernel.next_alarm(),
            timer,
            self.hw.next_due(),
            self.sound_alarm(),
            self.sound_replies.front().map(|r| r.0),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Runs the SPU2 up to now and completes the DMA transfers due: their
    /// data moves and their interrupt line is raised; so is the SPU2's, if
    /// it asked.
    fn finish_transfers(&mut self, ram: &mut [u8]) {
        self.hw.now = self.cpu.cycles;
        for line in self.hw.finish_due(ram) {
            tracing::debug!(target: "iop::spu2", "cycle {}: DMA done, line {line:#x}", self.cpu.cycles);
            self.raise(line);
            self.sound_dma_done(line);
        }
        if self.hw.take_irq() {
            tracing::debug!(target: "iop::spu2", "cycle {}: SPU2 interrupt", self.cpu.cycles);
            self.raise(crate::spu2::SPU2_LINE);
        }
    }

    /// Queues the handlers of the timers due by now.
    fn fire_timers(&mut self) {
        let now = self.cpu.cycles;
        for (id, timer) in self.timers.iter_mut().enumerate() {
            if let Some(t) = timer
                && t.due.is_some_and(|due| due <= now)
            {
                t.due = None;
                self.callbacks.push_back(Callback {
                    func: t.handler,
                    args: [t.arg, 0, 0, 0],
                    gp: t.gp,
                    kind: CallbackKind::Timer(id),
                });
            }
        }
    }

    /// Starts the next callback, interrupting whatever runs.
    fn start_callback(&mut self, ram: &mut [u8]) {
        let Some(callback) = self.callbacks.pop_front() else {
            return;
        };
        tracing::debug!(target: "iop::intr", "cycle {}: callback {:#x} ({:?})", self.cpu.cycles, callback.func, callback.kind);
        if self.interrupt_stack == 0 {
            self.interrupt_stack = self
                .kernel
                .heap
                .alloc(1, 0x2000, 0)
                .map_or(0x0001_F000, |s| s + 0x2000);
        }
        let _ = ram;
        self.interrupted = Some((self.cpu.clone(), callback.kind));
        self.kernel.in_interrupt = true;
        let cycles = self.cpu.cycles;
        self.cpu = kernel::start_context(
            callback.func,
            &callback.args,
            callback.gp,
            self.interrupt_stack - 16,
            CALLBACK_RETURN,
        );
        self.cpu.cycles = cycles;
    }

    /// A callback returned: the interrupted state resumes.
    fn callback_returned(&mut self) {
        let result = self.cpu.gpr[2];
        tracing::debug!(target: "iop::intr", "cycle {}: callback returned {result:#x}", self.cpu.cycles);
        let Some((state, kind)) = self.interrupted.take() else {
            return;
        };
        let cycles = self.cpu.cycles;
        self.cpu = state;
        self.cpu.cycles = cycles;
        self.kernel.in_interrupt = false;
        if let CallbackKind::Timer(id) = kind
            && let Some(Some(timer)) = self.timers.get_mut(id)
            && result != 0
        {
            timer.compare = result;
            timer.due = Some(cycles + u64::from(result) * timer.cycles_per_tick);
        }
        self.kernel.schedule(&mut self.cpu);
    }

    /// A serving thread is back in its RPC loop: answer the next request
    /// or wait for one.
    fn rpc_loop(&mut self) {
        let Some(t) = self.kernel.current else {
            return;
        };
        let Some((queue_id, request)) = self
            .queues
            .iter_mut()
            .find(|(_, (thread, _))| *thread == t)
            .map(|(&q, (_, requests))| (q, requests.pop_front()))
        else {
            // A loop without a queue: nothing will ever arrive.
            self.kernel.block(&mut self.cpu, Wait::Rpc(0), RPC_LOOP);
            self.kernel.schedule(&mut self.cpu);
            return;
        };
        let Some(request) = request else {
            self.kernel
                .block(&mut self.cpu, Wait::Rpc(queue_id), RPC_LOOP);
            self.kernel.schedule(&mut self.cpu);
            return;
        };
        let Some(server) = self.servers.get(&request.sid).copied() else {
            return;
        };
        self.cpu.gpr[4] = request.function;
        self.cpu.gpr[5] = server.buffer;
        self.cpu.gpr[6] = request.size;
        self.cpu.gpr[31] = RPC_RETURN;
        self.cpu.pc = server.func;
        self.serving.insert(t, request);
    }

    /// A server function returned its reply's address.
    fn rpc_returned(&mut self) {
        if let Some(t) = self.kernel.current
            && let Some(request) = self.serving.remove(&t)
        {
            self.answered.push_back((request, self.cpu.gpr[2]));
        }
        self.cpu.pc = RPC_LOOP;
    }

    /// Runs the IOP until its clock reaches `target`.
    ///
    /// # Errors
    ///
    /// Something the emulation does not support yet (a kernel function, an
    /// exception), with where it happened.
    pub fn run_until(&mut self, regions: &mut Regions, target: u64) -> Result<(), String> {
        loop {
            if self.kernel.fire_alarms(self.cpu.cycles) {
                self.kernel.schedule(&mut self.cpu);
            }
            self.fire_timers();
            self.run_sound();
            while self
                .sound_replies
                .front()
                .is_some_and(|r| r.0 <= self.cpu.cycles)
            {
                if let Some(reply) = self.sound_replies.pop_front() {
                    self.sound_ready.push_back(reply);
                }
            }
            self.finish_transfers(&mut regions.iop_ram);
            if self.interrupted.is_none() && self.kernel.interrupts && !self.callbacks.is_empty() {
                self.start_callback(&mut regions.iop_ram);
            }
            if self.kernel.current.is_none() && self.interrupted.is_none() {
                self.kernel.schedule(&mut self.cpu);
            }
            if self.cpu.cycles >= target {
                return Ok(());
            }
            if self.kernel.current.is_none() && self.interrupted.is_none() {
                // Idle: jump to the next alarm or timer.
                let next = self.next_alarm().unwrap_or(u64::MAX).min(target);
                self.cpu.cycles = next.max(self.cpu.cycles);
                continue;
            }
            match self.cpu.pc {
                CALLBACK_RETURN => self.callback_returned(),
                THREAD_EXIT => {
                    self.kernel.exit_current();
                    self.kernel.schedule(&mut self.cpu);
                }
                MODULE_RETURN => self.module_returned(),
                RPC_LOOP => self.rpc_loop(),
                RPC_RETURN => self.rpc_returned(),
                pc => {
                    if let Some((library, index)) = self.stubs.get(&pc).cloned() {
                        self.kernel_call(regions, &library, index)?;
                    } else {
                        self.step(&mut regions.iop_ram)?;
                    }
                }
            }
        }
    }

    /// A module's start function returned.
    fn module_returned(&mut self) {
        let result = self.cpu.gpr[2];
        if let Some(t) = self.kernel.current
            && let Some((token, id)) = self.loading.remove(&t)
        {
            if let Some(module) = self.modules.get_mut(id as usize - 1) {
                module.result = Some(result);
                tracing::debug!(target: "iop::hle", "module {} started: {result}", module.name);
            }
            self.started.push_back(Started { token, id, result });
        }
        self.kernel.exit_current();
        self.kernel.schedule(&mut self.cpu);
    }

    /// Runs the kernel function `index` of `library`, called from a stub.
    fn kernel_call(
        &mut self,
        regions: &mut Regions,
        library: &str,
        index: u16,
    ) -> Result<(), String> {
        *self.calls.entry((library.to_owned(), index)).or_default() += 1;
        self.cpu.cycles += HLE_COST;
        let ra = self.cpu.gpr[31];
        let Regions { iop_ram, ram, .. } = regions;
        match hle::call(self, iop_ram, ram, library, index) {
            Hle::Return(value) => {
                self.cpu.gpr[2] = value;
                self.cpu.pc = ra;
            }
            Hle::Goto(pc) => self.cpu.pc = pc,
            Hle::Block(wait) => self.kernel.block(&mut self.cpu, wait, ra),
            Hle::Unimplemented => {
                return Err(format!(
                    "IOP kernel function {library} {index} ({}) called from {ra:#010x}",
                    hle::function_name(library, index)
                ));
            }
        }
        self.kernel.schedule(&mut self.cpu);
        Ok(())
    }

    /// Runs one instruction (with its delay slot).
    fn step(&mut self, ram: &mut [u8]) -> Result<(), String> {
        let pc = self.cpu.pc;
        self.hw.now = self.cpu.cycles;
        let mut mem = IopMemory {
            ram,
            scratchpad: &mut self.scratchpad,
            mmio: &mut self.hw,
        };
        let word = mem.read32(pc);
        self.cpu.cycles += 1;
        match execute(&mut self.cpu, &mut mem, word, pc) {
            IopOutcome::Next | IopOutcome::Rfe => self.cpu.pc = pc.wrapping_add(4),
            IopOutcome::Branch { target, taken } => {
                let slot = mem.read32(pc.wrapping_add(4));
                self.cpu.cycles += 1;
                match execute(&mut self.cpu, &mut mem, slot, pc.wrapping_add(4)) {
                    IopOutcome::Next => {}
                    other => {
                        return Err(format!(
                            "IOP: {other:?} in the delay slot at {:#010x}",
                            pc + 4
                        ));
                    }
                }
                self.cpu.pc = if taken { target } else { pc.wrapping_add(8) };
            }
            other => {
                return Err(format!("IOP: {other:?} at {pc:#010x} (word {word:#010x})"));
            }
        }
        Ok(())
    }

    /// Memory access for the kernel functions.
    pub(crate) fn memory<'a>(&'a mut self, ram: &'a mut [u8]) -> IopMemory<'a> {
        IopMemory {
            ram,
            scratchpad: &mut self.scratchpad,
            mmio: &mut self.hw,
        }
    }
}
