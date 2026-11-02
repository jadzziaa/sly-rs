//! Function fingerprints: a hash of a function's code with the
//! address-dependent bits masked, so the same source compiled into another
//! executable (an SDK library, shared engine code) hashes the same.
//!
//! Masked: `j`/`jal` targets, `lui` immediates, and the 16-bit immediate of
//! any instruction whose base or source register was last set by a `lui`
//! (the `%lo` half of an address). Branch offsets are relative and kept;
//! so are stack and structure offsets.

use ps2_isa::ee::{Insn, Op};

use crate::{Function, Image};

/// Functions shorter than this many instructions are too common to identify
/// by fingerprint.
pub const MIN_INSTRUCTIONS: u32 = 6;

/// The fingerprint of `function`, or `None` if it is shorter than
/// [`MIN_INSTRUCTIONS`].
#[must_use]
pub fn fingerprint(image: &Image, function: &Function) -> Option<u64> {
    let len = (function.end - function.start) / 4;
    if len < MIN_INSTRUCTIONS {
        return None;
    }
    // Registers holding a `lui` result (bit set).
    let mut hi_regs: u32 = 0;
    let mut hash = Fnv::new();
    hash.write(len);
    for at in (function.start..function.end).step_by(4) {
        let insn = Insn::decode(image.word(at)?);
        let word = insn.word();
        let masked = match insn.op() {
            Op::J | Op::Jal => word & 0xFC00_0000,
            Op::Lui => word & 0xFFFF_0000,
            _ if hi_regs & (1 << insn.rs()) != 0 && uses_lo_immediate(insn.op()) => {
                word & 0xFFFF_0000
            }
            _ => word,
        };
        hash.write(masked);
        if let Some(dest) = insn.gpr_dest() {
            if insn.op() == Op::Lui {
                hi_regs |= 1 << dest;
            } else if !(uses_lo_immediate(insn.op())
                && hi_regs & (1 << insn.rs()) != 0
                && dest == insn.rs())
            {
                hi_regs &= !(1 << dest);
            }
        }
    }
    Some(hash.finish())
}

/// Operations whose immediate can be the `%lo` half of an address.
const fn uses_lo_immediate(op: Op) -> bool {
    matches!(
        op,
        Op::Addiu
            | Op::Daddiu
            | Op::Ori
            | Op::Lb
            | Op::Lbu
            | Op::Lh
            | Op::Lhu
            | Op::Lw
            | Op::Lwu
            | Op::Ld
            | Op::Lq
            | Op::Lwc1
            | Op::Lqc2
            | Op::Sb
            | Op::Sh
            | Op::Sw
            | Op::Sd
            | Op::Sq
            | Op::Swc1
            | Op::Sqc2
    )
}

/// FNV-1a, 64-bit: small, stable across Rust versions and platforms.
struct Fnv(u64);

impl Fnv {
    const fn new() -> Self {
        Self(0xCBF2_9CE4_8422_2325)
    }

    fn write(&mut self, word: u32) {
        for byte in word.to_le_bytes() {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3);
        }
    }

    const fn finish(&self) -> u64 {
        self.0
    }
}
