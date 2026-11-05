//! The inverse DCT and the coefficient scan orders (`IDCT_Block`,
//! `IDCT_Copy`, `IDCT_Add` and `mpeg2_scan` in `IPU_MultiISA.cpp`, from
//! libmpeg2's C IDCT). Integer arithmetic as PCSX2's, wrapping where its
//! 32-bit `int`s would.

const W1: i32 = 2841; // 2048 * sqrt(2) * cos(1 * pi / 16)
const W2: i32 = 2676; // 2048 * sqrt(2) * cos(2 * pi / 16)
const W3: i32 = 2408; // 2048 * sqrt(2) * cos(3 * pi / 16)
const W5: i32 = 1609; // 2048 * sqrt(2) * cos(5 * pi / 16)
const W6: i32 = 1108; // 2048 * sqrt(2) * cos(6 * pi / 16)
const W7: i32 = 565; // 2048 * sqrt(2) * cos(7 * pi / 16)

/// The zig-zag scan: coefficient order to block position.
const SCAN_NORM: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// The alternate scan.
const SCAN_ALT: [u8; 64] = [
    0, 8, 16, 24, 1, 9, 2, 10, 17, 25, 32, 40, 48, 56, 57, 49, 41, 33, 26, 18, 3, 11, 4, 12, 19,
    27, 34, 42, 50, 58, 35, 43, 51, 59, 20, 28, 5, 13, 6, 14, 21, 29, 36, 44, 52, 60, 37, 45, 53,
    61, 22, 30, 7, 15, 23, 31, 38, 46, 54, 62, 39, 47, 55, 63,
];

/// PCSX2 permutes the scans to suit the IDCT's layout (`make_scan_pack`):
/// it swaps the bits of a position as `((j & 0x36) >> 1) | ((j & 0x09) << 2)`.
const fn permute(scan: [u8; 64]) -> [u8; 64] {
    let mut out = [0; 64];
    let mut i = 0;
    while i < 64 {
        let j = scan[i];
        out[i] = ((j & 0x36) >> 1) | ((j & 0x09) << 2);
        i += 1;
    }
    out
}

/// The zig-zag scan as the decoder uses it.
pub(crate) const MPEG2_SCAN_NORM: [u8; 64] = permute(SCAN_NORM);
/// The alternate scan as the decoder uses it.
pub(crate) const MPEG2_SCAN_ALT: [u8; 64] = permute(SCAN_ALT);

/// `t0 = w0 * d0 + w1 * d1` and `t1 = w0 * d1 - w1 * d0`, computed as
/// PCSX2's `BUTTERFLY` does.
fn butterfly(w0: i32, w1: i32, d0: i32, d1: i32) -> (i32, i32) {
    let tmp = w0.wrapping_mul(d0.wrapping_add(d1));
    (
        tmp.wrapping_add((w1 - w0).wrapping_mul(d1)),
        tmp.wrapping_sub((w1 + w0).wrapping_mul(d0)),
    )
}

/// The 8x8 inverse DCT, in place: rows, then columns (`IDCT_Block`).
pub(crate) fn idct_block(block: &mut [i16; 64]) {
    for row in block.as_chunks_mut::<8>().0 {
        if row[1..].iter().all(|&c| c == 0) {
            let dc = (i32::from(row[0]) << 3) as i16;
            row.fill(dc);
            continue;
        }
        let r: [i32; 8] = std::array::from_fn(|k| i32::from(row[k]));

        let d0 = (r[0] << 11) + 128;
        let d2 = r[2] << 11;
        let t0 = d0 + d2;
        let t1 = d0 - d2;
        let (t2, t3) = butterfly(W6, W2, r[3], r[1]);
        let a0 = t0.wrapping_add(t2);
        let a1 = t1.wrapping_add(t3);
        let a2 = t1.wrapping_sub(t3);
        let a3 = t0.wrapping_sub(t2);

        let (mut t0, mut t1) = butterfly(W7, W1, r[7], r[4]);
        let (t2, t3) = butterfly(W3, W5, r[5], r[6]);
        let b0 = t0.wrapping_add(t2);
        let b3 = t1.wrapping_add(t3);
        t0 = t0.wrapping_sub(t2);
        t1 = t1.wrapping_sub(t3);
        let b1 = t0.wrapping_add(t1).wrapping_mul(181) >> 8;
        let b2 = t0.wrapping_sub(t1).wrapping_mul(181) >> 8;

        row[0] = (a0.wrapping_add(b0) >> 8) as i16;
        row[1] = (a1.wrapping_add(b1) >> 8) as i16;
        row[2] = (a2.wrapping_add(b2) >> 8) as i16;
        row[3] = (a3.wrapping_add(b3) >> 8) as i16;
        row[4] = (a3.wrapping_sub(b3) >> 8) as i16;
        row[5] = (a2.wrapping_sub(b2) >> 8) as i16;
        row[6] = (a1.wrapping_sub(b1) >> 8) as i16;
        row[7] = (a0.wrapping_sub(b0) >> 8) as i16;
    }

    for col in 0..8 {
        let c: [i32; 8] = std::array::from_fn(|k| i32::from(block[col + 8 * k]));

        let d0 = (c[0] << 11) + 65536;
        let d2 = c[2] << 11;
        let t0 = d0 + d2;
        let t1 = d0 - d2;
        let (t2, t3) = butterfly(W6, W2, c[3], c[1]);
        let a0 = t0.wrapping_add(t2);
        let a1 = t1.wrapping_add(t3);
        let a2 = t1.wrapping_sub(t3);
        let a3 = t0.wrapping_sub(t2);

        let (mut t0, mut t1) = butterfly(W7, W1, c[7], c[4]);
        let (t2, t3) = butterfly(W3, W5, c[5], c[6]);
        let b0 = t0.wrapping_add(t2);
        let b3 = t1.wrapping_add(t3);
        t0 = t0.wrapping_sub(t2) >> 8;
        t1 = t1.wrapping_sub(t3) >> 8;
        let b1 = t0.wrapping_add(t1).wrapping_mul(181);
        let b2 = t0.wrapping_sub(t1).wrapping_mul(181);

        let out = [
            a0.wrapping_add(b0),
            a1.wrapping_add(b1),
            a2.wrapping_add(b2),
            a3.wrapping_add(b3),
            a3.wrapping_sub(b3),
            a2.wrapping_sub(b2),
            a1.wrapping_sub(b1),
            a0.wrapping_sub(b0),
        ];
        for (k, v) in out.into_iter().enumerate() {
            block[col + 8 * k] = (v >> 17) as i16;
        }
    }
}

/// Transforms `block` and stores it clipped to bytes at `dest[at..]`, rows
/// `stride` bytes apart; leaves `block` zeroed (`IDCT_Copy`).
///
/// PCSX2 clips through a table covering -384 to 639; corrupt streams can
/// index it outside that range, reading neighbouring memory. Here every
/// value is clamped.
pub(crate) fn idct_copy(block: &mut [i16; 64], dest: &mut [u8], at: usize, stride: usize) {
    idct_block(block);
    for (i, row) in block.as_chunks_mut::<8>().0.iter_mut().enumerate() {
        let d = &mut dest[at + i * stride..at + i * stride + 8];
        for (o, &v) in d.iter_mut().zip(row.iter()) {
            *o = v.clamp(0, 255) as u8;
        }
        row.fill(0);
    }
}

/// Transforms `block` and stores it at `dest[at..]`, rows `stride` apart;
/// leaves `block` zeroed (`IDCT_Add`, which despite its name stores).
///
/// PCSX2's shortcut for a block with only a DC term needs a `last` of 129,
/// which its decoder never passes, so it is left out.
pub(crate) fn idct_add(block: &mut [i16; 64], dest: &mut [i16], at: usize, stride: usize) {
    idct_block(block);
    for (i, row) in block.as_chunks_mut::<8>().0.iter_mut().enumerate() {
        dest[at + i * stride..at + i * stride + 8].copy_from_slice(row);
        row.fill(0);
    }
}
