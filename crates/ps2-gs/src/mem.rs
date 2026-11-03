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

/// Where a pixel lives: a word of local memory and the bits of it the
/// format holds (`mask` at `shift`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pixel {
    word: usize,
    shift: u32,
    mask: u32,
}

/// The address of pixel (`x`, `y`) of a `psm` buffer at `bp` with width
/// `bw`.
/// The address from the block and column tables directly: the reference
/// [`place`]'s page tables are built from.
fn place_slow(psm: Psm, bp: u32, bw: u32, x: u32, y: u32) -> Pixel {
    // Page width and height as powers of two.
    let (w_shift, h_shift, pages_per_row) = match psm {
        Psm::Ct16 | Psm::Ct16s | Psm::Z16 | Psm::Z16s => (6, 6, bw),
        Psm::T8 => (7, 6, bw >> 1),
        Psm::T4 => (7, 7, bw >> 1),
        _ => (6, 5, bw),
    };
    // As the GS (and PCSX2) does: x past the width runs into the next row's
    // pages, and 8- and 4-bit buffers have half as many pages per row.
    let page = (y >> h_shift) * pages_per_row + (x >> w_shift);
    let (px, py) = (x & ((1 << w_shift) - 1), y & ((1 << h_shift) - 1));
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
    let byte = block as usize * 256 + offset;
    let (shift, mask) = match psm {
        Psm::Ct32 | Psm::Z32 => (0, u32::MAX),
        Psm::Ct24 | Psm::Z24 => (0, 0x00FF_FFFF),
        Psm::Ct16 | Psm::Ct16s | Psm::Z16 | Psm::Z16s => (8 * (byte as u32 & 2), 0xFFFF),
        Psm::T8 => (8 * (byte as u32 & 3), 0xFF),
        Psm::T4 => (8 * (byte as u32 & 3) + 4 * u32::from(high), 0xF),
        Psm::T8h => (24, 0xFF),
        Psm::T4hl => (24, 0xF),
        Psm::T4hh => (28, 0xF),
    };
    Pixel {
        word: byte / 4,
        shift,
        mask,
    }
}

/// Offsets within a page, by position in the page (row by row): bytes for
/// 32- and 16-bit formats and PSMT8, nibbles for PSMT4.
struct PageTables {
    ct32: Vec<u16>,
    z32: Vec<u16>,
    ct16: Vec<u16>,
    ct16s: Vec<u16>,
    z16: Vec<u16>,
    z16s: Vec<u16>,
    t8: Vec<u16>,
    t4: Vec<u16>,
}

fn page_tables() -> &'static PageTables {
    static TABLES: std::sync::OnceLock<PageTables> = std::sync::OnceLock::new();
    TABLES.get_or_init(|| {
        // A page at block 0 with one page per row: the offset is the
        // address itself.
        let table = |psm: Psm, w: u32, h: u32, nibbles: bool| -> Vec<u16> {
            let bw = if matches!(psm, Psm::T8 | Psm::T4) {
                2
            } else {
                1
            };
            (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|(x, y)| {
                    let p = place_slow(psm, 0, bw, x, y);
                    let byte = p.word * 4 + (p.shift / 8) as usize;
                    let at = if nibbles {
                        byte * 2 + ((p.shift / 4) & 1) as usize
                    } else {
                        byte
                    };
                    at as u16
                })
                .collect()
        };
        PageTables {
            ct32: table(Psm::Ct32, 64, 32, false),
            z32: table(Psm::Z32, 64, 32, false),
            ct16: table(Psm::Ct16, 64, 64, false),
            ct16s: table(Psm::Ct16s, 64, 64, false),
            z16: table(Psm::Z16, 64, 64, false),
            z16s: table(Psm::Z16s, 64, 64, false),
            t8: table(Psm::T8, 128, 64, false),
            t4: table(Psm::T4, 128, 128, true),
        }
    })
}

/// The address of pixel (`x`, `y`) of a `psm` buffer at `bp` with width
/// `bw`: its page's base, then the page table's offset.
#[inline]
fn place(psm: Psm, bp: u32, bw: u32, x: u32, y: u32) -> Pixel {
    let t = page_tables();
    let (table, w_shift, h_shift, pages_per_row): (&[u16], u32, u32, u32) = match psm {
        Psm::Ct32 | Psm::Ct24 | Psm::T8h | Psm::T4hl | Psm::T4hh => (&t.ct32, 6, 5, bw),
        Psm::Z32 | Psm::Z24 => (&t.z32, 6, 5, bw),
        Psm::Ct16 => (&t.ct16, 6, 6, bw),
        Psm::Ct16s => (&t.ct16s, 6, 6, bw),
        Psm::Z16 => (&t.z16, 6, 6, bw),
        Psm::Z16s => (&t.z16s, 6, 6, bw),
        Psm::T8 => (&t.t8, 7, 6, bw >> 1),
        Psm::T4 => (&t.t4, 7, 7, bw >> 1),
    };
    let page = (y >> h_shift) * pages_per_row + (x >> w_shift);
    let (px, py) = (x & ((1 << w_shift) - 1), y & ((1 << h_shift) - 1));
    let offset = u32::from(table[((py << w_shift) | px) as usize]);
    let base = (bp + page * 32) * 256;
    let (byte, nibble) = if psm == Psm::T4 {
        (offset >> 1, offset & 1)
    } else {
        (offset, 0)
    };
    let byte = ((base + byte) & 0x3F_FFFF) as usize;
    let (shift, mask) = match psm {
        Psm::Ct32 | Psm::Z32 => (0, u32::MAX),
        Psm::Ct24 | Psm::Z24 => (0, 0x00FF_FFFF),
        Psm::Ct16 | Psm::Ct16s | Psm::Z16 | Psm::Z16s => (8 * (byte as u32 & 2), 0xFFFF),
        Psm::T8 => (8 * (byte as u32 & 3), 0xFF),
        Psm::T4 => (8 * (byte as u32 & 3) + 4 * nibble, 0xF),
        Psm::T8h => (24, 0xFF),
        Psm::T4hl => (24, 0xF),
        Psm::T4hh => (28, 0xF),
    };
    Pixel {
        word: byte / 4,
        shift,
        mask,
    }
}

/// GS local memory, as 32-bit words.
pub struct Memory {
    words: Box<[u32]>,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            words: vec![0; SIZE / 4].into_boxed_slice(),
        }
    }
}

impl Memory {
    /// Where pixel (`x`, `y`) of the `psm` buffer at `bp` with width `bw`
    /// lives, for [`Memory::get`] and [`Memory::set`].
    #[inline]
    #[must_use]
    pub fn locate(psm: Psm, bp: u32, bw: u32, x: u32, y: u32) -> Pixel {
        place(psm, bp, bw, x, y)
    }

    /// The pixel at `p`, as the format stores it (indices for T formats,
    /// RGB5A1 for 16-bit colour).
    #[inline]
    #[must_use]
    pub fn get(&self, p: Pixel) -> u32 {
        (self.words[p.word] >> p.shift) & p.mask
    }

    /// Writes the pixel at `p`; bits the format does not hold are kept.
    #[inline]
    pub fn set(&mut self, p: Pixel, value: u32) {
        let w = &mut self.words[p.word];
        *w = (*w & !(p.mask << p.shift)) | ((value & p.mask) << p.shift);
    }

    /// Pixel (`x`, `y`) of the `psm` buffer at `bp` with width `bw`, as the
    /// format stores it.
    #[inline]
    #[must_use]
    pub fn read(&self, psm: Psm, bp: u32, bw: u32, x: u32, y: u32) -> u32 {
        self.get(place(psm, bp, bw, x, y))
    }

    /// Writes pixel (`x`, `y`); bits the format does not hold are kept.
    #[inline]
    pub fn write(&mut self, psm: Psm, bp: u32, bw: u32, x: u32, y: u32, value: u32) {
        self.set(place(psm, bp, bw, x, y), value);
    }

    /// Replaces the memory with `bytes`, little-endian (from dumps).
    pub fn load_bytes(&mut self, bytes: &[u8]) {
        for (w, b) in self.words.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *w = u32::from_le_bytes(*b);
        }
    }

    /// The memory as bytes, little-endian (for dumps).
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.words.iter().flat_map(|w| w.to_le_bytes()).collect()
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
    fn page_tables_agree_with_the_block_and_column_tables() {
        for psm in ALL {
            for (bp, bw) in [(0, 1), (0x40, 2), (0x3FE0, 10), (0x123, 4)] {
                for y in (0..300).step_by(7) {
                    for x in (0..700).step_by(3) {
                        assert_eq!(
                            place(psm, bp, bw, x, y),
                            place_slow(psm, bp, bw, x, y),
                            "{psm:?} bp {bp:#x} bw {bw} at {x},{y}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn addresses_follow_the_tables() {
        // PSMCT32: pixel (2, 0) is the 5th of block 0; (8, 0) starts block 1;
        // (0, 8) starts block 2; (0, 32) is the next page row.
        let at = |x, y| place(Psm::Ct32, 0, 1, x, y).word * 4;
        assert_eq!(at(2, 0), 16);
        assert_eq!(at(8, 0), 256);
        assert_eq!(at(0, 8), 512);
        assert_eq!(at(0, 32), 8192);
        // Depth swaps block groups: Z32's first pixel is in block 24.
        assert_eq!(place(Psm::Z32, 0, 1, 0, 0).word * 4, 24 * 256);
        // PSMT4: two pixels per byte; pixel 1 is the low nibble of byte 4.
        assert_eq!(
            place(Psm::T4, 0, 2, 1, 0),
            Pixel {
                word: 1,
                shift: 0,
                mask: 0xF
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
