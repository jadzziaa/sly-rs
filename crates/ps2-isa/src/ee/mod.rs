//! The Emotion Engine's R5900 core: MIPS III with 128-bit MMI integer SIMD,
//! a single-precision FPU (COP1) and VU0 in macro mode (COP2).
//!
//! [`Insn::decode`] identifies the operation by walking the opcode tables
//! (primary, `SPECIAL`, `REGIMM`, MMI and its four sub-tables, COP0, COP1,
//! COP2 and its special table). An instruction is valid when its operation
//! is known and every bit that is neither part of the opcode nor an operand
//! is zero ([`Insn::is_valid`]).

mod decode;
mod format;
mod ops;

pub use format::{Display, Format};
pub use ops::{Flow, Op};

/// A decoded R5900 instruction: its operation and the word, from which the
/// operand fields are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Insn {
    op: Op,
    word: u32,
}

impl Insn {
    /// Decodes an instruction word.
    #[must_use]
    pub const fn decode(word: u32) -> Self {
        Self {
            op: decode::op(word),
            word,
        }
    }

    /// The operation.
    #[must_use]
    pub const fn op(self) -> Op {
        self.op
    }

    /// The instruction word.
    #[must_use]
    pub const fn word(self) -> u32 {
        self.word
    }

    /// Whether the word is a known instruction with every reserved bit zero.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        if matches!(self.op, Op::Invalid) {
            return false;
        }
        let used = self.op.class().fixed_mask() | self.op.format().operand_mask();
        self.word & !used == 0
    }

    /// Bits 25..21: `rs` (also `base` and the coprocessor `fmt`).
    #[must_use]
    pub const fn rs(self) -> u8 {
        field(self.word, 21, 5)
    }

    /// Bits 20..16: `rt` (also `ft` and the VU `ft`/`it`).
    #[must_use]
    pub const fn rt(self) -> u8 {
        field(self.word, 16, 5)
    }

    /// Bits 15..11: `rd` (also `fs` and the VU `fs`/`is`).
    #[must_use]
    pub const fn rd(self) -> u8 {
        field(self.word, 11, 5)
    }

    /// Bits 10..6: `sa` (also `fd` and the VU `fd`/`id`).
    #[must_use]
    pub const fn sa(self) -> u8 {
        field(self.word, 6, 5)
    }

    /// Bits 5..0: the function field.
    #[must_use]
    pub const fn funct(self) -> u8 {
        field(self.word, 0, 6)
    }

    /// The 16-bit immediate, unsigned.
    #[must_use]
    pub const fn imm(self) -> u16 {
        self.word as u16
    }

    /// The 16-bit immediate, sign-extended.
    #[must_use]
    pub const fn simm(self) -> i16 {
        self.word as i16
    }

    /// The 26-bit jump target index.
    #[must_use]
    pub const fn target(self) -> u32 {
        self.word & 0x03FF_FFFF
    }

    /// Bits 25..6: the `syscall`/`break` code.
    #[must_use]
    pub const fn code20(self) -> u32 {
        (self.word >> 6) & 0xF_FFFF
    }

    /// Bits 24..21: the VU destination mask (bit 3 = x … bit 0 = w).
    #[must_use]
    pub const fn dest(self) -> u8 {
        field(self.word, 21, 4)
    }

    /// The broadcast component of a VU `...x/y/z/w` operation (bits 1..0).
    #[must_use]
    pub const fn bc(self) -> char {
        component(field(self.word, 0, 2))
    }

    /// The VU `fsf` component (bits 22..21).
    #[must_use]
    pub const fn fsf(self) -> char {
        component(field(self.word, 21, 2))
    }

    /// The VU `ftf` component (bits 24..23).
    #[must_use]
    pub const fn ftf(self) -> char {
        component(field(self.word, 23, 2))
    }

    /// The signed 5-bit immediate of `viaddi` (bits 10..6).
    #[must_use]
    pub const fn imm5(self) -> i8 {
        ((field(self.word, 6, 5) << 3) as i8) >> 3
    }

    /// The 15-bit immediate of `vcallms` (bits 20..6): a VU0 micro address
    /// in 8-byte units.
    #[must_use]
    pub const fn imm15(self) -> u32 {
        (self.word >> 6) & 0x7FFF
    }

    /// The target of a branch at `pc`: the delay slot plus the offset.
    #[must_use]
    pub const fn branch_target(self, pc: u32) -> u32 {
        pc.wrapping_add(4)
            .wrapping_add(((self.simm() as i32) << 2) as u32)
    }

    /// The target of a `j`/`jal` at `pc`: within the delay slot's 256 MiB
    /// region.
    #[must_use]
    pub const fn jump_target(self, pc: u32) -> u32 {
        (pc.wrapping_add(4) & 0xF000_0000) | (self.target() << 2)
    }

    /// What the instruction does to control flow.
    #[must_use]
    pub const fn flow(self) -> Flow {
        self.op.flow()
    }

    /// The instruction written with targets resolved against its address
    /// `pc`.
    #[must_use]
    pub const fn display(self, pc: u32) -> Display {
        Display {
            insn: self,
            pc: Some(pc),
        }
    }
}

/// Without an address, branch targets are written relative to the
/// instruction (`. + 4 + 0x10`) and jump targets within region 0.
impl core::fmt::Display for Insn {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(
            &Display {
                insn: *self,
                pc: None,
            },
            f,
        )
    }
}

const fn field(word: u32, shift: u32, bits: u32) -> u8 {
    ((word >> shift) & ((1 << bits) - 1)) as u8
}

const fn component(index: u8) -> char {
    crate::regs::VU_COMPONENTS[(index & 3) as usize]
}

#[cfg(test)]
mod tests;
