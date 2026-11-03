//! The vector units (plan §5, phase 6): VU0 and VU1 in micro mode, and the
//! VIFs that feed them.
//!
//! A VU is its registers (`ps2_core::Vu0`'s layout: VU0's are the EE's COP2
//! registers) and its micro and data memories, which live in
//! `ps2_core::Regions` where the EE also reaches them. The VIF writes data
//! memory (UNPACK) and micro memory (MPG) and starts programs, which run to
//! their end at once ([`micro`]); VU1's XGKICK sends GIF packets from data
//! memory to the GS on PATH1.

// VU registers and memory are bit patterns: the casts reinterpret them.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "register bit patterns"
)]

pub mod micro;
pub mod vif;

pub use ps2_core::Vu0 as Regs;
pub use vif::Vif;

/// A VU's registers at reset: `vf0` reads (0, 0, 0, 1.0).
#[must_use]
pub fn reset_regs() -> Regs {
    let mut regs = Regs::default();
    regs.vf[0] = [0, 0, 0, 0x3F80_0000];
    regs
}

/// A VU's memories, as the VIF writes them.
pub struct Memories<'a> {
    /// Micro memory.
    pub code: &'a mut [u8],
    /// Data memory.
    pub data: &'a mut [u8],
}

impl Memories<'_> {
    /// Writes a word of data memory (the address wraps).
    pub fn write_data32(&mut self, address: u32, value: u32) {
        let at = (address & (self.data.len() as u32 - 1) & !3) as usize;
        self.data[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// A word of data memory.
    #[must_use]
    pub fn read_data32(&self, address: u32) -> u32 {
        let at = (address & (self.data.len() as u32 - 1) & !3) as usize;
        u32::from_le_bytes([
            self.data[at],
            self.data[at + 1],
            self.data[at + 2],
            self.data[at + 3],
        ])
    }

    /// Writes an instruction pair into micro memory at byte `address`.
    pub fn write_micro(&mut self, address: u32, lower: u32, upper: u32) {
        let at = (address & (self.code.len() as u32 - 1) & !7) as usize;
        self.code[at..at + 4].copy_from_slice(&lower.to_le_bytes());
        self.code[at + 4..at + 8].copy_from_slice(&upper.to_le_bytes());
    }
}
