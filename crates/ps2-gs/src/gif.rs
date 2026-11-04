//! GIF packets: a 128-bit tag, then its data in one of three formats.
//!
//! - PACKED: one quadword per register descriptor, each register with its
//!   own layout (RGBAQ takes Q from the last ST; XYZ2 with ADC set is
//!   XYZ3), or A+D: a value and the register's address.
//! - REGLIST: 64-bit register values, two to a quadword, the last half of an
//!   odd count unused.
//! - IMAGE: transfer data for the transfer under way.
//!
//! Each path keeps its own tag state, so a packet may continue in a later
//! transfer.

use crate::Gs;
use crate::regs::{addr, bits};

/// A path's state: the tag being worked through.
#[derive(Clone, Copy, Debug, Default)]
pub struct Path {
    tag: Option<Tag>,
}

#[derive(Clone, Copy, Debug)]
struct Tag {
    /// Loops left.
    nloop: u32,
    nreg: u32,
    regs: u64,
    flg: u32,
    /// The descriptor next in this loop.
    reg: u32,
    /// The Q that PACKED ST latches for RGBAQ.
    q: u32,
}

impl Gs {
    /// Feeds quadwords of GIF packets on `path` (1 to 3).
    pub fn gif(&mut self, path: usize, data: impl IntoIterator<Item = u128>) {
        let path = path.min(3);
        if self.dumping() {
            let data: Vec<u128> = data.into_iter().collect();
            self.dump_transfer(path, &data);
            self.feed(path, data);
        } else {
            self.feed(path, data);
        }
    }

    fn feed(&mut self, path: usize, data: impl IntoIterator<Item = u128>) {
        // The path's tag in a local while its quadwords go by (nothing
        // else reads it), stored back at the end.
        let mut current = self.paths[path].tag.take();
        // IMAGE data, gathered until its tag ends (or the data does).
        let mut image = Vec::new();
        for q in data {
            let Some(tag) = current.as_mut() else {
                current = self.gif_tag(q);
                continue;
            };
            match tag.flg {
                0 => {
                    let desc = ((tag.regs >> (4 * tag.reg)) & 0xF) as u8;
                    self.packed(desc, q, &mut tag.q);
                    tag.reg += 1;
                }
                1 => {
                    // Loops end per half here, so `reg` never reaches
                    // `nreg` below.
                    for half in [q as u64, (q >> 64) as u64] {
                        let desc = ((tag.regs >> (4 * tag.reg)) & 0xF) as u8;
                        if desc < 0xE {
                            self.write(desc, half);
                        }
                        tag.reg += 1;
                        if tag.reg == tag.nreg {
                            tag.reg = 0;
                            tag.nloop -= 1;
                            if tag.nloop == 0 {
                                // The rest of the quadword is padding.
                                break;
                            }
                        }
                    }
                }
                _ => {
                    image.extend_from_slice(&q.to_le_bytes());
                    tag.reg = tag.nreg;
                }
            }
            if tag.reg == tag.nreg {
                tag.reg = 0;
                tag.nloop -= 1;
            }
            if tag.nloop == 0 {
                current = None;
                if !image.is_empty() {
                    self.image(&image);
                    image.clear();
                }
            }
        }
        if !image.is_empty() {
            self.image(&image);
        }
        self.paths[path].tag = current;
    }

    /// A GIF tag: its PRIM if it sets one, and the loops it starts (none
    /// for NLOOP 0).
    fn gif_tag(&mut self, q: u128) -> Option<Tag> {
        let lo = q as u64;
        let nloop = bits(lo, 0, 15);
        let flg = bits(lo, 58, 2);
        if flg == 0 && bits(lo, 46, 1) != 0 {
            self.write(addr::PRIM, u64::from(bits(lo, 47, 11)));
        }
        let nreg = match bits(lo, 60, 4) {
            0 => 16,
            n => n,
        };
        (nloop > 0).then_some(Tag {
            nloop,
            // IMAGE takes one quadword per loop.
            nreg: if flg >= 2 { 1 } else { nreg },
            regs: (q >> 64) as u64,
            flg: flg.min(2),
            reg: 0,
            q: 1.0f32.to_bits(),
        })
    }

    /// A PACKED quadword for descriptor `desc`. The registers a vertex
    /// takes are set here directly, as [`Gs::write`] sets them.
    fn packed(&mut self, desc: u8, q: u128, latched_q: &mut u32) {
        let w = |k: u32| (q >> (32 * k)) as u32;
        let lo = q as u64;
        match desc {
            0x0 => self.write(addr::PRIM, lo & 0x7FF),
            0x1 => {
                let rgba = (w(0) & 0xFF)
                    | ((w(1) & 0xFF) << 8)
                    | ((w(2) & 0xFF) << 16)
                    | ((w(3) & 0xFF) << 24);
                self.rgbaq = u64::from(rgba) | (u64::from(*latched_q) << 32);
            }
            0x2 => {
                *latched_q = w(2);
                self.st = u64::from(w(0)) | (u64::from(w(1)) << 32);
            }
            0x3 => self.uv = u64::from(w(0) & 0x3FFF) | (u64::from(w(1) & 0x3FFF) << 16),
            0x4 | 0xC => {
                let adc = desc == 0xC || (w(3) >> 15) & 1 != 0;
                let (x, y) = (w(0) & 0xFFFF, w(1) & 0xFFFF);
                let (z, f) = ((w(2) >> 4) & 0x00FF_FFFF, ((w(3) >> 4) & 0xFF) as u8);
                self.vertex(x, y, z, Some(f), !adc);
            }
            0x5 | 0xD => {
                let adc = desc == 0xD || (w(3) >> 15) & 1 != 0;
                self.vertex(w(0) & 0xFFFF, w(1) & 0xFFFF, w(2), None, !adc);
            }
            0x6..=0x9 => self.write(desc, lo),
            0xA => self.write(addr::FOG, u64::from((w(3) >> 4) & 0xFF) << 56),
            0xE => self.write((q >> 64) as u8, lo),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Gs;

    fn tag(nloop: u32, eop: bool, pre: Option<u32>, flg: u32, regs: &[u8]) -> u128 {
        let mut lo = u64::from(nloop) | (u64::from(eop) << 15) | (u64::from(flg) << 58);
        if let Some(prim) = pre {
            lo |= (1 << 46) | (u64::from(prim) << 47);
        }
        lo |= (regs.len() as u64 & 0xF) << 60;
        let hi = regs
            .iter()
            .enumerate()
            .fold(0u64, |h, (i, &r)| h | (u64::from(r) << (4 * i)));
        u128::from(lo) | (u128::from(hi) << 64)
    }

    fn ad(reg: u8, value: u64) -> u128 {
        u128::from(value) | (u128::from(reg) << 64)
    }

    #[test]
    fn packed_sets_registers_and_latches_q() {
        let mut gs = Gs::default();
        let st = u128::from(2.0f32.to_bits())
            | (u128::from(3.0f32.to_bits()) << 32)
            | (u128::from(0.5f32.to_bits()) << 64);
        let rgba = 0x10 | (0x20 << 32) | (0x30 << 64) | (0x40 << 96);
        gs.gif(
            3,
            [
                tag(1, true, Some(6), 0, &[0x2, 0x1, 0xE]),
                st,
                rgba,
                ad(0x3B, 0x80_0000_0080),
            ],
        );
        assert_eq!(gs.prim, 6);
        assert_eq!(
            gs.rgbaq,
            0x40_30_20_10 | (u64::from(0.5f32.to_bits()) << 32)
        );
        assert_eq!(
            gs.st,
            u64::from(2.0f32.to_bits()) | (u64::from(3.0f32.to_bits()) << 32)
        );
        assert_eq!(gs.texa, 0x80_0000_0080);
        assert!(gs.paths[3].tag.is_none());
    }

    #[test]
    fn reglist_takes_halves_and_skips_padding() {
        let mut gs = Gs::default();
        // Two loops of (RGBAQ, UV, TEXA via 0xE = ignored): six halves,
        // three quadwords; then a PACKED A+D to show the next tag is read.
        gs.gif(
            2,
            [
                tag(2, false, None, 1, &[0x1, 0x3, 0xE]),
                1 | (2u128 << 64),
                3u128 << 64,
                5,
                tag(1, true, None, 0, &[0xE]),
                ad(0x46, 0),
            ],
        );
        assert_eq!(gs.rgbaq, 3);
        assert_eq!(gs.uv, 5);
        assert_eq!(gs.colclamp, 0);
    }

    #[test]
    fn odd_reglist_ends_mid_quadword() {
        let mut gs = Gs::default();
        gs.gif(
            3,
            [
                tag(1, false, None, 1, &[0x1]),
                7 | (99u128 << 64),
                tag(1, true, None, 0, &[0xE]),
                ad(0x49, 1),
            ],
        );
        assert_eq!(gs.rgbaq, 7);
        assert_eq!(gs.pabe, 1);
    }
}
