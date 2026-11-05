//! Register reads and writes and the soft reset (`IPU.cpp`'s
//! `ipuRead32`, `ipuRead64`, `ipuWrite32`, `ipuWrite64`, `ipuSoftReset`;
//! the field layouts are `IPU.h`'s).
//!
//! The registers sit in a 256-byte page, mirrored through `0x1000_2000` to
//! `0x1000_20FF` as PCSX2 reads it. Words PCSX2 does not handle itself read
//! back what was written there.

use crate::Ipu;

/// Word indices in the register page.
pub(crate) const CMD_DATA: usize = 0;
pub(crate) const CMD_BUSY: usize = 1;
pub(crate) const CTRL: usize = 4;
pub(crate) const BP: usize = 8;
pub(crate) const TOP: usize = 12;
pub(crate) const TOP_BUSY: usize = 13;

/// The busy bit of `IPU_CMD`'s and `IPU_TOP`'s upper words and of `IPU_CTRL`.
pub(crate) const BUSY: u32 = 0x8000_0000;

/// `IPU_CTRL` fields.
const CTRL_IFC: u32 = 0xF;
const CTRL_OFC: u32 = 0xF0;
const CTRL_CBP: u32 = 0x3F00;
pub(crate) const CTRL_ECD: u32 = 1 << 14;
pub(crate) const CTRL_SCD: u32 = 1 << 15;
const CTRL_IDP_SHIFT: u32 = 16;
pub(crate) const CTRL_AS: u32 = 1 << 20;
pub(crate) const CTRL_IVF: u32 = 1 << 21;
pub(crate) const CTRL_QST: u32 = 1 << 22;
pub(crate) const CTRL_MP1: u32 = 1 << 23;
const CTRL_PCT_SHIFT: u32 = 24;
const CTRL_RST: u32 = 1 << 30;
pub(crate) const CTRL_BUSY: u32 = 1 << 31;

impl Ipu {
    /// `IPU_CTRL` as stored, with the live output FIFO count.
    pub(crate) fn ctrl(&self) -> u32 {
        (self.regs[CTRL] & !CTRL_OFC) | ((self.out.count as u32) << 4)
    }

    pub(crate) fn ctrl_flag(&self, flag: u32) -> bool {
        self.regs[CTRL] & flag != 0
    }

    pub(crate) fn set_ctrl_flag(&mut self, flag: u32, on: bool) {
        if on {
            self.regs[CTRL] |= flag;
        } else {
            self.regs[CTRL] &= !flag;
        }
    }

    /// The intra DC precision (IDP): 8 to 10 bits as 0 to 2.
    pub(crate) fn ctrl_idp(&self) -> i32 {
        ((self.regs[CTRL] >> CTRL_IDP_SHIFT) & 3) as i32
    }

    /// The picture type (PCT).
    pub(crate) fn ctrl_pct(&self) -> i32 {
        ((self.regs[CTRL] >> CTRL_PCT_SHIFT) & 7) as i32
    }

    pub(crate) fn set_ctrl_pct(&mut self, pct: u32) {
        self.regs[CTRL] = (self.regs[CTRL] & !(7 << CTRL_PCT_SHIFT)) | (pct << CTRL_PCT_SHIFT);
    }

    /// A register word as a raw read sees it.
    fn word(&self, word: usize) -> u32 {
        match word {
            CTRL => self.ctrl(),
            w if w < self.regs.len() => self.regs[w],
            _ => 0,
        }
    }

    /// A read at `address`: the word there and the one after.
    pub(crate) fn read_reg(&mut self, address: u32) -> u64 {
        let off = (address & 0xFF) as usize;
        match off {
            0x00 => {
                // Unless a VDEC or FDEC left its result here, IPU_CMD shows
                // the next 32 bits of the stream (loading them if it can).
                let cmd = self.cmd.current >> 28;
                if cmd != crate::commands::FDEC
                    && cmd != crate::commands::VDEC
                    && let Some(v) = self.bs.peek32()
                {
                    self.regs[CMD_DATA] = v;
                }
            }
            0x10 => {
                let ifc = self.bs.fifo.count as u32;
                let cbp = (self.cbp as u32) & 0x3F;
                self.regs[CTRL] = (self.regs[CTRL] & !(CTRL_IFC | CTRL_CBP)) | ifc | (cbp << 8);
            }
            0x20 => {
                self.regs[BP] =
                    (self.bs.bp & 0x7F) | ((self.bs.fifo.count as u32) << 8) | (self.bs.fp << 16);
            }
            _ => {}
        }
        let word = off / 4;
        u64::from(self.word(word)) | (u64::from(self.word(word + 1)) << 32)
    }

    /// A write at `address` (its low 32 bits; only `IPU_CMD` takes more,
    /// and uses 32 of them).
    pub(crate) fn write_reg(&mut self, address: u32, value: u64) {
        let value = value as u32;
        match address & 0xFFF {
            0x00 => self.command(value),
            0x10 => {
                // Bits 16-26 and 30 are written (less the reserved 18-19);
                // the counters, CBP, ECD, SCD and BUSY are kept.
                self.regs[CTRL] = (value & 0x47F3_0000) | (self.regs[CTRL] & 0x8000_FFFF);
                if self.ctrl_idp() == 3 {
                    // An invalid precision is taken as 9 bits.
                    self.regs[CTRL] &= !(3 << CTRL_IDP_SHIFT);
                    self.regs[CTRL] |= 1 << CTRL_IDP_SHIFT;
                }
                if self.ctrl_flag(CTRL_RST) {
                    self.soft_reset();
                }
            }
            off if off < 0x100 && off.trailing_zeros() >= 2 => self.regs[off as usize / 4] = value,
            _ => {}
        }
    }

    /// `IPU_CTRL`'s reset bit (`ipuSoftReset`): empties both FIFOs and the bit
    /// buffer, clears the thresholds, the error and busy state and the
    /// command, and raises the interrupt. Quantiser matrices and the VQ
    /// colour table are kept.
    pub(crate) fn soft_reset(&mut self) {
        self.bs.fifo.clear();
        self.out.clear();
        self.bs.reset_pointer();
        self.cbp = 0;
        self.thresh = [0; 2];
        if self.dec.intra_dc_precision == 0 {
            self.dec.reset_dc_pred();
        }
        // Keeps CBP, IDP, AS, IVF, QST, MP1 and PCT.
        self.regs[CTRL] &= 0x07F3_3F00;
        self.regs[TOP] = 0;
        self.cmd.clear();
        self.regs[CMD_BUSY] = 0;
        self.regs[CMD_DATA] = 0;
        self.irq = true;
    }
}
