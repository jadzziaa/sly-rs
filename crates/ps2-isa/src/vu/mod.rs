//! VU micro mode: the 64-bit instruction pairs VU0 and VU1 execute from
//! micro memory. The upper word (bits 63..32) drives the FMAC pipeline and
//! carries the I, E, M, D and T flags; the lower word (bits 31..0) is an
//! integer, load/store, branch, divide, random-number or EFU instruction, or
//! with the I flag a float immediate for the I register (`loi`).
//!
//! Opcode tables as in PCSX2's VU disassembler (`DisVUmicro.h`), which the
//! tests check against; mnemonics are written in lower case.

mod lower;
mod upper;

use core::fmt;

pub use lower::LowerOp;
pub use upper::UpperOp;

/// The I flag: the lower word is an immediate for the I register.
pub const FLAG_I: u32 = 1 << 31;
/// The E flag: the program ends after the next pair.
pub const FLAG_E: u32 = 1 << 30;
/// The M flag: VU0 may be interrupted by `vcallms` here.
pub const FLAG_M: u32 = 1 << 29;
/// The D flag: debug break.
pub const FLAG_D: u32 = 1 << 28;
/// The T flag: debug halt.
pub const FLAG_T: u32 = 1 << 27;

/// A decoded instruction pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Pair {
    upper_op: UpperOp,
    lower_op: LowerOp,
    upper: u32,
    lower: u32,
}

impl Pair {
    /// Decodes a pair from its 64-bit little-endian value (upper word in the
    /// high half).
    #[must_use]
    pub const fn decode(pair: u64) -> Self {
        Self::from_words((pair >> 32) as u32, pair as u32)
    }

    /// Decodes a pair from its upper and lower words.
    #[must_use]
    pub const fn from_words(upper: u32, lower: u32) -> Self {
        let lower_op = if upper & FLAG_I != 0 {
            LowerOp::Loi
        } else {
            lower::decode(lower)
        };
        Self {
            upper_op: upper::decode(upper),
            lower_op,
            upper,
            lower,
        }
    }

    /// The upper operation.
    #[must_use]
    pub const fn upper_op(self) -> UpperOp {
        self.upper_op
    }

    /// The lower operation ([`LowerOp::Loi`] when the I flag is set).
    #[must_use]
    pub const fn lower_op(self) -> LowerOp {
        self.lower_op
    }

    /// The upper word.
    #[must_use]
    pub const fn upper(self) -> u32 {
        self.upper
    }

    /// The lower word.
    #[must_use]
    pub const fn lower(self) -> u32 {
        self.lower
    }

    /// The flag bits of the upper word ([`FLAG_I`] … [`FLAG_T`]).
    #[must_use]
    pub const fn flags(self) -> u32 {
        self.upper & upper::FLAGS
    }

    /// Whether the upper word is a known instruction with reserved bits zero.
    #[must_use]
    pub const fn upper_is_valid(self) -> bool {
        !matches!(self.upper_op, UpperOp::Invalid)
            && self.upper & !upper::fixed_mask(self.upper_op, self.upper) == 0
    }

    /// Whether the lower word is a known instruction with reserved bits zero
    /// (always true for an immediate).
    #[must_use]
    pub const fn lower_is_valid(self) -> bool {
        !matches!(self.lower_op, LowerOp::Invalid)
            && self.lower & !lower::allowed_mask(self.lower_op, self.lower) == 0
    }

    /// Whether both halves are valid.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.upper_is_valid() && self.lower_is_valid()
    }

    /// The branch target of the lower instruction of a pair at byte address
    /// `pc` in micro memory: the next pair plus the offset in pairs.
    #[must_use]
    pub const fn branch_target(self, pc: u32) -> u32 {
        pc.wrapping_add(8)
            .wrapping_add((lower::imm11(self.lower) * 8) as u32)
    }

    /// The pair written with branch targets resolved against its byte address
    /// `pc` in micro memory.
    #[must_use]
    pub const fn display(self, pc: u32) -> Display {
        Display {
            pair: self,
            pc: Some(pc),
        }
    }
}

/// A pair written as `upper[flags] | lower`. Made by [`Pair::display`].
#[derive(Clone, Copy, Debug)]
pub struct Display {
    pair: Pair,
    pc: Option<u32>,
}

impl fmt::Display for Display {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pair = self.pair;
        if pair.upper_is_valid() {
            upper::write(f, pair.upper_op, pair.upper)?;
            for (flag, name) in [
                (FLAG_E, "[e]"),
                (FLAG_M, "[m]"),
                (FLAG_D, "[d]"),
                (FLAG_T, "[t]"),
            ] {
                if pair.upper & flag != 0 {
                    f.write_str(name)?;
                }
            }
        } else {
            write!(f, ".word 0x{:08X}", pair.upper)?;
        }
        f.write_str(" | ")?;
        if pair.lower_is_valid() {
            lower::write(f, pair.lower_op, pair.lower, self.pc)
        } else {
            write!(f, ".word 0x{:08X}", pair.lower)
        }
    }
}

impl fmt::Display for Pair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(
            &Display {
                pair: *self,
                pc: None,
            },
            f,
        )
    }
}

#[cfg(test)]
mod tests;
