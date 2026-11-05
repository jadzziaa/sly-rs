//! Starting and running commands (`IPU.cpp`'s `IPUCMD_WRITE`, `ipuBCLR`,
//! `ipuIDEC`, `ipuBDEC`, `ipuSETTH`; `IPU_MultiISA.cpp`'s `IPUWorker`,
//! `ipuSETIQ`, `ipuSETVQ`, `ipuCSC`, `ipuPACK`).

use crate::Ipu;
use crate::color;
use crate::decode::{
    DCT_TYPE_INTERLACED, I_TYPE, MACROBLOCK_INTRA, MACROBLOCK_PATTERN, NON_LINEAR_QUANTIZER_SCALE,
    Output,
};
use crate::regs::{
    BUSY, CMD_BUSY, CTRL_AS, CTRL_BUSY, CTRL_ECD, CTRL_IVF, CTRL_MP1, CTRL_QST, CTRL_SCD, TOP_BUSY,
};

/// Command codes (`IPU_CMD` bits 28-31).
pub(crate) const BCLR: u32 = 0;
pub(crate) const IDEC: u32 = 1;
pub(crate) const BDEC: u32 = 2;
pub(crate) const VDEC: u32 = 3;
pub(crate) const FDEC: u32 = 4;
pub(crate) const SETIQ: u32 = 5;
pub(crate) const SETVQ: u32 = 6;
pub(crate) const CSC: u32 = 7;
pub(crate) const PACK: u32 = 8;
pub(crate) const SETTH: u32 = 9;

/// The command in progress and where it stopped (PCSX2's `ipu_cmd`).
#[derive(Clone, Debug)]
pub(crate) struct Cmd {
    /// Macroblocks done (CSC and PACK).
    pub(crate) index: i32,
    /// Resume points, by nesting level.
    pub(crate) pos: [i32; 6],
    /// The command word as written.
    pub(crate) current: u32,
}

impl Default for Cmd {
    fn default() -> Self {
        Self {
            index: 0,
            pos: [0; 6],
            current: 0xFFFF_FFFF,
        }
    }
}

impl Cmd {
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

impl Ipu {
    /// A write to `IPU_CMD` (`IPUCMD_WRITE`). BCLR and SETTH finish at once;
    /// IDEC and BDEC wait for [`Ipu::run`] (PCSX2 starts them 64 cycles
    /// later); the others run as far as they can straight away.
    pub(crate) fn command(&mut self, val: u32) {
        self.set_ctrl_flag(CTRL_ECD, false);
        self.set_ctrl_flag(CTRL_SCD, false);
        self.cmd.clear();
        self.cmd.current = val;
        let code = val >> 28;
        match code {
            BCLR => {
                // Empties the input FIFO and sets the bit pointer.
                self.bs.fifo.clear();
                self.bs.reset_pointer();
                self.bs.bp = val & 0x7F;
                self.regs[CMD_BUSY] = 0;
                self.irq = true;
                self.set_ctrl_flag(CTRL_BUSY, false);
                return;
            }
            SETTH => {
                self.thresh = [(val & 0x1FF) as u16, ((val >> 16) & 0x1FF) as u16];
                self.irq = true;
                self.set_ctrl_flag(CTRL_BUSY, false);
                return;
            }
            IDEC => {
                self.bs.advance(val & 0x3F);
                self.setup_idec(val);
                self.regs[TOP_BUSY] = BUSY;
            }
            BDEC => {
                self.bs.advance(val & 0x3F);
                self.setup_bdec(val);
                self.regs[TOP_BUSY] = BUSY;
            }
            VDEC | FDEC => {
                self.bs.advance(val & 0x3F);
                self.regs[CMD_BUSY] = BUSY;
                self.regs[TOP_BUSY] = BUSY;
            }
            SETIQ => self.bs.advance(val & 0x3F),
            _ => {}
        }
        self.set_ctrl_flag(CTRL_BUSY, true);
        if code == IDEC || code == BDEC {
            self.bs.waiting = false;
            self.waiting_out = false;
        } else {
            self.worker();
        }
    }

    /// The decoder settings IDEC takes from `IPU_CTRL` and the command
    /// (`ipuIDEC`). It sets the picture type to I.
    fn setup_idec(&mut self, val: u32) {
        self.set_ctrl_pct(I_TYPE as u32);
        self.decoder_from_ctrl();
        self.dec.coding_type = self.ctrl_pct();
        // The raw quantiser scale code: until a macroblock sets it, it is
        // not mapped or doubled, as PCSX2 has it.
        self.dec.quantizer_scale = ((val >> 16) & 0x1F) as i32;
        self.dec.frame_pred_frame_dct = val & (1 << 24) == 0;
        self.dec.sgn = val & (1 << 25) != 0;
        self.dec.dte = val & (1 << 26) != 0;
        self.dec.ofm = val & (1 << 27) != 0;
        self.dec.dcr = true;
    }

    /// The decoder settings for BDEC (`ipuBDEC`).
    fn setup_bdec(&mut self, val: u32) {
        self.dec.coding_type = I_TYPE;
        self.decoder_from_ctrl();
        let qsc = ((val >> 16) & 0x1F) as usize;
        self.dec.quantizer_scale = if self.dec.q_scale_type {
            NON_LINEAR_QUANTIZER_SCALE[qsc]
        } else {
            (qsc << 1) as i32
        };
        self.dec.macroblock_modes = if val & (1 << 25) != 0 {
            DCT_TYPE_INTERLACED
        } else {
            0
        };
        self.dec.dcr = val & (1 << 26) != 0;
        self.dec.macroblock_modes |= if val & (1 << 27) != 0 {
            MACROBLOCK_INTRA
        } else {
            MACROBLOCK_PATTERN
        };
        self.dec.mb8.fill(0);
        self.dec.mb16.fill(0);
    }

    fn decoder_from_ctrl(&mut self) {
        self.dec.mpeg1 = self.ctrl_flag(CTRL_MP1);
        self.dec.q_scale_type = self.ctrl_flag(CTRL_QST);
        self.dec.intra_vlc_format = self.ctrl_flag(CTRL_IVF);
        self.dec.scantype = self.ctrl_flag(CTRL_AS);
        self.dec.intra_dc_precision = self.ctrl_idp();
    }

    /// Runs the current command until it finishes or waits (`IPUWorker`);
    /// on finishing, clears the busy bits and raises the interrupt. Called
    /// only while busy.
    pub(crate) fn worker(&mut self) {
        match self.cmd.current >> 28 {
            IDEC | BDEC | VDEC | FDEC => {
                let done = match self.cmd.current >> 28 {
                    IDEC => self.idec(),
                    BDEC => self.bdec(),
                    VDEC => self.vdec(),
                    _ => self.fdec(),
                };
                if !done {
                    return;
                }
                self.regs[TOP_BUSY] = 0;
                self.regs[CMD_BUSY] = 0;
            }
            SETIQ if !self.setiq() => return,
            SETVQ if !self.setvq() => return,
            CSC if !self.csc() => return,
            PACK if !self.pack() => return,
            // Codes 10 to 15 are undefined; PCSX2 assumes they never come.
            // Here they finish at once.
            _ => {}
        }
        self.set_ctrl_flag(CTRL_BUSY, false);
        self.irq = true;
    }

    /// SETIQ: 64 bytes into the intra (bit 27 clear) or non-intra quantiser
    /// matrix.
    fn setiq(&mut self) -> bool {
        let non_intra = (self.cmd.current >> 27) & 1 != 0;
        while self.cmd.pos[0] < 8 {
            let Some(bytes) = self.bs.bytes::<8>(true) else {
                return false;
            };
            let at = 8 * self.cmd.pos[0] as usize;
            let matrix = if non_intra {
                &mut self.dec.niq
            } else {
                &mut self.dec.iq
            };
            matrix[at..at + 8].copy_from_slice(&bytes);
            self.cmd.pos[0] += 1;
        }
        true
    }

    /// SETVQ: the 16-entry RGB16 colour table PACK matches against.
    fn setvq(&mut self) -> bool {
        while self.cmd.pos[0] < 4 {
            let Some(bytes) = self.bs.bytes::<8>(true) else {
                return false;
            };
            let at = 4 * self.cmd.pos[0] as usize;
            for (k, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
                self.vqclut[at + k] = u16::from_le_bytes([pair[0], pair[1]]);
            }
            self.cmd.pos[0] += 1;
        }
        true
    }

    /// CSC: converts MBC macroblocks of raw YCbCr (384 bytes each) to RGB32
    /// or, with OFM, RGB16 (dithered with DTE).
    fn csc(&mut self) -> bool {
        let val = self.cmd.current;
        let mbc = (val & 0x7FF) as i32;
        let dte = val & (1 << 26) != 0;
        let ofm = val & (1 << 27) != 0;
        while self.cmd.index < mbc {
            while self.cmd.pos[0] < 48 {
                let Some(bytes) = self.bs.bytes::<8>(true) else {
                    return false;
                };
                let at = 8 * self.cmd.pos[0] as usize;
                self.dec.mb8[at..at + 8].copy_from_slice(&bytes);
                self.cmd.pos[0] += 1;
            }
            color::csc(&self.dec.mb8, &mut self.dec.rgb32, self.thresh, false);
            let (out, total) = if ofm {
                color::dither(&self.dec.rgb32, &mut self.dec.rgb16, dte);
                (Output::Rgb16, 32)
            } else {
                (Output::Rgb32, 64)
            };
            if !self.output_part(out, total) {
                return false;
            }
            self.cmd.index += 1;
        }
        true
    }

    /// PACK: converts MBC macroblocks of RGB32 (1024 bytes each) to RGB16
    /// (OFM) or to 4-bit indices into the VQ table, dithered with DTE.
    fn pack(&mut self) -> bool {
        let val = self.cmd.current;
        let mbc = (val & 0x7FF) as i32;
        let dte = val & (1 << 26) != 0;
        let ofm = val & (1 << 27) != 0;
        while self.cmd.index < mbc {
            while self.cmd.pos[0] < 128 {
                let Some(bytes) = self.bs.bytes::<8>(true) else {
                    return false;
                };
                let at = 8 * self.cmd.pos[0] as usize;
                self.dec.rgb32[at..at + 8].copy_from_slice(&bytes);
                self.cmd.pos[0] += 1;
            }
            color::dither(&self.dec.rgb32, &mut self.dec.rgb16, dte);
            let (out, total) = if ofm {
                (Output::Rgb16, 32)
            } else {
                color::vq(&self.dec.rgb16, &self.vqclut, &mut self.dec.indx4);
                (Output::Indx4, 8)
            };
            if !self.output_part(out, total) {
                return false;
            }
            self.cmd.index += 1;
        }
        true
    }

    /// Writes the rest of a CSC or PACK macroblock from quadword `pos[1]`;
    /// false while some is left. Done, it clears `pos[0]` and `pos[1]`.
    fn output_part(&mut self, out: Output, total: i32) -> bool {
        let from = self.cmd.pos[1];
        self.cmd.pos[1] += self.write_from(out, from as usize, total as usize) as i32;
        if self.cmd.pos[1] < total {
            self.waiting_out = true;
            return false;
        }
        self.cmd.pos[0] = 0;
        self.cmd.pos[1] = 0;
        true
    }
}
