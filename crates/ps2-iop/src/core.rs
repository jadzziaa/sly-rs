//! The IOP processor running modules: the interpreter over
//! `ps2_core::iop`, module loading and linking, and the calls modules make
//! into the high-level emulated kernel ([`crate::hle`]).
//!
//! Imports from kernel libraries are intercepted at their stubs: when the
//! PC reaches one, the library function runs in Rust and returns to `$ra`.
//! Imports from libraries other modules export (`989snd` uses `libsd`) are
//! linked as the real loader does: the stub becomes a jump to the function.

use std::collections::{BTreeMap, VecDeque};

use ps2_core::iop::{IopCpu, IopMemory, IopMmio, IopOutcome, execute};

use crate::hle::{self, Hle};
use crate::irx::{Irx, IrxError};
use crate::kernel::{self, Kernel, MODULE_RETURN, THREAD_EXIT, Thread};

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

/// Hardware registers not modelled yet: reads return the last value
/// written, and the first access to each register is logged.
#[derive(Default)]
pub struct Registers {
    values: BTreeMap<u32, u32>,
}

impl IopMmio for Registers {
    fn read(&mut self, address: u32, bytes: u32) -> u32 {
        let value = self.values.get(&address).copied().unwrap_or(0);
        tracing::trace!(target: "iop::hw", "read{} {address:#010x} = {value:#x}", bytes * 8);
        value
    }

    fn write(&mut self, address: u32, bytes: u32, value: u32) {
        tracing::trace!(target: "iop::hw", "write{} {address:#010x} = {value:#x}", bytes * 8);
        self.values.insert(address, value);
    }
}

/// The IOP processor and its kernel.
pub struct IopCore {
    /// The CPU.
    pub cpu: IopCpu,
    scratchpad: Vec<u8>,
    /// Hardware registers.
    pub hw: Registers,
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
}

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
            hw: Registers::default(),
            kernel: Kernel::default(),
            stubs: BTreeMap::new(),
            libraries: BTreeMap::new(),
            modules: Vec::new(),
            loading: BTreeMap::new(),
            started: VecDeque::new(),
            console: Vec::new(),
            calls: BTreeMap::new(),
        }
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
            wait: kernel::Wait::None,
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
        self.kernel.runnable() || self.kernel.next_alarm().is_some_and(|a| a <= until)
    }

    /// When the IOP next has something to do, if it is idle.
    #[must_use]
    pub fn next_alarm(&self) -> Option<u64> {
        self.kernel.next_alarm()
    }

    /// Runs the IOP until its clock reaches `target`.
    ///
    /// # Errors
    ///
    /// Something the emulation does not support yet (a kernel function, an
    /// exception), with where it happened.
    pub fn run_until(&mut self, ram: &mut [u8], target: u64) -> Result<(), String> {
        loop {
            if self.kernel.fire_alarms(self.cpu.cycles) {
                self.kernel.schedule(&mut self.cpu);
            }
            if self.kernel.current.is_none() {
                self.kernel.schedule(&mut self.cpu);
            }
            if self.cpu.cycles >= target {
                return Ok(());
            }
            if self.kernel.current.is_none() {
                // Idle: jump to the next alarm.
                let next = self.kernel.next_alarm().unwrap_or(u64::MAX).min(target);
                self.cpu.cycles = next.max(self.cpu.cycles);
                continue;
            }
            match self.cpu.pc {
                THREAD_EXIT => {
                    self.kernel.exit_current();
                    self.kernel.schedule(&mut self.cpu);
                }
                MODULE_RETURN => self.module_returned(),
                pc => {
                    if let Some((library, index)) = self.stubs.get(&pc).cloned() {
                        self.kernel_call(ram, &library, index)?;
                    } else {
                        self.step(ram)?;
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
    fn kernel_call(&mut self, ram: &mut [u8], library: &str, index: u16) -> Result<(), String> {
        *self.calls.entry((library.to_owned(), index)).or_default() += 1;
        self.cpu.cycles += HLE_COST;
        let ra = self.cpu.gpr[31];
        match hle::call(self, ram, library, index) {
            Hle::Return(value) => {
                self.cpu.gpr[2] = value;
                self.cpu.pc = ra;
            }
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
