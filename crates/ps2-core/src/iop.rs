//! The I/O processor: an R3000A (MIPS I) with 2 MiB of RAM, as Sony's IOP
//! modules see it.
//!
//! - [`IopCpu`]: 32-bit registers, HI/LO, COP0 and the clock.
//! - [`IopMemory`]: the IOP's address space over its RAM (which lives in the
//!   EE's [`Regions`](crate::Regions), as on the console), a 1 KiB
//!   scratchpad, and hardware registers routed to an [`IopMmio`] handler.
//! - [`execute`]: one instruction's effect.
//!
//! Instructions decode with the EE's decoder: MIPS I is a subset of the
//! R5900's encodings. Anything outside MIPS I is [`IopOutcome::Reserved`].
//! `rfe`, which the EE does not have, is recognised by its word.
//!
//! The R3000's load delay (a loaded value is not visible to the next
//! instruction) is not modelled: the modules sly-rs runs never read a
//! register in the instruction after loading it (`docs/research`).

use ps2_isa::ee::{Insn, Op};

/// IOP RAM size: 2 MiB.
pub const RAM_SIZE: usize = 2 << 20;
/// The scratchpad's base address and size (1 KiB).
pub const SCRATCHPAD: u32 = 0x1F80_0000;
/// The `rfe` instruction.
pub const RFE: u32 = 0x4200_0010;

/// COP0 register numbers.
pub mod cop0 {
    /// Status register: interrupt enable (bit 0, `IEc`) and mask (bits 8..15).
    pub const SR: usize = 12;
    /// Cause.
    pub const CAUSE: usize = 13;
    /// Exception PC.
    pub const EPC: usize = 14;
    /// Processor ID.
    pub const PRID: usize = 15;
}

/// The IOP core's registers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IopCpu {
    /// General-purpose registers.
    pub gpr: [u32; 32],
    /// Address of the next instruction.
    pub pc: u32,
    /// HI.
    pub hi: u32,
    /// LO.
    pub lo: u32,
    /// COP0 registers.
    pub cop0: [u32; 32],
    /// IOP cycles run (one instruction per cycle).
    pub cycles: u64,
}

impl Default for IopCpu {
    fn default() -> Self {
        let mut cop0 = [0; 32];
        cop0[cop0::PRID] = 0x1F;
        Self {
            gpr: [0; 32],
            pc: 0,
            hi: 0,
            lo: 0,
            cop0,
            cycles: 0,
        }
    }
}

impl IopCpu {
    /// A register.
    #[inline]
    #[must_use]
    pub const fn r(&self, r: u8) -> u32 {
        self.gpr[r as usize]
    }

    /// Writes a register (`$zero` stays zero).
    #[inline]
    pub fn set(&mut self, r: u8, value: u32) {
        if r != 0 {
            self.gpr[r as usize] = value;
        }
    }
}

/// The IOP's hardware registers. Values are the low `bytes` bytes.
pub trait IopMmio {
    /// A read of `bytes` (1, 2 or 4) at the physical `address`.
    fn read(&mut self, address: u32, bytes: u32) -> u32;
    /// A write of `bytes` at the physical `address`.
    fn write(&mut self, address: u32, bytes: u32, value: u32);
}

/// The IOP's view of memory for the duration of a run.
pub struct IopMemory<'a> {
    /// IOP RAM.
    pub ram: &'a mut [u8],
    /// The 1 KiB scratchpad.
    pub scratchpad: &'a mut [u8],
    /// Hardware registers.
    pub mmio: &'a mut dyn IopMmio,
}

macro_rules! access {
    ($read:ident, $write:ident, $ty:ty) => {
        #[doc = concat!("Reads a `", stringify!($ty), "`.")]
        #[inline]
        pub fn $read(&mut self, address: u32) -> $ty {
            const N: usize = size_of::<$ty>();
            match self.slice(address, N) {
                Some(bytes) => {
                    let mut raw = [0; N];
                    raw.copy_from_slice(bytes);
                    <$ty>::from_le_bytes(raw)
                }
                None => self.mmio.read(address & 0x1FFF_FFFF, N as u32) as $ty,
            }
        }

        #[doc = concat!("Writes a `", stringify!($ty), "`.")]
        #[inline]
        pub fn $write(&mut self, address: u32, value: $ty) {
            const N: usize = size_of::<$ty>();
            match self.slice(address, N) {
                Some(bytes) => bytes.copy_from_slice(&value.to_le_bytes()),
                None => self
                    .mmio
                    .write(address & 0x1FFF_FFFF, N as u32, u32::from(value)),
            }
        }
    };
}

impl IopMemory<'_> {
    /// Plain memory behind `[address, address + len)`: RAM (mirrored four
    /// times below 8 MiB, in every segment) or the scratchpad.
    #[inline]
    fn slice(&mut self, address: u32, len: usize) -> Option<&mut [u8]> {
        let physical = address & 0x1FFF_FFFF;
        let (region, offset): (&mut [u8], usize) = match physical {
            0..0x0080_0000 => (self.ram, (physical as usize) & (RAM_SIZE - 1)),
            0x1F80_0000..0x1F80_0400 => (self.scratchpad, (physical - SCRATCHPAD) as usize),
            _ => return None,
        };
        region.get_mut(offset..offset + len)
    }

    access!(read8, write8, u8);
    access!(read16, write16, u16);
    access!(read32, write32, u32);
}

/// What the IOP does after an instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IopOutcome {
    /// The next instruction.
    Next,
    /// A branch or jump; the delay slot follows, then `target` if `taken`.
    Branch {
        /// Destination.
        target: u32,
        /// Whether the branch is taken.
        taken: bool,
    },
    /// `syscall`.
    Syscall,
    /// `break`.
    Break,
    /// `rfe`: the interrupt-enable bits of Status pop.
    Rfe,
    /// Not a MIPS I instruction.
    Reserved,
}

/// Runs the instruction `word`, at `pc`.
#[allow(clippy::too_many_lines, reason = "one arm per operation")]
pub fn execute(cpu: &mut IopCpu, mem: &mut IopMemory<'_>, word: u32, pc: u32) -> IopOutcome {
    if word == RFE {
        let sr = cpu.cop0[cop0::SR];
        cpu.cop0[cop0::SR] = (sr & !0xF) | ((sr >> 2) & 0xF);
        return IopOutcome::Rfe;
    }
    let insn = Insn::decode(word);
    let (rs, rt, rd, sa) = (insn.rs(), insn.rt(), insn.rd(), insn.sa());
    let (s, t) = (cpu.r(rs), cpu.r(rt));
    let imm = i32::from(insn.simm()).cast_unsigned();
    let uimm = u32::from(insn.imm());
    let address = s.wrapping_add(imm);
    let branch = |taken: bool| IopOutcome::Branch {
        target: insn.branch_target(pc),
        taken,
    };
    match insn.op() {
        Op::Nop => {}
        Op::J => {
            return IopOutcome::Branch {
                target: insn.jump_target(pc),
                taken: true,
            };
        }
        Op::Jal => {
            cpu.set(31, pc.wrapping_add(8));
            return IopOutcome::Branch {
                target: insn.jump_target(pc),
                taken: true,
            };
        }
        Op::Jr => {
            return IopOutcome::Branch {
                target: s,
                taken: true,
            };
        }
        Op::Jalr => {
            cpu.set(rd, pc.wrapping_add(8));
            return IopOutcome::Branch {
                target: s,
                taken: true,
            };
        }
        Op::Beq => return branch(s == t),
        Op::Bne => return branch(s != t),
        Op::Blez => return branch(s.cast_signed() <= 0),
        Op::Bgtz => return branch(s.cast_signed() > 0),
        Op::Bltz => return branch(s.cast_signed() < 0),
        Op::Bgez => return branch(s.cast_signed() >= 0),
        Op::Bltzal | Op::Bgezal => {
            cpu.set(31, pc.wrapping_add(8));
            let taken = if insn.op() == Op::Bltzal {
                s.cast_signed() < 0
            } else {
                s.cast_signed() >= 0
            };
            return branch(taken);
        }
        Op::Syscall => return IopOutcome::Syscall,
        Op::Break => return IopOutcome::Break,

        Op::Addi | Op::Addiu => cpu.set(rt, s.wrapping_add(imm)),
        Op::Slti => cpu.set(rt, u32::from(s.cast_signed() < imm.cast_signed())),
        Op::Sltiu => cpu.set(rt, u32::from(s < imm)),
        Op::Andi => cpu.set(rt, s & uimm),
        Op::Ori => cpu.set(rt, s | uimm),
        Op::Xori => cpu.set(rt, s ^ uimm),
        Op::Lui => cpu.set(rt, uimm << 16),
        Op::Add | Op::Addu => cpu.set(rd, s.wrapping_add(t)),
        Op::Sub | Op::Subu => cpu.set(rd, s.wrapping_sub(t)),
        Op::And => cpu.set(rd, s & t),
        Op::Or => cpu.set(rd, s | t),
        Op::Xor => cpu.set(rd, s ^ t),
        Op::Nor => cpu.set(rd, !(s | t)),
        Op::Slt => cpu.set(rd, u32::from(s.cast_signed() < t.cast_signed())),
        Op::Sltu => cpu.set(rd, u32::from(s < t)),
        Op::Sll => cpu.set(rd, t << sa),
        Op::Srl => cpu.set(rd, t >> sa),
        Op::Sra => cpu.set(rd, (t.cast_signed() >> sa).cast_unsigned()),
        Op::Sllv => cpu.set(rd, t << (s & 31)),
        Op::Srlv => cpu.set(rd, t >> (s & 31)),
        Op::Srav => cpu.set(rd, (t.cast_signed() >> (s & 31)).cast_unsigned()),

        Op::Mult => {
            let p = i64::from(s.cast_signed()) * i64::from(t.cast_signed());
            (cpu.lo, cpu.hi) = (p as u32, (p >> 32) as u32);
        }
        Op::Multu => {
            let p = u64::from(s) * u64::from(t);
            (cpu.lo, cpu.hi) = (p as u32, (p >> 32) as u32);
        }
        // Division by zero and overflow give the R3000's results.
        Op::Div => {
            let (a, b) = (s.cast_signed(), t.cast_signed());
            (cpu.lo, cpu.hi) = if b == 0 {
                (if a >= 0 { u32::MAX } else { 1 }, s)
            } else if a == i32::MIN && b == -1 {
                (s, 0)
            } else {
                ((a / b).cast_unsigned(), (a % b).cast_unsigned())
            };
        }
        Op::Divu => {
            (cpu.lo, cpu.hi) = match s.checked_div(t) {
                Some(q) => (q, s % t),
                None => (u32::MAX, s),
            };
        }
        Op::Mfhi => cpu.set(rd, cpu.hi),
        Op::Mflo => cpu.set(rd, cpu.lo),
        Op::Mthi => cpu.hi = s,
        Op::Mtlo => cpu.lo = s,

        Op::Lb => {
            let v = mem.read8(address);
            cpu.set(rt, i32::from(v.cast_signed()).cast_unsigned());
        }
        Op::Lbu => {
            let v = mem.read8(address);
            cpu.set(rt, u32::from(v));
        }
        Op::Lh => {
            let v = mem.read16(address);
            cpu.set(rt, i32::from(v.cast_signed()).cast_unsigned());
        }
        Op::Lhu => {
            let v = mem.read16(address);
            cpu.set(rt, u32::from(v));
        }
        Op::Lw => {
            let v = mem.read32(address);
            cpu.set(rt, v);
        }
        Op::Lwl | Op::Lwr => {
            let shift = address & 3;
            let word = mem.read32(address & !3);
            let value = if insn.op() == Op::Lwl {
                let keep = 0x00FF_FFFF_u32.checked_shr(8 * shift).unwrap_or(0);
                (t & keep) | (word << (24 - 8 * shift))
            } else {
                let keep = u32::MAX.checked_shl(32 - 8 * shift).unwrap_or(0);
                (t & keep) | (word >> (8 * shift))
            };
            cpu.set(rt, value);
        }
        Op::Sb => mem.write8(address, t as u8),
        Op::Sh => mem.write16(address, t as u16),
        Op::Sw => mem.write32(address, t),
        Op::Swl | Op::Swr => {
            let shift = address & 3;
            let aligned = address & !3;
            let word = mem.read32(aligned);
            let value = if insn.op() == Op::Swl {
                let keep = u32::MAX.checked_shl(8 * (shift + 1)).unwrap_or(0);
                (t >> (24 - 8 * shift)) | (word & keep)
            } else {
                let keep = u32::MAX.checked_shr(32 - 8 * shift).unwrap_or(0);
                (t << (8 * shift)) | (word & keep)
            };
            mem.write32(aligned, value);
        }

        Op::Mfc0 => cpu.set(rt, cpu.cop0[rd as usize]),
        Op::Mtc0 => cpu.cop0[rd as usize] = t,
        _ => return IopOutcome::Reserved,
    }
    IopOutcome::Next
}
