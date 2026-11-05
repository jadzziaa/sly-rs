//! MPEG-2 decoding: macroblock types and address increments, DC and AC
//! coefficients with their inverse quantisation, and the IDEC, BDEC, VDEC
//! and FDEC commands (`IPU_MultiISA.cpp`: `GetMacroblockModes`,
//! `get_macroblock_address_increment`, `get_*_dc_dct_diff`,
//! `get_intra_block`, `get_non_intra_block`, `slice_*_DCT`,
//! `mpeg2sliceIDEC`, `mpeg2_slice`, `ipuVDEC`, `ipuFDEC`).
//!
//! Each command is a resumable step function, as in PCSX2: it returns
//! false when it has to wait for input or output, having saved where it
//! was in the command state's `pos` array, and true when done.

use crate::Ipu;
use crate::color::{self, CB, CR, MB8_BYTES};
use crate::idct::{MPEG2_SCAN_ALT, MPEG2_SCAN_NORM, idct_add, idct_copy};
use crate::regs::{CMD_DATA, CTRL_ECD, CTRL_MP1, CTRL_SCD, TOP};
use crate::vlc::{
    CBP_7, CBP_9, DC_CHROM0, DC_CHROM1, DC_LUM0, DC_LUM1, DCT_FIRST, DCT_NEXT, DCT_TAB0, DCT_TAB0A,
    DCT_TAB1, DCT_TAB1A, DCT_TAB2, DCT_TAB3, DCT_TAB4, DCT_TAB5, DCT_TAB6, DMV_2, Dct, MB_B, MB_I,
    MB_P, MBA_5, MBA_11, MV_4, MV_10,
};

pub(crate) const MACROBLOCK_INTRA: i32 = 1;
pub(crate) const MACROBLOCK_PATTERN: i32 = 2;
const MACROBLOCK_MOTION_FORWARD: i32 = 8;
const MACROBLOCK_QUANT: i32 = 16;
pub(crate) const DCT_TYPE_INTERLACED: i32 = 32;
const MOTION_TYPE_BASE: i32 = 64;
const MC_FRAME: i32 = 2 * 64;

/// `picture_structure` for a frame picture.
pub(crate) const FRAME_PICTURE: i32 = 3;
/// Picture coding types.
pub(crate) const I_TYPE: i32 = 1;
const P_TYPE: i32 = 2;
const B_TYPE: i32 = 3;
const D_TYPE: i32 = 4;

/// The decoder's row stride in samples (a quadword of bytes).
const STRIDE: usize = 16;

/// Quantiser scales for `q_scale_type` 1.
pub(crate) const NON_LINEAR_QUANTIZER_SCALE: [i32; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 18, 20, 22, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64,
    72, 80, 88, 96, 104, 112,
];

/// Which buffer the output FIFO is being fed from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Output {
    /// Decoded samples as 16-bit values (BDEC), 48 quadwords.
    #[default]
    Mb16,
    /// RGBA bytes, 64 quadwords.
    Rgb32,
    /// 5:5:5:1 pixels, 32 quadwords.
    Rgb16,
    /// 4-bit indices (PACK), 8 quadwords.
    Indx4,
}

/// The decoder's state (PCSX2's `decoder_t`) and its buffers.
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools, reason = "PCSX2's flags")]
pub(crate) struct Decoder {
    pub(crate) dct_block: [i16; 64],
    /// Non-intra and intra quantiser matrices, in scan order.
    pub(crate) niq: [u8; 64],
    pub(crate) iq: [u8; 64],
    pub(crate) mb8: [u8; MB8_BYTES],
    pub(crate) mb16: [i16; MB8_BYTES],
    pub(crate) rgb32: [u8; 1024],
    pub(crate) rgb16: [u16; 256],
    /// PACK's index output (PCSX2's `g_ipu_indx4`).
    pub(crate) indx4: [u8; 128],
    /// The buffer being output, the next quadword and how many remain.
    pub(crate) out: Output,
    pub(crate) out_idx: usize,
    pub(crate) out_left: usize,
    pub(crate) quantizer_scale: i32,
    pub(crate) coding_type: i32,
    pub(crate) dc_dct_pred: [i16; 3],
    pub(crate) intra_dc_precision: i32,
    pub(crate) picture_structure: i32,
    pub(crate) frame_pred_frame_dct: bool,
    pub(crate) q_scale_type: bool,
    pub(crate) intra_vlc_format: bool,
    /// Sign offset, dither and RGB16 output (IDEC's SGN, DTE and OFM).
    pub(crate) sgn: bool,
    pub(crate) dte: bool,
    pub(crate) ofm: bool,
    pub(crate) macroblock_modes: i32,
    /// Reset the DC predictors (BDEC's DCR).
    pub(crate) dcr: bool,
    pub(crate) coded_block_pattern: i32,
    /// The alternate scan.
    pub(crate) scantype: bool,
    pub(crate) mpeg1: bool,
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            dct_block: [0; 64],
            niq: [0; 64],
            iq: [0; 64],
            mb8: [0; MB8_BYTES],
            mb16: [0; MB8_BYTES],
            rgb32: [0; 1024],
            rgb16: [0; 256],
            indx4: [0; 128],
            out: Output::Mb16,
            out_idx: 0,
            out_left: 0,
            quantizer_scale: 0,
            coding_type: 0,
            dc_dct_pred: [0; 3],
            intra_dc_precision: 0,
            picture_structure: FRAME_PICTURE,
            frame_pred_frame_dct: false,
            q_scale_type: false,
            intra_vlc_format: false,
            sgn: false,
            dte: false,
            ofm: false,
            macroblock_modes: 0,
            dcr: false,
            coded_block_pattern: 0,
            scantype: false,
            mpeg1: false,
        }
    }
}

impl Decoder {
    /// Points the output at a whole buffer (`SetOutputTo`).
    pub(crate) fn set_output(&mut self, out: Output) {
        self.out = out;
        self.out_idx = 0;
        self.out_left = match out {
            Output::Mb16 => 48,
            Output::Rgb32 => 64,
            Output::Rgb16 => 32,
            Output::Indx4 => 8,
        };
    }

    /// Quadword `k` of a buffer, as it sits in memory.
    pub(crate) fn qword(&self, out: Output, k: usize) -> u128 {
        let mut b = [0u8; 16];
        match out {
            Output::Mb16 => {
                for (d, s) in b
                    .as_chunks_mut::<2>()
                    .0
                    .iter_mut()
                    .zip(&self.mb16[8 * k..8 * k + 8])
                {
                    d.copy_from_slice(&s.to_le_bytes());
                }
            }
            Output::Rgb32 => b.copy_from_slice(&self.rgb32[16 * k..16 * k + 16]),
            Output::Rgb16 => {
                for (d, s) in b
                    .as_chunks_mut::<2>()
                    .0
                    .iter_mut()
                    .zip(&self.rgb16[8 * k..8 * k + 8])
                {
                    d.copy_from_slice(&s.to_le_bytes());
                }
            }
            Output::Indx4 => b.copy_from_slice(&self.indx4[16 * k..16 * k + 16]),
        }
        u128::from_le_bytes(b)
    }

    /// Sets the three DC predictors to their reset value.
    pub(crate) fn reset_dc_pred(&mut self) {
        self.dc_dct_pred = [(128 << self.intra_dc_precision) as i16; 3];
    }

    /// The first block row offset and the row stride of the luma blocks:
    /// frame DCT has blocks 8 rows apart, field DCT interleaves them.
    fn dct_layout(&self) -> (usize, usize) {
        if self.macroblock_modes & DCT_TYPE_INTERLACED != 0 {
            (STRIDE, STRIDE * 2)
        } else {
            (STRIDE * 8, STRIDE)
        }
    }
}

/// Clamps a coefficient to 12 bits (`SATURATE`).
fn saturate(val: i32) -> i32 {
    if (val.wrapping_add(2048) as u32) > 4095 {
        (val >> 31) ^ 2047
    } else {
        val
    }
}

/// `(val + ~(val >> 31)) | 1`: MPEG-1's oddification of escaped levels.
fn oddify(val: i32) -> i32 {
    val.wrapping_add(!(val >> 31)) | 1
}

impl Ipu {
    /// Writes as much of the current output buffer as fits; returns how
    /// many quadwords went.
    pub(crate) fn write_output(&mut self) -> usize {
        let dec = &self.dec;
        let (out, idx) = (dec.out, dec.out_idx);
        let n = self.out.write(dec.out_left, |k| dec.qword(out, idx + k));
        self.dec.out_idx += n;
        self.dec.out_left -= n;
        n
    }

    /// Writes quadwords `from..to` of `out`; returns how many went.
    pub(crate) fn write_from(&mut self, out: Output, from: usize, to: usize) -> usize {
        let dec = &self.dec;
        self.out.write(to - from, |k| dec.qword(out, from + k))
    }

    /// Whether IDEC and BDEC may decode a macroblock: the fromIPU DMA is
    /// running with data to move and the output FIFO is empty.
    fn out_ready(&self) -> bool {
        self.out_dma_ready && self.out.count == 0
    }

    /// The macroblock type for the current picture type
    /// (`GetMacroblockModes`); B pictures add the code length at bit 16.
    fn macroblock_modes(&mut self) -> i32 {
        let frame = self.dec.picture_structure == FRAME_PICTURE;
        let frame_dct = self.dec.frame_pred_frame_dct;
        match self.dec.coding_type {
            I_TYPE => {
                let code = self.bs.ubits(2);
                if code == 0 {
                    return 0;
                }
                let tab = MB_I[(code >> 1) as usize];
                self.bs.advance(tab.len.into());
                let mut modes = i32::from(tab.modes);
                if !frame_dct && frame {
                    modes |= self.bs.getbits(1) as i32 * DCT_TYPE_INTERLACED;
                }
                modes
            }
            P_TYPE => {
                let code = self.bs.ubits(6);
                if code == 0 {
                    return 0;
                }
                let tab = MB_P[(code >> 1) as usize];
                self.bs.advance(tab.len.into());
                let mut modes = i32::from(tab.modes);
                let forward = modes & MACROBLOCK_MOTION_FORWARD != 0;
                if !frame {
                    if forward {
                        modes |= self.bs.getbits(2) as i32 * MOTION_TYPE_BASE;
                    }
                } else if frame_dct {
                    if forward {
                        modes |= MC_FRAME;
                    }
                } else {
                    if forward {
                        modes |= self.bs.getbits(2) as i32 * MOTION_TYPE_BASE;
                    }
                    if modes & (MACROBLOCK_INTRA | MACROBLOCK_PATTERN) != 0 {
                        modes |= self.bs.getbits(1) as i32 * DCT_TYPE_INTERLACED;
                    }
                }
                modes
            }
            B_TYPE => {
                let code = self.bs.ubits(6);
                if code == 0 {
                    return 0;
                }
                let tab = MB_B[code as usize];
                self.bs.advance(tab.len.into());
                let mut modes = i32::from(tab.modes);
                let intra = modes & MACROBLOCK_INTRA != 0;
                if !frame {
                    if !intra {
                        modes |= self.bs.getbits(2) as i32 * MOTION_TYPE_BASE;
                    }
                } else if frame_dct {
                    modes |= MC_FRAME;
                } else {
                    if !intra {
                        modes |= self.bs.getbits(2) as i32 * MOTION_TYPE_BASE;
                    }
                    if modes & (MACROBLOCK_INTRA | MACROBLOCK_PATTERN) != 0 {
                        modes |= self.bs.getbits(1) as i32 * DCT_TYPE_INTERLACED;
                    }
                }
                modes | (i32::from(tab.len) << 16)
            }
            D_TYPE => {
                if self.bs.getbits(1) == 0 {
                    return 0;
                }
                MACROBLOCK_INTRA | (1 << 16)
            }
            _ => 0,
        }
    }

    /// The macroblock address increment with its code length at bit 16;
    /// 0 for an invalid code (`get_macroblock_address_increment`).
    fn macroblock_address_increment(&mut self) -> i32 {
        let code = self.bs.ubits(16);
        let mba = if code >= 4096 {
            MBA_5[(self.bs.ubits(5) - 2) as usize]
        } else if code >= 768 {
            MBA_11[(self.bs.ubits(11) - 24) as usize]
        } else {
            match self.bs.ubits(11) {
                8 => {
                    // macroblock_escape
                    self.bs.advance(11);
                    return 0xB_0023;
                }
                15 if self.dec.mpeg1 => {
                    // macroblock_stuffing
                    self.bs.advance(11);
                    return 0xB_0022;
                }
                _ => return 0,
            }
        };
        self.bs.advance(mba.len.into());
        (i32::from(mba.mba) + 1) | (i32::from(mba.len) << 16)
    }

    /// Reads a DC difference of `size` bits.
    fn dc_diff(&mut self, size: u8) -> i32 {
        if size == 0 {
            return 0;
        }
        let size = u32::from(size);
        let mut diff = self.bs.getbits(size) as i32;
        if diff & (1 << (size - 1)) == 0 {
            diff -= (1 << size) - 1;
        }
        diff
    }

    /// A luma DC difference (`get_luma_dc_dct_diff`).
    fn luma_dc_diff(&mut self) -> i32 {
        let code = self.bs.ubits(5) as usize;
        let tab = if code < 31 {
            DC_LUM0[code]
        } else {
            DC_LUM1[self.bs.ubits(9) as usize - 0x1F0]
        };
        self.bs.advance(tab.len.into());
        self.dc_diff(tab.size)
    }

    /// A chroma DC difference (`get_chroma_dc_dct_diff`).
    fn chroma_dc_diff(&mut self) -> i32 {
        let code = self.bs.ubits(5) as usize;
        let tab = if code < 31 {
            DC_CHROM0[code]
        } else {
            DC_CHROM1[self.bs.ubits(10) as usize - 0x3E0]
        };
        self.bs.advance(tab.len.into());
        self.dc_diff(tab.size)
    }

    /// Looks up an AC coefficient code (16 bits, at least 16); `alt` picks
    /// table B-15 and `first` the first coefficient of a non-intra block.
    pub(crate) fn dct_code(code: usize, alt: bool, first: bool) -> Dct {
        if code >= 16384 && !alt {
            if first {
                DCT_FIRST[(code >> 12) - 4]
            } else {
                DCT_NEXT[(code >> 12) - 4]
            }
        } else if code >= 1024 {
            if alt {
                DCT_TAB0A[(code >> 8) - 4]
            } else {
                DCT_TAB0[(code >> 8) - 4]
            }
        } else if code >= 512 {
            if alt {
                DCT_TAB1A[(code >> 6) - 8]
            } else {
                DCT_TAB1[(code >> 6) - 8]
            }
        } else if code >= 256 {
            DCT_TAB2[(code >> 4) - 16]
        } else if code >= 128 {
            DCT_TAB3[(code >> 3) - 16]
        } else if code >= 64 {
            DCT_TAB4[(code >> 2) - 16]
        } else if code >= 32 {
            DCT_TAB5[(code >> 1) - 16]
        } else {
            DCT_TAB6[code - 16]
        }
    }

    /// An intra block's AC coefficients, inverse quantised into the DCT
    /// block (`get_intra_block`). Resumes from `pos[4]` (the coefficient)
    /// and `pos[5]` (1: the code is read, its level is not).
    fn intra_block(&mut self) -> bool {
        let scan = if self.dec.scantype {
            &MPEG2_SCAN_ALT
        } else {
            &MPEG2_SCAN_NORM
        };
        let qs = self.dec.quantizer_scale;
        let mpeg1 = self.dec.mpeg1;
        let alt = self.dec.intra_vlc_format && !mpeg1;
        let mut i = 1 + self.cmd.pos[4];
        loop {
            if self.cmd.pos[5] == 0 {
                if !self.bs.fill(16) {
                    self.cmd.pos[4] = i - 1;
                    return false;
                }
                let code = self.bs.ubits(16) as usize;
                if code < 16 {
                    self.cmd.pos[4] = 0;
                    return true;
                }
                self.tab = Self::dct_code(code, alt, false);
                self.bs.advance(self.tab.len.into());
                if self.tab.run == 64 {
                    // End of block.
                    self.cmd.pos[4] = 0;
                    return true;
                }
                i += if self.tab.run == 65 {
                    self.bs.getbits(6) as i32
                } else {
                    i32::from(self.tab.run)
                };
                if i >= 64 {
                    self.cmd.pos[4] = 0;
                    return true;
                }
            }

            if !self.bs.fill(16) {
                self.cmd.pos[4] = i - 1;
                self.cmd.pos[5] = 1;
                return false;
            }
            let j = usize::from(scan[i as usize]);
            let qm = i32::from(self.dec.iq[i as usize]);
            let mut val;
            if self.tab.run == 65 {
                // Escape.
                if mpeg1 {
                    val = self.bs.sbits(8);
                    self.bs.advance(8);
                    if val.trailing_zeros() >= 7 {
                        val = self.bs.getbits(8) as i32 + 2 * val;
                    }
                    val = oddify((val * qs * qm) >> 4);
                } else {
                    val = (self.bs.sbits(12) * qs * qm) >> 4;
                    self.bs.advance(12);
                }
            } else {
                val = (i32::from(self.tab.level) * qs * qm) >> 4;
                if mpeg1 {
                    val = (val - 1) | 1;
                }
                let sign = self.bs.sbits(1);
                val = (val ^ sign) - sign;
                self.bs.advance(1);
            }
            self.dec.dct_block[j] = saturate(val) as i16;
            self.cmd.pos[5] = 0;
            i += 1;
        }
    }

    /// A non-intra block's coefficients (`get_non_intra_block`). Resumes
    /// from `pos[4]` and `pos[5]` as [`Self::intra_block`] does.
    fn non_intra_block(&mut self) -> bool {
        let scan = if self.dec.scantype {
            &MPEG2_SCAN_ALT
        } else {
            &MPEG2_SCAN_NORM
        };
        let qs = self.dec.quantizer_scale;
        let mpeg1 = self.dec.mpeg1;
        let mut i = self.cmd.pos[4];
        loop {
            if self.cmd.pos[5] == 0 {
                if !self.bs.fill(16) {
                    self.cmd.pos[4] = i;
                    return false;
                }
                let code = self.bs.ubits(16) as usize;
                if code < 16 {
                    self.cmd.pos[4] = 0;
                    return true;
                }
                self.tab = Self::dct_code(code, false, i == 0);
                self.bs.advance(self.tab.len.into());
                if self.tab.run == 64 {
                    self.cmd.pos[4] = 0;
                    return true;
                }
                i += if self.tab.run == 65 {
                    self.bs.getbits(6) as i32
                } else {
                    i32::from(self.tab.run)
                };
                if i >= 64 {
                    self.cmd.pos[4] = 0;
                    return true;
                }
            }

            if !self.bs.fill(16) {
                self.cmd.pos[4] = i;
                self.cmd.pos[5] = 1;
                return false;
            }
            let j = usize::from(scan[i as usize]);
            let qm = i32::from(self.dec.niq[i as usize]);
            let mut val;
            if self.tab.run == 65 {
                if mpeg1 {
                    val = self.bs.sbits(8);
                    self.bs.advance(8);
                    if val.trailing_zeros() >= 7 {
                        val = self.bs.getbits(8) as i32 + 2 * val;
                    }
                    val = oddify(((2 * (val + (val >> 31)) + 1) * qs * qm) / 32);
                } else {
                    let level = self.bs.sbits(12) + self.bs.sbits(1);
                    val = ((2 * level + 1) * qs * qm) >> 5;
                    self.bs.advance(12);
                }
            } else {
                let sign = self.bs.sbits(1);
                val = ((2 * i32::from(self.tab.level) + 1) * qs * qm) >> 5;
                val = (val ^ sign) - sign;
                self.bs.advance(1);
            }
            self.dec.dct_block[j] = saturate(val) as i16;
            self.cmd.pos[5] = 0;
            i += 1;
        }
    }

    /// An intra block: its DC term (unless `skip` and it was read before
    /// the stall: `pos[3]` says it was not), its AC terms, then the IDCT
    /// into the 8-bit macroblock at `at` (`slice_intra_DCT`).
    fn slice_intra_dct(&mut self, cc: usize, at: usize, stride: usize, skip: bool) -> bool {
        if !skip || self.cmd.pos[3] != 0 {
            self.cmd.pos[3] = 0;
            if !self.bs.fill(16) {
                self.cmd.pos[3] = 1;
                return false;
            }
            let diff = if cc == 0 {
                self.luma_dc_diff()
            } else {
                self.chroma_dc_diff()
            };
            let pred = (i32::from(self.dec.dc_dct_pred[cc]) + diff) as i16;
            self.dec.dc_dct_pred[cc] = pred;
            self.dec.dct_block[0] = (i32::from(pred) << (3 - self.dec.intra_dc_precision)) as i16;
        }
        if !self.intra_block() {
            return false;
        }
        idct_copy(&mut self.dec.dct_block, &mut self.dec.mb8, at, stride);
        true
    }

    /// A non-intra block into the 16-bit macroblock at `at`
    /// (`slice_non_intra_DCT`).
    fn slice_non_intra_dct(&mut self, at: usize, stride: usize, skip: bool) -> bool {
        if !skip {
            self.dec.dct_block = [0; 64];
        }
        if !self.non_intra_block() {
            return false;
        }
        idct_add(&mut self.dec.dct_block, &mut self.dec.mb16, at, stride);
        true
    }

    /// The six blocks of an intra macroblock, resuming from `pos[k]`.
    fn intra_blocks(&mut self, k: usize) -> bool {
        let (off, stride) = self.dec.dct_layout();
        let blocks = [
            (0, 0, stride),
            (0, 8, stride),
            (0, off, stride),
            (0, off + 8, stride),
            (1, CB, STRIDE / 2),
            (2, CR, STRIDE / 2),
        ];
        let s = self.cmd.pos[k];
        for (n, (cc, at, st)) in (1..).zip(blocks) {
            if s <= n && !self.slice_intra_dct(cc, at, st, s == n) {
                self.cmd.pos[k] = n;
                return false;
            }
        }
        true
    }

    /// The coded blocks of a non-intra macroblock, resuming from `pos[1]`;
    /// the coded block pattern is read first.
    fn non_intra_blocks(&mut self) -> bool {
        let s = self.cmd.pos[1];
        if s == 0 {
            let code = self.bs.ubits(16);
            let tab = if code >= 0x2000 {
                CBP_7[(self.bs.ubits(7) - 16) as usize]
            } else {
                CBP_9[self.bs.ubits(9) as usize]
            };
            self.bs.advance(tab.len.into());
            self.dec.coded_block_pattern = i32::from(tab.cbp);
        }
        let (off, stride) = self.dec.dct_layout();
        let blocks = [
            (0x20, 0, stride),
            (0x10, 8, stride),
            (0x08, off, stride),
            (0x04, off + 8, stride),
            (0x02, CB, STRIDE / 2),
            (0x01, CR, STRIDE / 2),
        ];
        let cbp = self.dec.coded_block_pattern;
        for (n, (bit, at, st)) in (1..).zip(blocks) {
            if s <= n && cbp & bit != 0 && !self.slice_non_intra_dct(at, st, s == n) {
                self.cmd.pos[1] = n;
                return false;
            }
        }
        true
    }

    /// After a slice or macroblock: if the next byte is zero, aligns and
    /// skips zero bytes to the next start code, setting SCD for 00 00 01
    /// and ECD for anything else.
    fn start_code(&mut self) -> bool {
        let Some([byte]) = self.bs.bytes::<1>(false) else {
            return false;
        };
        if byte == 0 {
            self.bs.align();
            loop {
                if !self.bs.fill(24) {
                    return false;
                }
                let check = self.bs.ubits(24);
                if check != 0 {
                    self.set_ctrl_flag(if check == 1 { CTRL_SCD } else { CTRL_ECD }, true);
                    break;
                }
                self.bs.advance(8);
            }
        }
        true
    }

    /// Peeks the next 32 bits into `IPU_TOP`.
    fn load_top(&mut self) -> bool {
        match self.bs.peek32() {
            Some(top) => {
                self.regs[TOP] = top;
                true
            }
            None => false,
        }
    }

    /// The yield PCSX2 takes once before writing a decoded macroblock (a
    /// 64-cycle delay there; here [`Ipu::run`] goes straight on).
    fn delay(&mut self) {
        self.bs.waiting = false;
        self.waiting_out = false;
        self.process_pending = true;
    }

    /// IDEC: decodes a slice of intra macroblocks, converting each to RGB
    /// (`mpeg2sliceIDEC`). Resumes from `pos[0]`.
    pub(crate) fn idec(&mut self) -> bool {
        let start = self.cmd.pos[0];
        if start == 0 {
            self.dec.reset_dc_pred();
            self.regs[TOP] = 0;
            self.set_ctrl_flag(CTRL_ECD, false);
        }
        if start <= 1 {
            self.cmd.pos[0] = 1;
            if !self.bs.fill(32) {
                return false;
            }
        }
        if start <= 2 {
            self.cmd.pos[0] = 2;
            if !self.idec_macroblocks() {
                return false;
            }
            self.set_ctrl_flag(CTRL_SCD, false);
            self.cbp = self.dec.coded_block_pattern;
        }
        if start <= 3 && !self.start_code() {
            self.cmd.pos[0] = 3;
            return false;
        }
        if !self.load_top() {
            self.cmd.pos[0] = 4;
            return false;
        }
        true
    }

    /// IDEC's macroblock loop: true at the end of the slice, false when
    /// waiting. Resumes from `pos[1]`, the blocks from `pos[2]`.
    fn idec_macroblocks(&mut self) -> bool {
        loop {
            if !self.out_ready() && self.cmd.pos[1] <= 2 {
                self.waiting_out = true;
                return false;
            }
            let s = self.cmd.pos[1];
            if s == 0 {
                self.dec.macroblock_modes = self.macroblock_modes();
                if self.dec.macroblock_modes & MACROBLOCK_QUANT != 0 {
                    let code = self.bs.getbits(5);
                    self.dec.quantizer_scale = if self.dec.q_scale_type {
                        NON_LINEAR_QUANTIZER_SCALE[code as usize]
                    } else {
                        (code << 1) as i32
                    };
                }
                self.dec.coded_block_pattern = 0x3F;
                self.dec.mb8 = [0; MB8_BYTES];
                self.dec.rgb32 = [0; 1024];
            }
            if s <= 1 {
                self.cmd.pos[1] = 1;
                if !self.intra_blocks(2) {
                    return false;
                }
                color::csc(
                    &self.dec.mb8,
                    &mut self.dec.rgb32,
                    self.thresh,
                    self.dec.sgn,
                );
                if self.dec.ofm {
                    color::dither(&self.dec.rgb32, &mut self.dec.rgb16, self.dec.dte);
                    self.dec.set_output(Output::Rgb16);
                } else {
                    self.dec.set_output(Output::Rgb32);
                }
                self.cmd.pos[1] = 2;
            }
            if s <= 2 {
                if self.ready_idec {
                    self.ready_idec = false;
                    self.delay();
                    self.cmd.pos[1] = 2;
                    return false;
                }
                let read = self.write_output();
                if self.dec.out_left != 0 {
                    self.waiting_out = true;
                    self.cmd.pos[1] = 2;
                    return false;
                }
                self.mba_count = 0;
                if read > 0 {
                    self.waiting_out = true;
                    self.cmd.pos[1] = 3;
                    return false;
                }
            }
            if s <= 3 {
                self.ready_idec = true;
                let mba = loop {
                    if !self.bs.fill(16) {
                        self.cmd.pos[1] = 3;
                        return false;
                    }
                    let code = self.bs.ubits(16);
                    if code >= 0x1000 {
                        break MBA_5[(self.bs.ubits(5) - 2) as usize];
                    }
                    if code >= 0x0300 {
                        break MBA_11[(self.bs.ubits(11) - 24) as usize];
                    }
                    match self.bs.ubits(11) {
                        8 => {
                            // macroblock_escape
                            self.mba_count += 33;
                            self.bs.advance(11);
                        }
                        // macroblock_stuffing
                        15 => self.bs.advance(11),
                        // End of the slice.
                        _ => return true,
                    }
                };
                self.bs.advance(mba.len.into());
                self.mba_count += i32::from(mba.mba);
                if self.mba_count != 0 {
                    self.dec.reset_dc_pred();
                }
            }
            if !self.bs.fill(16) {
                self.cmd.pos[1] = 4;
                return false;
            }
            self.cmd.pos[1] = 0;
            self.cmd.pos[2] = 0;
        }
    }

    /// BDEC: decodes one macroblock to 16-bit samples (`mpeg2_slice`).
    /// Resumes from `pos[0]`, the blocks from `pos[1]`.
    pub(crate) fn bdec(&mut self) -> bool {
        let start = self.cmd.pos[0];
        if start == 0 {
            if self.dec.dcr {
                self.dec.reset_dc_pred();
            }
            self.set_ctrl_flag(CTRL_ECD, false);
            self.regs[TOP] = 0;
            self.dec.mb8 = [0; MB8_BYTES];
            self.dec.mb16 = [0; MB8_BYTES];
        }
        if start <= 1 && !self.bs.fill(32) {
            self.cmd.pos[0] = 1;
            return false;
        }
        if start <= 2 {
            self.cmd.pos[0] = 2;
            if !self.out_ready() {
                self.waiting_out = true;
                return false;
            }
            if self.dec.macroblock_modes & MACROBLOCK_INTRA != 0 {
                if self.cmd.pos[1] == 0 {
                    self.dec.coded_block_pattern = 0x3F;
                }
                if !self.intra_blocks(1) {
                    return false;
                }
                // Widened without sign extension.
                for (d, &s) in self.dec.mb16.iter_mut().zip(&self.dec.mb8) {
                    *d = i16::from(s);
                }
            } else if self.dec.macroblock_modes & MACROBLOCK_PATTERN != 0
                && !self.non_intra_blocks()
            {
                return false;
            }
            self.set_ctrl_flag(CTRL_SCD, false);
            self.cbp = self.dec.coded_block_pattern;
            self.dec.set_output(Output::Mb16);
        }
        if start <= 3 {
            if self.ready_bdec {
                self.cmd.pos[0] = 3;
                self.ready_bdec = false;
                self.delay();
                return false;
            }
            let read = self.write_output();
            if self.dec.out_left != 0 {
                self.waiting_out = true;
                self.cmd.pos[0] = 3;
                return false;
            }
            self.mba_count = 0;
            if read > 0 {
                self.waiting_out = true;
                self.cmd.pos[0] = 4;
                return false;
            }
        }
        if start <= 4 && !self.start_code() {
            self.cmd.pos[0] = 4;
            return false;
        }
        if !self.load_top() {
            self.cmd.pos[0] = 5;
            return false;
        }
        self.ready_bdec = true;
        true
    }

    /// VDEC: decodes one variable-length code into `IPU_CMD`'s data, the
    /// table chosen by bits 26-27 (`ipuVDEC`).
    pub(crate) fn vdec(&mut self) -> bool {
        if self.cmd.pos[0] == 0 {
            if !self.bs.fill(32) {
                return false;
            }
            let data = match (self.cmd.current >> 26) & 3 {
                0 => {
                    // Macroblock address increment.
                    self.dec.mpeg1 = self.ctrl_flag(CTRL_MP1);
                    self.macroblock_address_increment()
                }
                1 => {
                    // Macroblock type; a picture type of 0 is taken as I.
                    self.dec.frame_pred_frame_dct = true;
                    let pct = self.ctrl_pct();
                    self.dec.coding_type = if pct > 0 { pct } else { I_TYPE };
                    self.macroblock_modes()
                }
                2 => self.motion_code(),
                _ => {
                    // Dual-prime vector.
                    let tab = DMV_2[self.bs.ubits(2) as usize];
                    self.bs.advance(tab.len.into());
                    i32::from(tab.dmv) | (i32::from(tab.len) << 16)
                }
            };
            self.regs[CMD_DATA] = data as u32;
            self.set_ctrl_flag(CTRL_ECD, data == 0);
        }
        if !self.load_top() {
            self.cmd.pos[0] = 1;
            return false;
        }
        true
    }

    /// A motion code, its length at bit 16.
    fn motion_code(&mut self) -> i32 {
        let code = self.bs.ubits(16);
        if code & 0x8000 != 0 {
            self.bs.advance(1);
            return 0x0001_0000;
        }
        let tab = if code & 0xF000 != 0 || code & 0xFC00 == 0x0C00 {
            MV_4[self.bs.ubits(4) as usize]
        } else {
            MV_10[self.bs.ubits(10) as usize]
        };
        let delta = i32::from(tab.delta) + 1;
        self.bs.advance(tab.len.into());
        let sign = self.bs.sbits(1);
        self.bs.advance(1);
        ((delta ^ sign) - sign) | (i32::from(tab.len) << 16)
    }

    /// FDEC: the next 32 bits into `IPU_CMD`'s data and `IPU_TOP`, not
    /// removed (`ipuFDEC`).
    pub(crate) fn fdec(&mut self) -> bool {
        let Some(v) = self.bs.peek32() else {
            return false;
        };
        self.regs[CMD_DATA] = v;
        self.regs[TOP] = v;
        true
    }
}
