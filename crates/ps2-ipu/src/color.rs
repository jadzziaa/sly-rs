//! Colour conversion: YCbCr to RGB32 (`yuv2rgb.cpp`'s
//! `yuv2rgb_reference`), the alpha thresholds and sign offset (`ipu_csc`
//! in `IPU_MultiISA.cpp`), RGB32 to RGB16 with optional dithering
//! (`IPUdither.cpp`'s `ipu_dither_reference`) and RGB16 to 4-bit indices
//! through the VQ colour table (`ipu_vq`).
//!
//! PCSX2 runs SSE2 or NEON versions of the first and third on most hosts;
//! these are its reference versions, which give the same results (the
//! SIMD versions saturate where the reference clamps, on values that
//! cannot leave the range).

/// A macroblock's samples: 16x16 Y, then 8x8 Cb, then 8x8 Cr (PCSX2's
/// `macroblock_8`), 384 bytes.
pub(crate) const MB8_BYTES: usize = 384;
/// Offset of Cb in a [`MB8_BYTES`] macroblock.
pub(crate) const CB: usize = 256;
/// Offset of Cr.
pub(crate) const CR: usize = 320;

const Y_BIAS: i32 = 16;
const Y_COEFF: i32 = 0x95; // 1.1640625
const GCR_COEFF: i32 = -0x68; // -0.8125
const GCB_COEFF: i32 = -0x32; // -0.390625
const RCR_COEFF: i32 = 0xCC; // 1.59375
const BCB_COEFF: i32 = 0x102; // 2.015625

/// Converts a macroblock of YCbCr to 16x16 RGBA bytes, alpha 0x80.
pub(crate) fn yuv2rgb(mb8: &[u8; MB8_BYTES], rgb32: &mut [u8; 1024]) {
    for y in 0..16 {
        for x in 0..16 {
            let c = (y >> 1) * 8 + (x >> 1);
            let cb = i32::from(mb8[CB + c]) - 128;
            let cr = i32::from(mb8[CR + c]) - 128;
            let lum = (Y_COEFF * (i32::from(mb8[y * 16 + x]) - Y_BIAS).max(0)) >> 6;
            let rcr = (RCR_COEFF * cr) >> 6;
            let gcr = (GCR_COEFF * cr) >> 6;
            let gcb = (GCB_COEFF * cb) >> 6;
            let bcb = (BCB_COEFF * cb) >> 6;
            let p = &mut rgb32[(y * 16 + x) * 4..][..4];
            p[0] = ((lum + rcr + 1) >> 1).clamp(0, 255) as u8;
            p[1] = ((lum + gcr + gcb + 1) >> 1).clamp(0, 255) as u8;
            p[2] = ((lum + bcb + 1) >> 1).clamp(0, 255) as u8;
            p[3] = 0x80;
        }
    }
}

/// `ipu_csc`: converts, then applies the thresholds (TH0: pixels with all
/// of R, G and B below it become 0, alpha included; TH1: below it, alpha
/// 0x40) and, with `sgn`, flips the colour bytes' top bits.
///
/// PCSX2 walks one pointer through the threshold pass and the sign pass
/// without resetting it, so with either threshold set its sign pass lands
/// on the memory after the RGB32 buffer instead. The RGB32 result is
/// what is kept here: no sign offset when a threshold is set. (PCSX2's
/// stray writes also hit its decoder state; that is not reproduced.)
pub(crate) fn csc(mb8: &[u8; MB8_BYTES], rgb32: &mut [u8; 1024], thresh: [u16; 2], sgn: bool) {
    yuv2rgb(mb8, rgb32);
    let below = |p: &[u8; 4], th: u16| p[..3].iter().all(|&c| u16::from(c) < th);
    if thresh[0] > 0 {
        for p in rgb32.as_chunks_mut::<4>().0 {
            if below(p, thresh[0]) {
                p.fill(0);
            } else if below(p, thresh[1]) {
                p[3] = 0x40;
            }
        }
    } else if thresh[1] > 0 {
        for p in rgb32.as_chunks_mut::<4>().0 {
            if below(p, thresh[1]) {
                p[3] = 0x40;
            }
        }
    } else if sgn {
        for p in rgb32.as_chunks_mut::<4>().0 {
            for c in &mut p[..3] {
                *c ^= 0x80;
            }
        }
    }
}

/// The 4x4 ordered dither added before truncating to 5 bits.
const DITHER: [[i32; 4]; 4] = [
    [-4, 0, -3, 1],
    [2, -2, 3, -1],
    [-3, 1, -4, 0],
    [3, -1, 2, -2],
];

/// RGB32 to RGB16 (5:5:5, alpha bit set where alpha was 0x40), dithered
/// with `dte` (`ipu_dither`).
pub(crate) fn dither(rgb32: &[u8; 1024], rgb16: &mut [u16; 256], dte: bool) {
    for i in 0..16 {
        for j in 0..16 {
            let p = &rgb32[(i * 16 + j) * 4..][..4];
            let d = if dte { DITHER[i & 3][j & 3] } else { 0 };
            let c = |v: u8| ((i32::from(v) + d).clamp(0, 255) >> 3) as u16;
            rgb16[i * 16 + j] =
                c(p[0]) | (c(p[1]) << 5) | (c(p[2]) << 10) | (u16::from(p[3] == 0x40) << 15);
        }
    }
}

/// RGB16 to 4-bit indices of the nearest VQ colour table entries, two
/// pixels a byte, the left one in the low nibble (`ipu_vq`). The first of
/// equally near entries wins.
pub(crate) fn vq(rgb16: &[u16; 256], clut: &[u16; 16], indx4: &mut [u8; 128]) {
    let parts = |c: u16| {
        [
            i32::from(c & 0x1F),
            i32::from((c >> 5) & 0x1F),
            i32::from((c >> 10) & 0x1F),
        ]
    };
    let closest = |px: u16| {
        let p = parts(px);
        let mut index = 0u8;
        let mut min = i32::MAX;
        for (k, &entry) in clut.iter().enumerate() {
            let e = parts(entry);
            let dist: i32 = (0..3).map(|n| (p[n] - e[n]) * (p[n] - e[n])).sum();
            if min > dist {
                index = k as u8;
                min = dist;
            }
        }
        index
    };
    for i in 0..16 {
        for j in 0..8 {
            indx4[i * 8 + j] =
                (closest(rgb16[i * 16 + 2 * j + 1]) << 4) | closest(rgb16[i * 16 + 2 * j]);
        }
    }
}
