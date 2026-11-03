//! GS local memory: 4 MiB addressed through pixel storage formats.
//!
//! A buffer starts at a block pointer (`bp`, 256-byte blocks) and has a width
//! in 64-pixel units (`bw`). Memory is organised in 8 KiB pages of 32
//! blocks; each format has its page and block size and its own block and
//! pixel orders (PCSX2's tables, in `tables.rs`). Every access goes through
//! [`Memory::read`] and [`Memory::write`].

use crate::tables::{
    BLOCK4, BLOCK8, BLOCK16, BLOCK16S, BLOCK32, COLUMN4, COLUMN8, COLUMN16, COLUMN32,
};

/// Local memory size: 4 MiB.
pub const SIZE: usize = 4 << 20;

/// A pixel storage format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Psm {
    /// 32-bit colour.
    Ct32,
    /// 24-bit colour (in 32-bit storage).
    Ct24,
    /// 16-bit colour (RGB5A1).
    Ct16,
    /// 16-bit colour, alternative block order.
    Ct16s,
    /// 8-bit indices.
    T8,
    /// 4-bit indices.
    T4,
    /// 8-bit indices in the top byte of 32-bit pixels.
    T8h,
    /// 4-bit indices in bits 24..27 of 32-bit pixels.
    T4hl,
    /// 4-bit indices in bits 28..31 of 32-bit pixels.
    T4hh,
    /// 32-bit depth.
    Z32,
    /// 24-bit depth.
    Z24,
    /// 16-bit depth.
    Z16,
    /// 16-bit depth, alternative block order.
    Z16s,
}

impl Psm {
    /// The format a register's PSM field names, if it is one.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Option<Self> {
        Some(match bits & 0x3F {
            0x00 => Self::Ct32,
            0x01 => Self::Ct24,
            0x02 => Self::Ct16,
            0x0A => Self::Ct16s,
            0x13 => Self::T8,
            0x14 => Self::T4,
            0x1B => Self::T8h,
            0x24 => Self::T4hl,
            0x2C => Self::T4hh,
            0x30 => Self::Z32,
            0x31 => Self::Z24,
            0x32 => Self::Z16,
            0x3A => Self::Z16s,
            _ => return None,
        })
    }

    /// Bits a pixel takes in local memory's own layout (PSMT8H, 4HL and
    /// 4HH live in 32-bit words; transfers move them as 8 or 4 bits).
    #[must_use]
    pub const fn bits(self) -> u32 {
        match self {
            Self::Ct32 | Self::Z32 | Self::T8h | Self::T4hl | Self::T4hh => 32,
            Self::Ct24 | Self::Z24 => 24,
            Self::Ct16 | Self::Ct16s | Self::Z16 | Self::Z16s => 16,
            Self::T8 => 8,
            Self::T4 => 4,
        }
    }

    /// Whether it is a depth format.
    #[must_use]
    pub const fn is_depth(self) -> bool {
        matches!(self, Self::Z32 | Self::Z24 | Self::Z16 | Self::Z16s)
    }
}

/// Where a pixel lives: the byte address and, for 4-bit formats, whether it
/// is the high nibble.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Place {
    byte: usize,
    high: bool,
}

/// The address of pixel (`x`, `y`) of a `psm` buffer at `bp` with width
/// `bw`.
fn place(psm: Psm, bp: u32, bw: u32, x: u32, y: u32) -> Place {
    let (page_w, page_h, pages_per_row) = match psm {
        Psm::Ct16 | Psm::Ct16s | Psm::Z16 | Psm::Z16s => (64, 64, bw),
        Psm::T8 => (128, 64, bw >> 1),
        Psm::T4 => (128, 128, bw >> 1),
        _ => (64, 32, bw),
    };
    // As the GS (and PCSX2) does: x past the width runs into the next row's
    // pages, and 8- and 4-bit buffers have half as many pages per row.
    let page = (y / page_h) * pages_per_row + x / page_w;
    let (px, py) = (x % page_w, y % page_h);
    let z = if psm.is_depth() { 0x18 } else { 0 };
    let (block, offset, high) = match psm {
        Psm::Ct16 | Psm::Z16 => {
            let b = BLOCK16[(py / 8) as usize][(px / 16) as usize];
            (
                b,
                2 * usize::from(COLUMN16[(py % 8) as usize][(px % 16) as usize]),
                false,
            )
        }
        Psm::Ct16s | Psm::Z16s => {
            let b = BLOCK16S[(py / 8) as usize][(px / 16) as usize];
            (
                b,
                2 * usize::from(COLUMN16[(py % 8) as usize][(px % 16) as usize]),
                false,
            )
        }
        Psm::T8 => {
            let b = BLOCK8[(py / 16) as usize][(px / 16) as usize];
            (
                b,
                usize::from(COLUMN8[(py % 16) as usize][(px % 16) as usize]),
                false,
            )
        }
        Psm::T4 => {
            let b = BLOCK4[(py / 16) as usize][(px / 32) as usize];
            let n = usize::from(COLUMN4[(py % 16) as usize][(px % 32) as usize]);
            (b, n / 2, n % 2 == 1)
        }
        _ => {
            let b = BLOCK32[(py / 8) as usize][(px / 8) as usize];
            (
                b,
                4 * usize::from(COLUMN32[(py % 8) as usize][(px % 8) as usize]),
                false,
            )
        }
    };
    let block = (bp + page * 32 + u32::from(block ^ z)) & 0x3FFF;
    Place {
        byte: block as usize * 256 + offset,
        high,
    }
}

/// GS local memory.
pub struct Memory {
    data: Box<[u8]>,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            data: vec![0; SIZE].into_boxed_slice(),
        }
    }
}

impl Memory {
    fn u32_at(&self, byte: usize) -> u32 {
        let b = &self.data[byte..byte + 4];
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }

    fn set_u32(&mut self, byte: usize, value: u32) {
        self.data[byte..byte + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// Pixel (`x`, `y`) of the `psm` buffer at `bp` with width `bw`, as the
    /// format stores it (indices for T formats, RGB5A1 for 16-bit colour).
    #[must_use]
    pub fn read(&self, psm: Psm, bp: u32, bw: u32, x: u32, y: u32) -> u32 {
        let p = place(psm, bp, bw, x, y);
        match psm {
            Psm::Ct32 | Psm::Z32 => self.u32_at(p.byte),
            Psm::Ct24 | Psm::Z24 => self.u32_at(p.byte) & 0x00FF_FFFF,
            Psm::Ct16 | Psm::Ct16s | Psm::Z16 | Psm::Z16s => u32::from(u16::from_le_bytes([
                self.data[p.byte],
                self.data[p.byte + 1],
            ])),
            Psm::T8 => u32::from(self.data[p.byte]),
            Psm::T4 => u32::from(if p.high {
                self.data[p.byte] >> 4
            } else {
                self.data[p.byte] & 0xF
            }),
            Psm::T8h => self.u32_at(p.byte) >> 24,
            Psm::T4hl => (self.u32_at(p.byte) >> 24) & 0xF,
            Psm::T4hh => self.u32_at(p.byte) >> 28,
        }
    }

    /// Writes pixel (`x`, `y`); bits the format does not hold are kept.
    pub fn write(&mut self, psm: Psm, bp: u32, bw: u32, x: u32, y: u32, value: u32) {
        let p = place(psm, bp, bw, x, y);
        match psm {
            Psm::Ct32 | Psm::Z32 => self.set_u32(p.byte, value),
            Psm::Ct24 | Psm::Z24 => {
                let old = self.u32_at(p.byte);
                self.set_u32(p.byte, (old & 0xFF00_0000) | (value & 0x00FF_FFFF));
            }
            Psm::Ct16 | Psm::Ct16s | Psm::Z16 | Psm::Z16s => {
                self.data[p.byte..p.byte + 2].copy_from_slice(&(value as u16).to_le_bytes());
            }
            Psm::T8 => self.data[p.byte] = value as u8,
            Psm::T4 => {
                let b = &mut self.data[p.byte];
                *b = if p.high {
                    (*b & 0x0F) | ((value as u8 & 0xF) << 4)
                } else {
                    (*b & 0xF0) | (value as u8 & 0xF)
                };
            }
            Psm::T8h => {
                let old = self.u32_at(p.byte);
                self.set_u32(p.byte, (old & 0x00FF_FFFF) | (value << 24));
            }
            Psm::T4hl => {
                let old = self.u32_at(p.byte);
                self.set_u32(p.byte, (old & !0x0F00_0000) | ((value & 0xF) << 24));
            }
            Psm::T4hh => {
                let old = self.u32_at(p.byte);
                self.set_u32(p.byte, (old & !0xF000_0000) | ((value & 0xF) << 28));
            }
        }
    }

    /// The raw bytes (for tests and dumps).
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Psm; 13] = [
        Psm::Ct32,
        Psm::Ct24,
        Psm::Ct16,
        Psm::Ct16s,
        Psm::T8,
        Psm::T4,
        Psm::T8h,
        Psm::T4hl,
        Psm::T4hh,
        Psm::Z32,
        Psm::Z24,
        Psm::Z16,
        Psm::Z16s,
    ];

    fn mask(psm: Psm) -> u32 {
        match psm.bits() {
            32 if matches!(psm, Psm::T8h) => 0xFF,
            32 if matches!(psm, Psm::T4hl | Psm::T4hh) => 0xF,
            32 => u32::MAX,
            24 => 0x00FF_FFFF,
            16 => 0xFFFF,
            8 => 0xFF,
            _ => 0xF,
        }
    }

    #[test]
    fn every_format_round_trips_without_collisions() {
        for psm in ALL {
            let mut mem = Memory::default();
            let (bw, w, h) = (4, 256, 128);
            let value = |x: u32, y: u32| {
                (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) & mask(psm)
            };
            for y in 0..h {
                for x in 0..w {
                    mem.write(psm, 0, bw, x, y, value(x, y));
                }
            }
            for y in 0..h {
                for x in 0..w {
                    assert_eq!(
                        mem.read(psm, 0, bw, x, y),
                        value(x, y),
                        "{psm:?} at {x},{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn addresses_follow_the_tables() {
        // PSMCT32: pixel (2, 0) is the 5th of block 0; (8, 0) starts block 1;
        // (0, 8) starts block 2; (0, 32) is the next page row.
        let at = |x, y| place(Psm::Ct32, 0, 1, x, y).byte;
        assert_eq!(at(2, 0), 16);
        assert_eq!(at(8, 0), 256);
        assert_eq!(at(0, 8), 512);
        assert_eq!(at(0, 32), 8192);
        // Depth swaps block groups: Z32's first pixel is in block 24.
        assert_eq!(place(Psm::Z32, 0, 1, 0, 0).byte, 24 * 256);
        // PSMT4: two pixels per byte.
        assert_eq!(
            place(Psm::T4, 0, 2, 1, 0),
            Place {
                byte: 4,
                high: false
            }
        );
    }

    #[test]
    fn formats_sharing_words_keep_each_others_bits() {
        let mut mem = Memory::default();
        mem.write(Psm::Ct24, 0, 1, 3, 3, 0x00AB_CDEF);
        mem.write(Psm::T8h, 0, 1, 3, 3, 0x12);
        mem.write(Psm::T4hl, 0, 1, 3, 3, 0x7);
        assert_eq!(mem.read(Psm::Ct32, 0, 1, 3, 3), 0x17AB_CDEF);
        assert_eq!(mem.read(Psm::T4hh, 0, 1, 3, 3), 0x1);
    }
}
