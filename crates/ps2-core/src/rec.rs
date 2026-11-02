//! What recompiled code runs on: [`Ctx`], the state a generated function
//! receives, and [`Table`], the generated functions by address.
//!
//! A generated function is a `fn(&mut Ctx)` that starts at the block
//! `cpu.pc` names (any block of the function) and runs until the guest
//! returns from it, or until it must give control back to the machine. It
//! keeps no guest state of its own: registers live in [`Cpu`] and the guest's
//! stack in guest memory. So control can go back to the machine at any block
//! boundary, and the machine can resume at any block start, without
//! unwinding or switching host stacks.
//!
//! Giving control back is an *exit*: `cpu.pc` and `cpu.cycles` are set
//! to the boundary where the interpreter would stand, [`Ctx::exiting`] is
//! set, and every generated frame returns. After a call, a frame continues
//! only if the callee returned normally to the frame's return address.
//!
//! Compiled code runs blocks only while the interpreter would do nothing
//! between their instructions: a block starts only if it ends by
//! [`Ctx::deadline`] (the next event), and the code exits at the boundary
//! after anything that may let an interrupt in (a hardware register write
//! after which one is pending, `ei`, a write to COP0 Status) and before
//! anything the machine must run itself (system calls, VU calls, faults).
//! Time then advances exactly as under the interpreter: one cycle per
//! instruction, events at the same counts.

use ps2_isa::ee::Insn;

use crate::{Cpu, Memory, Outcome, execute};

/// A generated function.
pub type GuestFn = fn(&mut Ctx<'_>);

/// How deep generated calls nest on the host stack before the machine takes
/// over (and starts again from an empty stack).
pub const MAX_DEPTH: u32 = 512;

/// Why compiled code gave control back to the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Exit {
    /// The guest returned from the function the machine called.
    Return,
    /// The next block would run past the deadline.
    Budget,
    /// An instruction the machine runs itself: system call, VU call,
    /// `eret`, a trap that fires, an invalid instruction.
    Interpret,
    /// A write to a hardware register.
    Mmio,
    /// `ei` or a write to COP0 Status.
    Interrupts,
    /// A jump or call to an address with no generated code.
    NotCompiled,
    /// Calls nested [`MAX_DEPTH`] deep.
    Depth,
}

impl Exit {
    /// Every reason, in a fixed order (for statistics).
    pub const ALL: [Self; 7] = [
        Self::Return,
        Self::Budget,
        Self::Interpret,
        Self::Mmio,
        Self::Interrupts,
        Self::NotCompiled,
        Self::Depth,
    ];
}

/// Generated functions by the addresses they can start at (every block
/// start), dense over the code they cover.
pub struct Table {
    base: u32,
    entries: Vec<Option<GuestFn>>,
}

impl Table {
    /// A table of `(block start, function)` pairs.
    #[must_use]
    pub fn new(blocks: &[(u32, GuestFn)]) -> Self {
        let base = blocks.iter().map(|b| b.0).min().unwrap_or(0) & !3;
        let top = blocks.iter().map(|b| b.0).max().unwrap_or(0);
        let mut entries = vec![None; ((top.saturating_sub(base)) / 4 + 1) as usize];
        for &(address, f) in blocks {
            entries[((address - base) / 4) as usize] = Some(f);
        }
        Self { base, entries }
    }

    /// An empty table: nothing is compiled.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            base: 0,
            entries: Vec::new(),
        }
    }

    /// The function that can start at `address`.
    #[inline]
    #[must_use]
    pub fn get(&self, address: u32) -> Option<GuestFn> {
        if address & 3 != 0 {
            return None;
        }
        let index = address.wrapping_sub(self.base) / 4;
        self.entries.get(index as usize).copied().flatten()
    }

    /// How many addresses have code.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.iter().filter(|e| e.is_some()).count()
    }

    /// Whether nothing is compiled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(Option::is_none)
    }
}

/// The state generated code runs on.
pub struct Ctx<'a> {
    /// The EE core.
    pub cpu: &'a mut Cpu,
    /// Its memory.
    pub mem: &'a mut Memory,
    /// No block may end after this cycle.
    pub deadline: u64,
    /// Set by every exit; frames return while it is set.
    pub exiting: bool,
    /// Why control went back to the machine.
    pub exit: Exit,
    /// Generated calls on the host stack.
    pub depth: u32,
    /// Generated functions, for calls through registers and across crates.
    pub table: &'a Table,
}

macro_rules! access {
    ($load:ident, $store:ident, $ty:ty, $read:ident, $write:ident) => {
        #[doc = concat!("Loads a `", stringify!($ty), "`; `rem` instructions of the block follow this one.")]
        #[inline]
        pub fn $load(&mut self, address: u32, rem: u64) -> $ty {
            const N: usize = size_of::<$ty>();
            if let Some(bytes) = self.direct(address, N) {
                let mut raw = [0; N];
                raw.copy_from_slice(bytes);
                return <$ty>::from_le_bytes(raw);
            }
            self.sync(rem);
            self.mem.$read(address)
        }

        #[doc = concat!("Stores a `", stringify!($ty), "`; true if it wrote a hardware register after which an interrupt may be taken (the code must then exit).")]
        #[inline]
        #[must_use]
        pub fn $store(&mut self, address: u32, value: $ty, rem: u64) -> bool {
            const N: usize = size_of::<$ty>();
            if let Some(bytes) = self.direct(address, N) {
                bytes.copy_from_slice(&value.to_le_bytes());
                return false;
            }
            self.sync(rem);
            self.mem.$write(address, value);
            std::mem::take(&mut self.mem.attention)
        }
    };
}

/// Addresses below this are main RAM itself, not a mirror.
const RAM_DIRECT: u32 = crate::mem::RAM_SIZE as u32;

impl Ctx<'_> {
    /// Main RAM or the scratchpad, the memory most accesses reach; anything
    /// else (mirrors, VU memory, hardware) takes the machine's own path.
    #[inline]
    fn direct(&mut self, address: u32, n: usize) -> Option<&mut [u8]> {
        let a = address as usize;
        if address < RAM_DIRECT {
            self.mem.regions.ram.get_mut(a..a + n)
        } else if address & !0x3FFF == crate::mem::SCRATCHPAD {
            let a = a & 0x3FFF;
            self.mem.regions.scratchpad.get_mut(a..a + n)
        } else {
            None
        }
    }

    /// Starts a block of `n` instructions at `pc`: false (after exiting) if
    /// it would end past the deadline.
    #[inline]
    #[must_use]
    pub fn enter(&mut self, n: u64, pc: u32) -> bool {
        if self.cpu.cycles + n > self.deadline {
            self.cpu.pc = pc;
            self.exiting = true;
            self.exit = Exit::Budget;
            return false;
        }
        self.cpu.cycles += n;
        true
    }

    /// Exits before the instruction at `pc`, `rem` instructions (this one
    /// included) before the end of its block: the machine runs it.
    #[cold]
    pub fn exit_before(&mut self, pc: u32, rem: u64) {
        self.cpu.cycles -= rem;
        self.cpu.pc = pc;
        self.exiting = true;
        self.exit = Exit::Interpret;
    }

    /// Exits at the boundary before `pc`, `rem` instructions before the end
    /// of the block, because of `why`.
    #[cold]
    pub fn exit_at(&mut self, pc: u32, rem: u64, why: Exit) {
        self.cpu.cycles -= rem;
        self.cpu.pc = pc;
        self.exiting = true;
        self.exit = why;
    }

    /// Sets the hardware's clock to the instruction running, `rem`
    /// instructions before the end of its block.
    #[cold]
    fn sync(&mut self, rem: u64) {
        self.mem.mmio.set_clock(self.cpu.cycles - rem);
    }

    access!(lb, sb, u8, read8, write8);
    access!(lh, sh, u16, read16, write16);
    access!(lw, sw, u32, read32, write32);
    access!(ld, sd, u64, read64, write64);
    access!(lq, sq, u128, read128, write128);

    /// Runs one instruction through the shared semantics; true (without
    /// effect) if the machine must run it instead.
    #[inline(never)]
    #[must_use]
    pub fn exec(&mut self, word: u32, pc: u32) -> bool {
        execute(self.cpu, self.mem, Insn::decode(word), pc) != Outcome::Next
    }

    /// [`Ctx::exec`] for an instruction that touches memory, `rem`
    /// instructions before the end of its block: true in the first value if
    /// the machine must run it, in the second if it wrote a hardware
    /// register after which an interrupt may be taken.
    #[inline(never)]
    #[must_use]
    pub fn exec_mem(&mut self, word: u32, pc: u32, rem: u64) -> (bool, bool) {
        self.sync(rem);
        let interpret = self.exec(word, pc);
        (interpret, std::mem::take(&mut self.mem.attention))
    }

    /// Calls the generated function `f` (with `cpu.pc` set to where it
    /// starts).
    #[inline]
    pub fn call_fn(&mut self, f: GuestFn) {
        if self.depth >= MAX_DEPTH {
            self.exiting = true;
            self.exit = Exit::Depth;
            return;
        }
        self.depth += 1;
        f(self);
        self.depth -= 1;
    }

    /// Calls or jumps to `target` through the table: the generated function
    /// there, or an exit to let the machine run it.
    #[inline(never)]
    pub fn call(&mut self, target: u32) {
        self.cpu.pc = target;
        if let Some(f) = self.table.get(target) {
            self.call_fn(f);
        } else {
            self.leave(target);
        }
    }

    /// Leaves for `target`, which this function has no block for: exits so
    /// the machine finds it.
    #[cold]
    pub fn leave(&mut self, target: u32) {
        self.cpu.pc = target;
        self.exiting = true;
        self.exit = Exit::NotCompiled;
    }
}
