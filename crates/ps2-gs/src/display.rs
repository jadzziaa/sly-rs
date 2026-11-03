//! Display read-out: the two read circuits (DISPFB and DISPLAY), merged as
//! PMODE says over BGCOLOR, give a picture as RGB.
//!
//! The picture is the read circuits' rectangles at framebuffer resolution
//! (DW and DH divided by the magnifications); in field mode (SMODE2 FFMD)
//! each field reads half the lines.

use crate::regs::bits;
use crate::{Gs, Psm};

/// The privileged registers the read-out uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Privileged {
    /// PMODE.
    pub pmode: u64,
    /// SMODE2.
    pub smode2: u64,
    /// DISPFB1 and DISPFB2.
    pub dispfb: [u64; 2],
    /// DISPLAY1 and DISPLAY2.
    pub display: [u64; 2],
    /// BGCOLOR.
    pub bgcolor: u64,
}

/// A picture, RGB, row by row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Three bytes per pixel.
    pub rgb: Vec<u8>,
}

/// One read circuit's placement.
struct Circuit {
    psm: Psm,
    fbp: u32,
    fbw: u32,
    dbx: u32,
    dby: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Circuit {
    fn new(dispfb: u64, display: u64, field_mode: bool) -> Self {
        let mag_x = bits(display, 23, 4) + 1;
        let mag_y = bits(display, 27, 2) + 1;
        let lines = |v: u32| if field_mode { v / 2 } else { v };
        Self {
            psm: Psm::from_bits(u64::from(bits(dispfb, 15, 5))).unwrap_or(Psm::Ct32),
            fbp: bits(dispfb, 0, 9) * 32,
            fbw: bits(dispfb, 9, 6),
            dbx: bits(dispfb, 32, 11),
            dby: bits(dispfb, 43, 11),
            x: bits(display, 0, 12) / mag_x,
            y: lines(bits(display, 12, 11) / mag_y),
            w: (bits(display, 32, 12) + 1) / mag_x,
            h: lines((bits(display, 44, 11) + 1) / mag_y),
        }
    }
}

impl Gs {
    /// The picture the display circuits show.
    #[must_use]
    pub fn display(&self, regs: &Privileged) -> Frame {
        let pmode = regs.pmode;
        let field_mode = bits(regs.smode2, 0, 2) == 3;
        let circuits: Vec<(usize, Circuit)> = (0..2)
            .filter(|&i| bits(pmode, i as u32, 1) != 0)
            .map(|i| (i, Circuit::new(regs.dispfb[i], regs.display[i], field_mode)))
            .collect();
        let Some(x0) = circuits.iter().map(|(_, c)| c.x).min() else {
            return Frame::default();
        };
        let y0 = circuits.iter().map(|(_, c)| c.y).min().unwrap_or(0);
        let width = circuits
            .iter()
            .map(|(_, c)| c.x - x0 + c.w)
            .max()
            .unwrap_or(0);
        let height = circuits
            .iter()
            .map(|(_, c)| c.y - y0 + c.h)
            .max()
            .unwrap_or(0);
        let bg = [
            bits(regs.bgcolor, 0, 8),
            bits(regs.bgcolor, 8, 8),
            bits(regs.bgcolor, 16, 8),
        ];
        let (mmod, slbg, alp) = (
            bits(pmode, 5, 1) != 0,
            bits(pmode, 7, 1) != 0,
            bits(pmode, 8, 8),
        );
        let read = |c: &Circuit, x: u32, y: u32| -> Option<[u32; 4]> {
            let (cx, cy) = (x.checked_sub(c.x - x0)?, y.checked_sub(c.y - y0)?);
            if cx >= c.w || cy >= c.h {
                return None;
            }
            let (fx, fy) = ((c.dbx + cx) & 0x7FF, (c.dby + cy) & 0x7FF);
            let v = self.mem.read(c.psm, c.fbp, c.fbw, fx, fy);
            Some(match c.psm {
                Psm::Ct16 | Psm::Ct16s => [
                    (v & 0x1F) << 3,
                    ((v >> 5) & 0x1F) << 3,
                    ((v >> 10) & 0x1F) << 3,
                    if v & 0x8000 != 0 { 0x80 } else { 0 },
                ],
                _ => [v & 0xFF, (v >> 8) & 0xFF, (v >> 16) & 0xFF, v >> 24],
            })
        };
        let first = circuits.iter().find(|(i, _)| *i == 0).map(|(_, c)| c);
        let second = circuits.iter().find(|(i, _)| *i == 1).map(|(_, c)| c);
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height {
            for x in 0..width {
                let back = match second {
                    Some(c) if !slbg => read(c, x, y).map_or(bg, |p| [p[0], p[1], p[2]]),
                    _ => bg,
                };
                let out = match first.and_then(|c| read(c, x, y)) {
                    Some(p) => {
                        let a = if mmod { alp } else { (p[3] * 2).min(255) };
                        std::array::from_fn(|k| (p[k] * a + back[k] * (255 - a)) / 255)
                    }
                    None => back,
                };
                rgb.extend(out.map(|v| v as u8));
            }
        }
        Frame { width, height, rgb }
    }
}

impl Frame {
    /// The picture as a PNG file's bytes (8-bit RGB, stored blocks; no
    /// compression, no dependencies).
    #[must_use]
    pub fn png(&self) -> Vec<u8> {
        let mut raw = Vec::with_capacity(self.rgb.len() + self.height as usize);
        for row in self.rgb.chunks_exact((self.width * 3).max(1) as usize) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&self.width.to_be_bytes());
        ihdr.extend_from_slice(&self.height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut out, *b"IHDR", &ihdr);
        chunk(&mut out, *b"IDAT", &zlib_stored(&raw));
        chunk(&mut out, *b"IEND", &[]);
        out
    }
}

fn chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Data as a zlib stream of stored (uncompressed) blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut blocks = data.chunks(0xFFFF).peekable();
    if blocks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(block) = blocks.next() {
        out.push(u8::from(blocks.peek().is_none()));
        let n = block.len() as u16;
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&(!n).to_le_bytes());
        out.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65_521;
        b = (b + a) % 65_521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn reads_out_circuit_one_over_the_background() {
        let mut gs = Gs::default();
        gs.mem.write(Psm::Ct32, 0, 1, 1, 0, 0x80_33_22_11);
        let regs = Privileged {
            pmode: 1,
            dispfb: [1 << 9, 0],
            // 4 x 2 pixels, no magnification.
            display: [(3 << 32) | (1 << 44), 0],
            bgcolor: 0x99,
            ..Privileged::default()
        };
        let f = gs.display(&regs);
        assert_eq!((f.width, f.height), (4, 2));
        assert_eq!(&f.rgb[3..6], &[0x11, 0x22, 0x33]);
        // Alpha 0 shows the background.
        assert_eq!(&f.rgb[0..3], &[0x99, 0, 0]);
        assert!(f.png().starts_with(b"\x89PNG"));
    }
}
