//! The EE machine: the R5900 interpreter, its hardware registers and a
//! high-level emulation of the EE kernel, booting a game's executable
//! without a BIOS.
//!
//! Time is the instruction count (one instruction per EE cycle). Events
//! (V-blank, alarms) happen at exact counts; interrupts are taken between
//! instructions, never in a delay slot. When every thread waits, the clock
//! jumps to the next event.

// The kernel and hardware work on guest register values: 32-bit addresses,
// signed and unsigned views of the same bits, ids that are list indexes.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::semicolon_if_nothing_returned,
    reason = "guest register values"
)]

pub mod dma;
pub mod hw;
pub mod kernel;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use ps2_analysis::Image;
use ps2_core::cpu::cop0;
use ps2_core::rec::{Ctx, Exit, Table};
use ps2_core::{Cpu, Memory, Outcome, execute};
use ps2_iop::{Action, IOP_PACKET_BUFFER, Iop, Transfer};
use ps2_isa::ee::Insn;

use crate::hw::{Hardware, HardwareHandle, intc};
use crate::kernel::{Effect, INTERRUPT_RETURN, Kernel, THREAD_EXIT};

/// EE clock: 294.912 MHz.
pub const EE_HZ: u64 = 294_912_000;
/// Cycles per NTSC field (59.94 Hz).
pub const FIELD_CYCLES: u64 = EE_HZ * 1001 / 60_000;
/// Cycles per H-blank line (262.5 lines per NTSC field).
pub const HSYNC_CYCLES: u64 = FIELD_CYCLES * 2 / 525;
/// Where V-blank starts within a field (line 240 of 262.5).
pub const VBLANK_START: u64 = HSYNC_CYCLES * 240;
/// How often the IOP's processor runs while it has work: every H-line.
pub const IOP_SLICE: u64 = HSYNC_CYCLES;
/// EE cycles per IOP cycle (294.912 MHz / 36.864 MHz).
pub const IOP_DIVIDER: u64 = 8;
/// The top of user memory, the default stack top.
pub const RAM_TOP: u32 = 0x0200_0000;

/// Why [`Machine::run`] stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum Stop {
    /// The cycle budget ran out.
    Budget,
    /// The program exited.
    Exit(i32),
    /// No thread can run and no event will wake one.
    Deadlock,
    /// Something not implemented yet.
    Unimplemented(String),
    /// A `break`, trap or invalid instruction.
    Fault(String),
}

/// A native replacement for a guest function: it reads its arguments from
/// the registers, does the function's work and leaves the result in `$v0`;
/// the machine then returns to `$ra`.
pub type Hook = fn(&mut Cpu, &mut Memory);

/// Where the machine's instructions ran.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Instructions run by generated code.
    pub compiled: u64,
    /// Instructions run by the interpreter, system calls excepted.
    pub interpreted: u64,
    /// System call instructions: the kernel's emulation runs them by
    /// design, whether the code around them is recompiled or not.
    pub kernel: u64,
    /// Calls into generated code.
    pub entries: u64,
    /// Why generated code gave control back, by reason.
    pub exits: BTreeMap<Exit, u64>,
    /// Instructions interpreted, by address (the first 4096 addresses).
    pub interpreted_at: BTreeMap<u32, u64>,
    /// Hooked functions run, by name.
    pub hooks: BTreeMap<&'static str, u64>,
}

/// A booted machine.
pub struct Machine {
    /// The EE core.
    pub cpu: Cpu,
    /// Its memory.
    pub mem: Memory,
    /// Hardware registers (shared with the memory's MMIO handler).
    pub hw: Rc<RefCell<Hardware>>,
    /// The kernel.
    pub kernel: Kernel,
    /// The IOP's side of the SIF.
    pub iop: Iop,
    clock: Rc<Cell<u64>>,
    next_vblank_start: u64,
    next_vblank_end: u64,
    /// When a rebooting IOP reports itself booted.
    iop_booted_at: u64,
    /// Why the IOP's processor stopped, if it did.
    iop_error: Option<String>,
    /// Fields shown so far.
    pub fields: u64,
    /// Calls to subsystems not built yet, by kind (VU0 micro programs, …).
    pub skipped: BTreeMap<String, u64>,
    /// PC samples (one every 4096 instructions), by address, when enabled.
    pub samples: Option<BTreeMap<u32, u64>>,
    /// Watched addresses: each time the PC reaches one, `$a0`–`$a3` and
    /// `$ra` are recorded (the first 16 times).
    pub watches: BTreeMap<u32, Vec<[u32; 5]>>,
    /// Generated code, by the addresses it can start at.
    table: Table,
    /// Native replacements, by function address.
    hooks: BTreeMap<u32, (&'static str, Hook)>,
    /// Where instructions ran.
    pub stats: Stats,
}

impl Machine {
    /// Loads `image` and prepares to start at its entry point, as the BIOS's
    /// `LoadExecPS2` leaves the machine. `argv0` is the program's path.
    ///
    /// # Errors
    ///
    /// The address of a section that does not fit in memory.
    pub fn boot(image: &Image, argv0: &str) -> Result<Self, u32> {
        let clock = Rc::new(Cell::new(0));
        let hw = Rc::new(RefCell::new(Hardware::new(Rc::clone(&clock))));
        // The IOP publishes its packet buffer in SMCOM when its SIF starts.
        hw.borrow_mut().sbus[1] = IOP_PACKET_BUFFER;
        let mut mem = Memory::new(Box::new(HardwareHandle(Rc::clone(&hw))));
        for section in &image.sections {
            if let Some(bytes) = image.bytes(section.range.clone())
                && section.kind != ps2_analysis::SectionKind::Bss
            {
                mem.load(section.range.start, bytes)?;
            }
        }
        // argv[0] for SetupThread.
        let path_at = kernel::ARGS + 16;
        mem.load(path_at, argv0.as_bytes())?;
        mem.write32(kernel::ARGS, path_at);
        let mut cpu = Cpu {
            pc: image.entry,
            ..Cpu::default()
        };
        cpu.cop0[cop0::STATUS] = 0x7000_0C13 | cop0::STATUS_EIE;
        let mut kernel = Kernel::default();
        kernel.boot();
        Ok(Self {
            cpu,
            mem,
            hw,
            kernel,
            iop: {
                let mut iop = Iop::new();
                iop.attach_pad();
                iop
            },
            clock,
            next_vblank_start: VBLANK_START,
            next_vblank_end: FIELD_CYCLES,
            iop_booted_at: u64::MAX,
            iop_error: None,
            fields: 0,
            skipped: BTreeMap::new(),
            samples: None,
            watches: BTreeMap::new(),
            table: Table::empty(),
            hooks: BTreeMap::new(),
            stats: Stats::default(),
        })
    }

    /// Replaces the function at `address` with `hook`. Generated code must
    /// not contain it (calls to it then reach the machine), so a compiled
    /// call can never skip a hook.
    pub fn hook(&mut self, address: u32, name: &'static str, hook: Hook) {
        self.hooks.insert(address, (name, hook));
    }

    /// Runs `table`'s generated code wherever it has some, the interpreter
    /// elsewhere. Execution is the same either way, to the cycle.
    pub fn attach(&mut self, table: Table) {
        self.table = table;
    }

    /// Runs generated code from the current PC, if there is some and the
    /// machine has nothing to do before `deadline`. True if it ran at least
    /// one instruction.
    fn run_compiled(&mut self, deadline: u64) -> bool {
        let Some(f) = self.table.get(self.cpu.pc) else {
            return false;
        };
        if !self.watches.is_empty() || self.samples.is_some() {
            return false;
        }
        let before = self.cpu.cycles;
        let mut ctx = Ctx {
            cpu: &mut self.cpu,
            mem: &mut self.mem,
            deadline,
            exiting: false,
            exit: Exit::Return,
            depth: 0,
            table: &self.table,
        };
        f(&mut ctx);
        let exit = if ctx.exiting { ctx.exit } else { Exit::Return };
        let ran = self.cpu.cycles - before;
        self.clock.set(self.cpu.cycles);
        self.stats.entries += 1;
        self.stats.compiled += ran;
        *self.stats.exits.entry(exit).or_default() += 1;
        ran > 0
    }

    /// When the IOP's processor next runs: the next slice boundary while it
    /// has a thread to run, the slice its next alarm falls in otherwise.
    fn next_iop_slice(&self) -> u64 {
        let slice_after = |cycle: u64| (cycle / IOP_SLICE + 1) * IOP_SLICE;
        if self.iop.core.kernel.runnable() {
            slice_after(self.cpu.cycles)
        } else {
            self.iop.core.next_alarm().map_or(u64::MAX, |alarm| {
                slice_after((alarm * IOP_DIVIDER).max(self.cpu.cycles))
            })
        }
    }

    /// The next cycle at which an event is due.
    fn next_event(&self) -> u64 {
        let alarm = self.kernel.next_alarm().unwrap_or(u64::MAX);
        self.next_vblank_start
            .min(self.next_vblank_end)
            .min(alarm)
            .min(self.iop_booted_at)
            .min(self.next_iop_slice())
            .min(self.hw.borrow().next_dma().unwrap_or(u64::MAX))
    }

    /// Raises the events due at the current cycle. Field k runs from
    /// k × [`FIELD_CYCLES`]; V-blank covers its last 22.5 lines.
    fn events(&mut self) {
        let now = self.cpu.cycles;
        // The IOP catches up whenever it has work due: at its slice
        // boundaries, and at any other event while it is busy.
        if self.iop.busy(now / IOP_DIVIDER)
            && let Err(error) = self.iop.run(&mut self.mem, now / IOP_DIVIDER)
        {
            self.iop_error = Some(error);
        }
        if self.hw.borrow().next_dma().is_some_and(|due| due <= now) {
            self.hw.borrow_mut().finish_dma(now);
            self.run_vif0();
        }
        if now >= self.next_vblank_start {
            // V-blank start: interrupt, GS CSR (V-sync bit, field flip), the
            // kernel's V-sync flag.
            let csr = {
                let mut hw = self.hw.borrow_mut();
                hw.raise_intc(intc::VBLANK_START);
                hw.gs_csr ^= 1 << 13;
                hw.gs_event(3);
                if hw.gs.dumping() {
                    let field = ((hw.gs_csr >> 13) & 1) as u8;
                    let block = hw.gs_privileged();
                    hw.gs.dump_vsync(field, &block);
                }
                hw.gs_csr
            };
            self.iop.vblank(&mut self.mem);
            if let Some((flag, csr_at)) = self.kernel.vsync_flag() {
                self.mem.write32(flag, 1);
                self.mem.write64(csr_at, csr);
            }
            self.fields += 1;
            self.next_vblank_start += FIELD_CYCLES;
        }
        if now >= self.iop_booted_at {
            let mut hw = self.hw.borrow_mut();
            hw.sbus[1] = IOP_PACKET_BUFFER;
            hw.sbus[3] |= hw::sif::SIFINIT | hw::sif::CMDINIT | hw::sif::BOOTEND;
            self.iop_booted_at = u64::MAX;
        }
        if now >= self.next_vblank_end {
            self.hw.borrow_mut().raise_intc(intc::VBLANK_END);
            self.next_vblank_end += FIELD_CYCLES;
        }
    }

    /// Whether an interrupt can be taken now.
    fn interrupt_ready(&self) -> bool {
        if self.kernel.in_interrupt() || self.cpu.cop0[cop0::STATUS] & cop0::STATUS_EIE == 0 {
            return false;
        }
        let hw = self.hw.borrow();
        hw.intc_pending() != 0
            || hw.dmac_pending() != 0
            || self
                .kernel
                .next_alarm()
                .is_some_and(|due| due <= self.cpu.cycles)
    }

    /// A hash of the machine's state: registers, the clock, RAM and the
    /// scratchpad, and the IOP's registers and RAM. Two runs from the same start must give the same hash.
    #[must_use]
    pub fn state_hash(&self) -> u64 {
        let mut hash: u64 = 0xCBF2_9CE4_8422_2325;
        let mut feed = |bytes: &[u8]| {
            for chunk in bytes.chunks(8) {
                let mut word = [0u8; 8];
                word[..chunk.len()].copy_from_slice(chunk);
                hash = (hash ^ u64::from_le_bytes(word)).wrapping_mul(0x0000_0100_0000_01B3);
            }
        };
        let cpu = &self.cpu;
        for reg in cpu.gpr {
            feed(&reg.to_le_bytes());
        }
        feed(&cpu.pc.to_le_bytes());
        feed(&cpu.cycles.to_le_bytes());
        for reg in cpu.fpr {
            feed(&reg.to_le_bytes());
        }
        feed(&self.mem.regions.ram);
        feed(&self.mem.regions.scratchpad);
        feed(&self.mem.regions.iop_ram);
        let iop = &self.iop.core.cpu;
        for reg in iop.gpr {
            feed(&reg.to_le_bytes());
        }
        feed(&iop.pc.to_le_bytes());
        feed(&iop.cycles.to_le_bytes());
        hash
    }

    /// Runs VIF0's completed transfers (and FIFO writes) into VU0, whose
    /// registers are the EE's COP2 registers.
    fn run_vif0(&mut self) {
        let mut hw = self.hw.borrow_mut();
        let hw = &mut *hw;
        if hw.vif0_ready.is_empty() {
            return;
        }
        for words in std::mem::take(&mut hw.vif0_ready) {
            hw.vif0.push(&words);
        }
        let regions = &mut self.mem.regions;
        let mut mem = ps2_vu::Memories {
            code: &mut regions.vu0_code,
            data: &mut regions.vu0_data,
        };
        if hw.vif0.run(&mut self.cpu.vu0, &mut mem, &mut hw.gs) == ps2_vu::vif::Stop::Interrupt {
            hw.raise_intc(intc::VIF0);
        }
    }

    /// Runs until `budget` more cycles have passed or the machine stops.
    pub fn run(&mut self, budget: u64) -> Stop {
        let end = self.cpu.cycles.saturating_add(budget);
        let mut next_event = self.next_event();
        while self.cpu.cycles < end {
            if self.cpu.cycles >= next_event {
                self.events();
                if let Some(error) = self.iop_error.take() {
                    return Stop::Unimplemented(error);
                }
                next_event = self.next_event();
            }
            if self.iop.pending() && !self.kernel.in_interrupt() && self.iop.deliver(&mut self.mem)
            {
                self.hw.borrow_mut().complete_dma(5);
            }
            if self.interrupt_ready() {
                let mut hw = self.hw.borrow_mut();
                self.kernel.begin_interrupt(&mut self.cpu, &mut hw);
            }
            if self.kernel.current.is_none() && !self.kernel.in_interrupt() {
                // Every thread waits: jump to the next event, but not past
                // the end of the run (several short runs then equal one long
                // one).
                if next_event == u64::MAX {
                    return Stop::Deadlock;
                }
                self.cpu.cycles = next_event.min(end).max(self.cpu.cycles);
                self.clock.set(self.cpu.cycles);
                continue;
            }
            match self.cpu.pc {
                INTERRUPT_RETURN => {
                    self.kernel.next_handler(&mut self.cpu);
                    continue;
                }
                THREAD_EXIT => {
                    self.kernel.thread_returned(&mut self.cpu);
                    continue;
                }
                _ => {}
            }
            if let Some(&(name, hook)) = self.hooks.get(&self.cpu.pc) {
                // A hooked function: its work in one cycle, then back to $ra.
                hook(&mut self.cpu, &mut self.mem);
                self.cpu.pc = self.cpu.r32(31);
                self.cpu.cycles += 1;
                self.clock.set(self.cpu.cycles);
                *self.stats.hooks.entry(name).or_default() += 1;
                continue;
            }
            if self.run_compiled(next_event.min(end)) {
                continue;
            }
            let before = self.cpu.cycles;
            let syscall =
                Insn::decode(self.mem.read32(self.cpu.pc)).op() == ps2_isa::ee::Op::Syscall;
            if let Some(stop) = self.step() {
                return stop;
            }
            let ran = self.cpu.cycles - before;
            if syscall {
                self.stats.kernel += ran;
            } else {
                self.stats.interpreted += ran;
            }
            if !self.table.is_empty() && self.stats.interpreted_at.len() < 4096 {
                *self.stats.interpreted_at.entry(self.cpu.pc).or_default() += ran;
            }
            next_event = next_event.min(self.next_event());
        }
        Stop::Budget
    }

    /// Runs a VU program the EE started (`vcallms` for VU0, CMSAR1 for
    /// VU1) to its end.
    fn run_vu(&mut self, vu1: bool, address: u32) {
        let regions = &mut self.mem.regions;
        if !vu1 {
            ps2_vu::micro::run(
                &mut ps2_vu::micro::Unit {
                    regs: &mut self.cpu.vu0,
                    code: &regions.vu0_code,
                    data: &mut regions.vu0_data,
                    vu1: None,
                },
                address,
            );
            return;
        }
        let mut hw = self.hw.borrow_mut();
        let hw = &mut *hw;
        ps2_vu::micro::run(
            &mut ps2_vu::micro::Unit {
                regs: &mut hw.vu1,
                code: &regions.vu1_code,
                data: &mut regions.vu1_data,
                vu1: Some(ps2_vu::micro::Vu1Link {
                    itop: hw.vif1.read(0xD0),
                    top: hw.vif1.read(0xE0),
                    gs: &mut hw.gs,
                }),
            },
            address,
        );
    }

    /// Runs one instruction (with its delay slot).
    fn step(&mut self) -> Option<Stop> {
        let pc = self.cpu.pc;
        if !self.watches.is_empty()
            && let Some(hits) = self.watches.get_mut(&pc)
            && hits.len() < 16
        {
            let c = &self.cpu;
            hits.push([c.r32(4), c.r32(5), c.r32(6), c.r32(7), c.r32(31)]);
        }
        if self.cpu.cycles.is_multiple_of(4096)
            && let Some(samples) = self.samples.as_mut()
        {
            *samples.entry(pc).or_default() += 1;
        }
        let insn = Insn::decode(self.mem.read32(pc));
        self.cpu.cycles += 1;
        self.clock.set(self.cpu.cycles);
        match execute(&mut self.cpu, &mut self.mem, insn, pc) {
            Outcome::Next | Outcome::InterruptsEnabled => self.cpu.pc = pc.wrapping_add(4),
            Outcome::Branch {
                target,
                taken,
                likely,
            } => {
                if likely && !taken {
                    self.cpu.pc = pc.wrapping_add(8);
                } else {
                    let slot = pc.wrapping_add(4);
                    let delay = Insn::decode(self.mem.read32(slot));
                    // The clock counts the instruction running, the delay
                    // slot included, for hardware and `mfc0 Count` alike.
                    self.cpu.cycles += 1;
                    self.clock.set(self.cpu.cycles);
                    match execute(&mut self.cpu, &mut self.mem, delay, slot) {
                        Outcome::Next | Outcome::InterruptsEnabled => {}
                        other => {
                            return Some(Stop::Fault(format!(
                                "{other:?} in the delay slot at {slot:#010x}"
                            )));
                        }
                    }
                    self.cpu.pc = if taken { target } else { pc.wrapping_add(8) };
                }
            }
            Outcome::Syscall => {
                self.cpu.pc = pc.wrapping_add(4);
                let mut hw = self.hw.borrow_mut();
                let effect = self.kernel.syscall(&mut self.cpu, &mut self.mem, &mut hw);
                drop(hw);
                while let Some(t) = self.kernel.sif_dma.pop_front() {
                    let transfer = Transfer {
                        src: t.src,
                        dest: t.dest,
                        size: t.size,
                    };
                    if self.iop.receive(&transfer, &mut self.mem) == Some(Action::Reboot) {
                        // A reboot takes a while (modules load); a quarter field.
                        self.iop_booted_at = self.cpu.cycles + FIELD_CYCLES / 4;
                    }
                }
                match effect {
                    Effect::Continue => {}
                    Effect::Exit(code) => return Some(Stop::Exit(code)),
                    Effect::Unimplemented(what) => return Some(Stop::Unimplemented(what)),
                }
            }
            Outcome::VuCall(address) => {
                self.run_vu(false, address);
                self.cpu.pc = pc.wrapping_add(4);
            }
            Outcome::Vu1Call(address) => {
                self.run_vu(true, address);
                self.cpu.pc = pc.wrapping_add(4);
            }
            Outcome::Break => return Some(Stop::Fault(format!("break at {pc:#010x}"))),
            Outcome::Trap => return Some(Stop::Fault(format!("trap at {pc:#010x}"))),
            Outcome::Eret => return Some(Stop::Fault(format!("eret at {pc:#010x}"))),
            Outcome::Reserved => {
                return Some(Stop::Fault(format!(
                    "invalid instruction {:#010x} at {pc:#010x}",
                    insn.word()
                )));
            }
        }
        None
    }
}
