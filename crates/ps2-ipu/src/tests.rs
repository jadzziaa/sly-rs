use crate::color::{self, MB8_BYTES};
use crate::idct::{MPEG2_SCAN_NORM, idct_block};
use crate::{IPU_BP, IPU_CMD, IPU_CTRL, IPU_TOP, Ipu};

/// Builds a bitstream most significant bit first.
#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    nbits: usize,
}

impl Bits {
    /// Appends a string of '0' and '1' (other characters are skipped).
    fn s(mut self, code: &str) -> Self {
        for c in code.chars().filter(|c| *c == '0' || *c == '1') {
            if self.nbits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if c == '1' {
                *self.bytes.last_mut().unwrap() |= 0x80 >> (self.nbits % 8);
            }
            self.nbits += 1;
        }
        self
    }

    /// Appends `value` in `n` bits.
    fn v(self, value: u32, n: usize) -> Self {
        let s: String = (0..n)
            .rev()
            .map(|k| {
                if k < 32 && value >> k & 1 == 1 {
                    '1'
                } else {
                    '0'
                }
            })
            .collect();
        self.s(&s)
    }

    /// Pads with zero bits to a byte boundary, then appends bytes.
    fn bytes(mut self, b: &[u8]) -> Self {
        self.nbits = self.bytes.len() * 8;
        self.bytes.extend_from_slice(b);
        self.nbits += 8 * b.len();
        self
    }

    /// The stream as quadwords, zero-padded.
    fn qws(&self) -> Vec<u128> {
        qws(&self.bytes)
    }
}

/// Bytes as quadwords in EE memory order, zero-padded.
fn qws(bytes: &[u8]) -> Vec<u128> {
    bytes
        .chunks(16)
        .map(|c| {
            let mut b = [0u8; 16];
            b[..c.len()].copy_from_slice(c);
            u128::from_le_bytes(b)
        })
        .collect()
}

/// Quadwords back to bytes.
fn bytes_of(qws: &[u128]) -> Vec<u8> {
    qws.iter().flat_map(|q| q.to_le_bytes()).collect()
}

fn cmd(code: u32, option: u32) -> u64 {
    u64::from((code << 28) | option)
}

/// Runs the command to the end, feeding `input` as room allows and
/// draining the output; returns the output.
fn drive(ipu: &mut Ipu, input: &[u128]) -> Vec<u128> {
    let mut input = input.to_vec();
    let mut out = Vec::new();
    for _ in 0..10_000 {
        let n = ipu.in_free().min(input.len());
        ipu.push_in(&input[..n]);
        input.drain(..n);
        ipu.run();
        out.extend(ipu.pop_out(8));
        if !ipu.busy() {
            return out;
        }
    }
    panic!("the command did not finish");
}

/// An IPU with its input FIFO cleared and the bit pointer at `bp`.
fn ipu_at(bp: u32) -> Ipu {
    let mut ipu = Ipu::new();
    ipu.write(IPU_CMD, cmd(0, bp));
    assert!(ipu.take_irq());
    ipu
}

// --- Variable-length codes -------------------------------------------------

/// VDEC with table `tbl` on `bits`; returns `IPU_CMD`'s data.
fn vdec(tbl: u32, bits: &Bits, ctrl: Option<u32>) -> u32 {
    let mut ipu = ipu_at(0);
    if let Some(c) = ctrl {
        ipu.write(IPU_CTRL, u64::from(c));
    }
    ipu.push_in(&bits.qws());
    ipu.write(IPU_CMD, cmd(3, tbl << 26));
    assert!(!ipu.busy(), "VDEC runs at once with data there");
    assert!(ipu.take_irq());
    ipu.read(IPU_CMD) as u32
}

#[test]
fn vdec_macroblock_address_increment() {
    let b = Bits::default;
    // Table B-1: '1' is 1, '011' is 2, '0000 0011 000' is 33; the result
    // carries the code length at bit 16.
    assert_eq!(vdec(0, &b().s("1"), None), 0x1_0001);
    assert_eq!(vdec(0, &b().s("011"), None), 0x3_0002);
    assert_eq!(vdec(0, &b().s("0010"), None), 0x4_0005);
    assert_eq!(vdec(0, &b().s("00000011000"), None), 0xB_0021);
    // macroblock_escape.
    assert_eq!(vdec(0, &b().s("00000001000"), None), 0xB_0023);
    // macroblock_stuffing is only taken in MPEG-1 (IPU_CTRL's MP1).
    assert_eq!(vdec(0, &b().s("00000001111"), None), 0);
    assert_eq!(vdec(0, &b().s("00000001111"), Some(1 << 23)), 0xB_0022);
}

#[test]
fn vdec_macroblock_type() {
    let b = Bits::default;
    // No picture type: taken as I. '1' is intra, '01' intra with quant.
    assert_eq!(vdec(1, &b().s("1"), None), 1);
    assert_eq!(vdec(1, &b().s("01"), None), 17);
    // P: '1' is motion forward and coded; frame prediction adds MC_FRAME.
    let p = Some(2 << 24);
    assert_eq!(vdec(1, &b().s("1"), p), 0x8 | 0x2 | 0x80);
    assert_eq!(vdec(1, &b().s("00011"), p), 1);
    // B: '10' is both directions, not coded, with its length at bit 16.
    let bp = Some(3 << 24);
    assert_eq!(vdec(1, &b().s("10"), bp), 0xC | 0x80 | (2 << 16));
    assert_eq!(vdec(1, &b().s("00011"), bp), 0x1 | 0x80 | (5 << 16));
}

#[test]
fn vdec_motion_code_and_dmv() {
    let b = Bits::default;
    assert_eq!(vdec(2, &b().s("1"), None), 0x1_0000);
    // '01' then the sign: +1 and -1, length 2 at bit 16.
    assert_eq!(vdec(2, &b().s("010"), None), 0x2_0001);
    assert_eq!(vdec(2, &b().s("011"), None), 0xFFFF_FFFF);
    // '0000 011' then the sign: 7; '0000 0011 00' then the sign: 16.
    assert_eq!(vdec(2, &b().s("0000011 0"), None), 0x7_0007);
    assert_eq!(vdec(2, &b().s("0000001100 0"), None), 0xA_0010);
    assert_eq!(vdec(3, &b().s("0"), None), 0x1_0000);
    assert_eq!(vdec(3, &b().s("10"), None), 0x2_0001);
    assert_eq!(vdec(3, &b().s("11"), None), 0xFFFF_FFFF);
}

#[test]
fn vdec_sets_ecd_on_zero_and_top() {
    let mut ipu = ipu_at(0);
    ipu.push_in(&Bits::default().s("000000000000").bytes(&[0xAB]).qws());
    ipu.write(IPU_CMD, cmd(3, 0));
    assert_eq!(ipu.read(IPU_CMD) as u32, 0);
    assert_ne!(ipu.read(IPU_CTRL) as u32 & (1 << 14), 0, "ECD");
    // Nothing was removed for an invalid code: TOP shows the stream.
    assert_eq!(ipu.read(IPU_TOP) as u32, 0x0000_AB00);
}

#[test]
fn dct_coefficient_codes() {
    let run_level_len = |code: usize, alt: bool, first: bool| {
        let t = Ipu::dct_code(code, alt, first);
        (t.run, t.level, t.len)
    };
    // Table B-14: '1s' first, '11s' after it, '10' end of block.
    assert_eq!(run_level_len(0x8000, false, true), (0, 1, 1));
    assert_eq!(run_level_len(0xC000, false, false), (0, 1, 2));
    assert_eq!(run_level_len(0x8000, false, false), (64, 0, 2));
    // '011s': run 1 level 1; '0000 01': escape.
    assert_eq!(run_level_len(0x6000, false, false), (1, 1, 3));
    assert_eq!(run_level_len(0x0400, false, false), (65, 0, 6));
    // '0000 0000 0001 0000' at 16 bits: run 1, level 18.
    assert_eq!(run_level_len(0x0010, false, false), (1, 18, 16));
    // Table B-15: '0110' ends the block, '10' is run 0 level 1.
    assert_eq!(run_level_len(0x6000, true, false), (64, 0, 4));
    assert_eq!(run_level_len(0x8000, true, false), (0, 1, 2));
}

// --- IDCT --------------------------------------------------------------------

/// PCSX2's `IDCT_Block`, written out again in 64-bit arithmetic as a check
/// on the port (no overflow for these inputs, so the results agree).
fn reference_idct(block: &[i16; 64]) -> [i16; 64] {
    let (w1, w2, w3, w5, w6, w7) = (2841i64, 2676, 2408, 1609, 1108, 565);
    let bf = |w0: i64, w1: i64, d0: i64, d1: i64| (w0 * d0 + w1 * d1, w0 * d1 - w1 * d0);
    let mut b: Vec<i64> = block.iter().map(|&v| i64::from(v)).collect();
    for r in 0..8 {
        let x = &mut b[8 * r..8 * r + 8];
        if x[1..].iter().all(|&v| v == 0) {
            let dc = i64::from((x[0] << 3) as i16);
            x.fill(dc);
            continue;
        }
        let (t0, t1) = (
            (x[0] << 11) + 128 + (x[2] << 11),
            (x[0] << 11) + 128 - (x[2] << 11),
        );
        let (t2, t3) = bf(w6, w2, x[3], x[1]);
        let a = [t0 + t2, t1 + t3, t1 - t3, t0 - t2];
        let (t0, t1) = bf(w7, w1, x[7], x[4]);
        let (t2, t3) = bf(w3, w5, x[5], x[6]);
        let (u0, u1) = (t0 - t2, t1 - t3);
        let bb = [
            t0 + t2,
            ((u0 + u1) * 181) >> 8,
            ((u0 - u1) * 181) >> 8,
            t1 + t3,
        ];
        for k in 0..4 {
            x[k] = i64::from(((a[k] + bb[k]) >> 8) as i16);
            x[7 - k] = i64::from(((a[k] - bb[k]) >> 8) as i16);
        }
    }
    let mut out = [0i16; 64];
    for c in 0..8 {
        let x: Vec<i64> = (0..8).map(|k| b[c + 8 * k]).collect();
        let (t0, t1) = (
            (x[0] << 11) + 65536 + (x[2] << 11),
            (x[0] << 11) + 65536 - (x[2] << 11),
        );
        let (t2, t3) = bf(w6, w2, x[3], x[1]);
        let a = [t0 + t2, t1 + t3, t1 - t3, t0 - t2];
        let (t0, t1) = bf(w7, w1, x[7], x[4]);
        let (t2, t3) = bf(w3, w5, x[5], x[6]);
        let (u0, u1) = ((t0 - t2) >> 8, (t1 - t3) >> 8);
        let bb = [t0 + t2, (u0 + u1) * 181, (u0 - u1) * 181, t1 + t3];
        for k in 0..4 {
            out[c + 8 * k] = ((a[k] + bb[k]) >> 17) as i16;
            out[c + 8 * (7 - k)] = ((a[k] - bb[k]) >> 17) as i16;
        }
    }
    out
}

/// A floating-point 8x8 IDCT of natural-order coefficients.
fn float_idct(coef: &[f64; 64]) -> [f64; 64] {
    let c = |u: u8| {
        if u == 0 {
            std::f64::consts::FRAC_1_SQRT_2
        } else {
            1.0
        }
    };
    let mut out = [0.0; 64];
    let basis =
        |p: u8, f: u8| (f64::from(2 * p + 1) * f64::from(f) * std::f64::consts::PI / 16.0).cos();
    for y in 0..8u8 {
        for x in 0..8u8 {
            let mut s = 0.0;
            for v in 0..8u8 {
                for u in 0..8u8 {
                    s += c(u) * c(v) * coef[usize::from(v * 8 + u)] * basis(x, u) * basis(y, v);
                }
            }
            out[usize::from(y * 8 + x)] = s / 4.0;
        }
    }
    out
}

/// PCSX2's block layout: natural position `j` is stored at this index.
fn permuted(j: usize) -> usize {
    ((j & 0x36) >> 1) | ((j & 0x09) << 2)
}

#[test]
fn idct_dc_only() {
    // A DC term d gives (8d + 4) >> 3 everywhere: the row pass replicates
    // d << 3, the column pass adds 65536 and shifts by 17.
    for d in [0i16, 100, -100, 255, -256, 5] {
        let mut b = [0i16; 64];
        b[0] = d * 8;
        idct_block(&mut b);
        assert!(b.iter().all(|&v| v == (d * 8 + 4) >> 3), "{d}: {b:?}");
    }
    // 4096 << 3 wraps to -32768 in the row pass, as in PCSX2's 16-bit
    // store: (-32768 << 11) + 65536 >> 17 is -512.
    let mut b = [0i16; 64];
    b[0] = 4096;
    idct_block(&mut b);
    assert!(b.iter().all(|&v| v == -512));
}

#[test]
fn idct_matches_reference_and_float() {
    let mut seed = 0x1234_5678u32;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for case in 0..200 {
        // Natural-order coefficients, a few of them non-zero.
        let mut natural = [0i16; 64];
        for _ in 0..=case % 12 {
            let j = (rand() % 64) as usize;
            natural[j] = (rand() % 512) as i16 - 256;
        }
        let mut block = [0i16; 64];
        for (j, &v) in natural.iter().enumerate() {
            block[permuted(j)] = v;
        }
        let expect = reference_idct(&block);
        let mut got = block;
        idct_block(&mut got);
        assert_eq!(got, expect, "case {case}");

        let coef: [f64; 64] = std::array::from_fn(|j| f64::from(natural[j]));
        let float = float_idct(&coef);
        for (k, (&g, &f)) in got.iter().zip(&float).enumerate() {
            assert!(
                (f64::from(g) - f).abs() <= 1.0,
                "case {case} at {k}: {g} vs {f}"
            );
        }
    }
}

#[test]
fn scan_tables_are_permuted_zigzag() {
    // The zig-zag scan starts 0, 1, 8, 16, 9 (natural positions).
    let natural = [0usize, 1, 8, 16, 9, 2];
    for (i, &j) in natural.iter().enumerate() {
        assert_eq!(usize::from(MPEG2_SCAN_NORM[i]), permuted(j));
    }
}

// --- Colour conversion ---------------------------------------------------

/// A macroblock with every Y, Cb and Cr sample the same.
fn flat_mb8(y: u8, cb: u8, cr: u8) -> [u8; MB8_BYTES] {
    let mut mb = [0u8; MB8_BYTES];
    mb[..256].fill(y);
    mb[256..320].fill(cb);
    mb[320..].fill(cr);
    mb
}

#[test]
fn csc_formula_flat_values() {
    let mut rgb = [0u8; 1024];
    // Y 128: lum = (0x95 * 112) >> 6 = 260; (260 + 1) >> 1 = 130.
    color::csc(&flat_mb8(128, 128, 128), &mut rgb, [0, 0], false);
    assert!(rgb.chunks(4).all(|p| p == [130, 130, 130, 0x80]));
    // Y 235 reaches 255.
    color::csc(&flat_mb8(235, 128, 128), &mut rgb, [0, 0], false);
    assert!(rgb.chunks(4).all(|p| p == [255, 255, 255, 0x80]));
    // Y 81, Cb 90, Cr 240: lum 151, R term 357, G terms -182 and 29, B term
    // -154 (arithmetic shifts round down): R 254, G and B clamp to 0.
    color::csc(&flat_mb8(81, 90, 240), &mut rgb, [0, 0], false);
    assert!(rgb.chunks(4).all(|p| p == [254, 0, 0, 0x80]));
    // The sign offset flips the colours' top bits.
    color::csc(&flat_mb8(128, 128, 128), &mut rgb, [0, 0], true);
    assert!(rgb.chunks(4).all(|p| p == [2, 2, 2, 0x80]));
    // Thresholds: below TH0 is all zero, below TH1 gets alpha 0x40, and
    // with a threshold set the sign offset is lost (as in PCSX2).
    color::csc(&flat_mb8(128, 128, 128), &mut rgb, [131, 0], true);
    assert!(rgb.iter().all(|&b| b == 0));
    color::csc(&flat_mb8(128, 128, 128), &mut rgb, [100, 131], true);
    assert!(rgb.chunks(4).all(|p| p == [130, 130, 130, 0x40]));
}

#[test]
fn dither_and_rgb16() {
    let mut rgb32 = [0u8; 1024];
    for p in rgb32.chunks_mut(4) {
        p.copy_from_slice(&[130, 130, 130, 0x80]);
    }
    let mut rgb16 = [0u16; 256];
    color::dither(&rgb32, &mut rgb16, false);
    // 130 >> 3 = 16 in each field.
    assert!(rgb16.iter().all(|&p| p == 0x4210));
    color::dither(&rgb32, &mut rgb16, true);
    // Row 0 dithers by -4, 0, -3, 1: 126 -> 15, 130 -> 16, 127 -> 15, 131 -> 16.
    assert_eq!(&rgb16[..4], &[0x3DEF, 0x4210, 0x3DEF, 0x4210]);
    // Row 1 by 2, -2, 3, -1: all 16.
    assert_eq!(&rgb16[16..20], &[0x4210; 4]);
    // Alpha 0x40 sets the top bit.
    rgb32[3] = 0x40;
    color::dither(&rgb32, &mut rgb16, false);
    assert_eq!(rgb16[0], 0xC210);
    assert_eq!(rgb16[1], 0x4210);
}

#[test]
fn csc_command_rgb32_and_rgb16() {
    let mb = flat_mb8(128, 128, 128);
    let input = qws(&mb);

    let mut ipu = ipu_at(0);
    ipu.write(IPU_CMD, cmd(7, 1));
    let out = drive(&mut ipu, &input);
    assert!(ipu.take_irq());
    assert_eq!(out.len(), 64);
    assert!(bytes_of(&out).chunks(4).all(|p| p == [130, 130, 130, 0x80]));

    // RGB16 with dither, two macroblocks.
    let mut ipu = ipu_at(0);
    ipu.write(IPU_CMD, cmd(7, (1 << 27) | (1 << 26) | 2));
    let mut two = input.clone();
    two.extend(&input);
    let out = drive(&mut ipu, &two);
    assert_eq!(out.len(), 64);
    let px: Vec<u16> = bytes_of(&out)
        .chunks(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    assert_eq!(&px[..4], &[0x3DEF, 0x4210, 0x3DEF, 0x4210]);
    assert_eq!(&px[256..260], &[0x3DEF, 0x4210, 0x3DEF, 0x4210]);
}

#[test]
fn setth_then_csc_alpha() {
    let mut ipu = ipu_at(0);
    // TH0 0, TH1 200: grey 130 gets alpha 0x40.
    ipu.write(IPU_CMD, cmd(9, 200 << 16));
    assert!(ipu.take_irq() && !ipu.busy());
    ipu.write(IPU_CMD, cmd(7, 1));
    let out = drive(&mut ipu, &qws(&flat_mb8(128, 128, 128)));
    assert!(bytes_of(&out).chunks(4).all(|p| p == [130, 130, 130, 0x40]));
}

#[test]
fn pack_rgb16_and_vq() {
    let mut rgb32 = vec![0u8; 1024];
    for (k, p) in rgb32.chunks_mut(4).enumerate() {
        // Left pixel of each pair red, right one blue.
        let c = if k % 2 == 0 {
            [255, 0, 0, 0x80]
        } else {
            [0, 0, 255, 0x80]
        };
        p.copy_from_slice(&c);
    }
    let input = qws(&rgb32);

    let mut ipu = ipu_at(0);
    ipu.write(IPU_CMD, cmd(8, (1 << 27) | 1));
    let out = drive(&mut ipu, &input);
    let px: Vec<u16> = bytes_of(&out)
        .chunks(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    assert_eq!(px.len(), 256);
    assert_eq!(&px[..2], &[0x001F, 0x7C00]);

    // A colour table with blue at 2 and red at 5 (and red again at 9: the
    // first match wins).
    let mut clut = [0u16; 16];
    clut[2] = 0x7C00;
    clut[5] = 0x001F;
    clut[9] = 0x001F;
    let clut_bytes: Vec<u8> = clut.iter().flat_map(|c| c.to_le_bytes()).collect();
    let mut ipu = ipu_at(0);
    ipu.write(IPU_CMD, cmd(6, 0));
    let _ = drive(&mut ipu, &qws(&clut_bytes));
    assert_eq!(ipu.vqclut, clut);
    ipu.write(IPU_CMD, cmd(8, 1));
    let out = drive(&mut ipu, &input);
    assert_eq!(out.len(), 8);
    assert!(bytes_of(&out).iter().all(|&b| b == 0x25));
}

// --- Macroblock decoding ---------------------------------------------------

/// One intra macroblock, DC terms only (the quantiser matrix is zero after
/// reset, so AC terms would vanish anyway): luma DC differences +10, -5, 0,
/// +1, Cb +20, Cr -3, each block ended by '10'. With 8-bit precision and
/// predictors from 128, the blocks come out flat 138, 133, 133, 134, Cb 148,
/// Cr 125.
fn intra_mb(b: Bits) -> Bits {
    b.s("110 1010 10") // size 4, +10
        .s("101 010 10") // size 3, -5
        .s("100 10") // size 0
        .s("00 1 10") // size 1, +1
        .s("11110 10100 10") // chroma size 5, +20
        .s("10 00 10") // chroma size 2, -3
}

/// The macroblock [`intra_mb`] decodes to, as Y rows, Cb and Cr samples.
fn intra_mb_samples() -> [u8; MB8_BYTES] {
    let mut mb = [0u8; MB8_BYTES];
    for y in 0..16 {
        for x in 0..16 {
            mb[y * 16 + x] = match (y < 8, x < 8) {
                (true, true) => 138,
                (true, false) | (false, true) => 133,
                (false, false) => 134,
            };
        }
    }
    mb[256..320].fill(148);
    mb[320..].fill(125);
    mb
}

/// BDEC of an intra macroblock: DCR set, 8-bit DC.
const BDEC_INTRA: u32 = (1 << 27) | (1 << 26);

fn bdec_stream() -> Bits {
    // After the macroblock: zero bits, then a start code and a sequence
    // header code.
    intra_mb(Bits::default()).bytes(&[0, 0, 1, 0xB3])
}

fn check_bdec_output(ipu: &mut Ipu, out: &[u128]) {
    assert_eq!(out.len(), 48);
    let samples: Vec<i16> = bytes_of(out)
        .chunks(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let expect: Vec<i16> = intra_mb_samples().iter().map(|&v| i16::from(v)).collect();
    assert_eq!(samples, expect);
    assert!(ipu.take_irq());
    let ctrl = ipu.read(IPU_CTRL) as u32;
    assert_ne!(ctrl & (1 << 15), 0, "SCD");
    assert_eq!(ctrl & (1 << 14), 0, "ECD");
    assert_eq!((ctrl >> 8) & 0x3F, 0x3F, "CBP");
    assert_eq!(ipu.read(IPU_TOP), 0x0000_01B3, "TOP not busy, start code");
}

#[test]
fn bdec_intra_macroblock() {
    let mut ipu = ipu_at(0);
    ipu.push_in(&bdec_stream().qws());
    ipu.write(IPU_CMD, cmd(2, BDEC_INTRA));
    assert!(ipu.busy());
    assert_eq!(ipu.read(IPU_TOP) >> 32, 0x8000_0000, "TOP busy");
    let out = drive(&mut ipu, &[]);
    check_bdec_output(&mut ipu, &out);
}

#[test]
fn bdec_stalls_for_input_and_resumes() {
    // Starting at bit 96 the macroblock runs past the first quadword: it
    // stops at the Cb block's DC term and resumes when the next arrives.
    let stream = Bits::default().v(0, 96);
    let stream = intra_mb(stream).bytes(&[0, 0, 1, 0xB3]);
    let input = stream.qws();
    assert_eq!(input.len(), 2);
    let mut ipu = ipu_at(96);
    ipu.write(IPU_CMD, cmd(2, BDEC_INTRA));
    assert!(ipu.run(), "starts, then waits at its first read");
    ipu.push_in(&input[..1]);
    assert!(ipu.run());
    assert!(ipu.busy());
    assert!(ipu.waiting_on_input());
    assert!(ipu.data_requested());
    assert_eq!(ipu.out_len(), 0);
    assert_eq!(ipu.cmd.pos[1], 3, "stopped at the third luma block");
    assert_eq!(ipu.read(IPU_BP) as u32, 0x0001_0071, "BP 113, FP 1");
    assert!(!ipu.run(), "no progress without data");
    let out = drive(&mut ipu, &input[1..]);
    check_bdec_output(&mut ipu, &out);
}

#[test]
fn bdec_waits_for_the_from_ipu_channel() {
    let mut ipu = ipu_at(0);
    ipu.push_in(&bdec_stream().qws());
    ipu.set_out_dma_ready(false);
    ipu.write(IPU_CMD, cmd(2, BDEC_INTRA));
    ipu.run();
    assert!(ipu.waiting_on_output());
    assert!(!ipu.run());
    ipu.set_out_dma_ready(true);
    let out = drive(&mut ipu, &[]);
    check_bdec_output(&mut ipu, &out);
}

#[test]
fn bdec_output_waits_for_room() {
    let mut ipu = ipu_at(0);
    ipu.push_in(&bdec_stream().qws());
    ipu.write(IPU_CMD, cmd(2, BDEC_INTRA));
    ipu.run();
    assert_eq!(ipu.out_len(), 8);
    assert!(ipu.waiting_on_output());
    assert!(!ipu.run(), "full output FIFO");
    let mut out = ipu.pop_out(3);
    assert!(ipu.run());
    assert_eq!(ipu.out_len(), 8);
    while ipu.busy() {
        out.extend(ipu.pop_out(8));
        ipu.run();
    }
    out.extend(ipu.pop_out(8));
    check_bdec_output(&mut ipu, &out);
}

/// The RGBA a flat YCbCr converts to, by the documented formula.
fn rgb_of(y: u8, cb: u8, cr: u8) -> [u8; 4] {
    let (y, cb, cr) = (i32::from(y), i32::from(cb) - 128, i32::from(cr) - 128);
    let lum = (0x95 * (y - 16).max(0)) >> 6;
    let c = |v: i32| ((v + 1) >> 1).clamp(0, 255) as u8;
    [
        c(lum + ((0xCC * cr) >> 6)),
        c(lum + ((-0x68 * cr) >> 6) + ((-0x32 * cb) >> 6)),
        c(lum + ((0x102 * cb) >> 6)),
        0x80,
    ]
}

#[test]
fn idec_two_macroblock_slice() {
    // Macroblock type '1' (intra), the macroblock, address increment '1',
    // then '1' and a macroblock of zero differences ('100 10' for luma,
    // '00 10' for chroma): its predictors carry on from the first. Then the
    // slice ends with zero bits up to a start code.
    let mut b = intra_mb(Bits::default().s("1")).s("1").s("1");
    for _ in 0..4 {
        b = b.s("100 10");
    }
    let b = b.s("00 10 00 10").bytes(&[0, 0, 1, 0x01]);

    let mut ipu = ipu_at(0);
    ipu.push_in(&b.qws());
    ipu.write(IPU_CMD, cmd(1, 0));
    assert_eq!(ipu.read(IPU_CTRL) as u32 >> 24 & 7, 1, "PCT set to I");
    let out = drive(&mut ipu, &[]);
    assert!(ipu.take_irq());
    assert_eq!(out.len(), 128);
    let bytes = bytes_of(&out);
    let first = intra_mb_samples();
    for (k, p) in bytes[..1024].chunks(4).enumerate() {
        let (y, x) = (k / 16, k % 16);
        let c = (y / 2) * 8 + x / 2;
        assert_eq!(p, rgb_of(first[k], first[256 + c], first[320 + c]), "{k}");
    }
    assert!(bytes[1024..].chunks(4).all(|p| p == rgb_of(134, 148, 125)));
    let ctrl = ipu.read(IPU_CTRL) as u32;
    assert_ne!(ctrl & (1 << 15), 0, "SCD");
    assert_eq!(ipu.read(IPU_TOP), 0x0000_0101);
}

#[test]
fn idec_rgb16_dithered() {
    let b = intra_mb(Bits::default().s("1")).bytes(&[0, 0, 1, 0x01]);
    let mut ipu = ipu_at(0);
    ipu.push_in(&b.qws());
    ipu.write(IPU_CMD, cmd(1, (1 << 27) | (1 << 26)));
    let out = drive(&mut ipu, &[]);
    assert_eq!(out.len(), 32);
    let mut rgb32 = [0u8; 1024];
    color::csc(&intra_mb_samples(), &mut rgb32, [0, 0], false);
    let mut rgb16 = [0u16; 256];
    color::dither(&rgb32, &mut rgb16, true);
    let got: Vec<u16> = bytes_of(&out)
        .chunks(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    assert_eq!(got, rgb16);
}

#[test]
fn setiq_loads_matrices_in_stream_order() {
    let iq: Vec<u8> = (1..=64).collect();
    let niq: Vec<u8> = (100..164).collect();
    let mut ipu = ipu_at(0);
    let input = qws(&iq);
    // Two quadwords arrive before the command, two after: it waits.
    ipu.push_in(&input[..2]);
    ipu.write(IPU_CMD, cmd(5, 0));
    assert!(ipu.busy() && ipu.waiting_on_input());
    ipu.push_in(&input[2..]);
    assert!(ipu.run());
    assert!(!ipu.busy() && ipu.take_irq());
    assert_eq!(ipu.dec.iq.to_vec(), iq);
    ipu.write(IPU_CMD, cmd(5, 1 << 27));
    let _ = drive(&mut ipu, &qws(&niq));
    assert_eq!(ipu.dec.niq.to_vec(), niq);
}

// --- Registers ---------------------------------------------------------------

#[test]
fn fdec_bp_and_cmd_readback() {
    let mut ipu = ipu_at(0);
    ipu.push_in(&qws(&[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]));
    // FDEC skipping 4 bits: the next 32 into IPU_CMD and IPU_TOP.
    ipu.write(IPU_CMD, cmd(4, 4));
    assert!(!ipu.busy() && ipu.take_irq());
    assert_eq!(ipu.read(IPU_CMD), 0x2345_6789);
    assert_eq!(ipu.read(IPU_TOP), 0x2345_6789);
    // BP 4, nothing left in the FIFO, one quadword buffered.
    assert_eq!(ipu.read(IPU_BP) as u32, 0x0001_0004);
    // A later command's IPU_CMD read shows the stream from BP.
    ipu.write(IPU_CMD, cmd(9, 0));
    assert_eq!(ipu.read(IPU_CMD), 0x2345_6789);
}

#[test]
fn bclr_sets_bp_and_counts() {
    let mut ipu = ipu_at(0x45);
    assert!(ipu.data_requested());
    assert_eq!(ipu.read(IPU_BP) as u32, 0x45);
    ipu.push_in(&[1, 2, 3]);
    assert_eq!(ipu.in_free(), 5);
    assert_eq!(ipu.read(IPU_BP) as u32, 0x0345);
    assert_eq!(ipu.read(IPU_CTRL) as u32 & 0xF, 3, "IFC");
    // Filling the FIFO stops the requests.
    ipu.push_in(&[0; 8]);
    assert_eq!(ipu.in_free(), 0);
    assert!(!ipu.data_requested());
    // BCLR empties it again.
    ipu.write(IPU_CMD, cmd(0, 0));
    assert_eq!(ipu.read(IPU_BP), 0);
    assert!(ipu.data_requested());
}

#[test]
fn ctrl_write_mask_idp_and_reset() {
    let mut ipu = Ipu::new();
    // Writable: IDP, AS, IVF, QST, MP1, PCT, RST; not 18-19 or 27-29.
    ipu.write(IPU_CTRL, 0x3FFF_FFFF);
    assert_eq!(ipu.read(IPU_CTRL) as u32, 0x07F1_0000, "IDP 3 becomes 1");
    ipu.write(IPU_CTRL, 0x0002_0000);
    assert_eq!(ipu.read(IPU_CTRL) as u32, 0x0002_0000);

    // Reset: FIFOs emptied, busy and the command cleared, interrupt.
    ipu.write(IPU_CTRL, 0x0000_0000);
    ipu.push_in(&[7; 2]);
    ipu.write(IPU_CMD, cmd(5, 0));
    assert!(ipu.busy());
    assert_eq!(ipu.read(IPU_CMD) >> 32, 0);
    ipu.write(IPU_CTRL, 1 << 30);
    assert!(!ipu.busy());
    assert!(ipu.take_irq());
    assert_eq!(ipu.in_free(), 8);
    assert_eq!(ipu.read(IPU_CTRL) as u32, 0);
    assert_eq!(ipu.read(IPU_CMD), 0);
}

#[test]
fn busy_words_of_cmd_and_top() {
    let mut ipu = ipu_at(0);
    ipu.write(IPU_CMD, cmd(4, 0));
    assert!(ipu.busy());
    assert_eq!(ipu.read(IPU_CMD) >> 32, 0x8000_0000);
    assert_eq!(ipu.read(IPU_TOP) >> 32, 0x8000_0000);
    // The 32-bit word above TOP reads the busy bit too.
    assert_eq!(ipu.read(IPU_TOP + 4) as u32, 0x8000_0000);
    ipu.push_in(&[0x0403_0201]);
    assert!(ipu.run());
    assert_eq!(ipu.read(IPU_CMD), 0x0102_0304);
    assert_eq!(ipu.read(IPU_TOP), 0x0102_0304);
}

#[test]
fn out_of_page_and_other_words() {
    let mut ipu = Ipu::new();
    // Words PCSX2 does not handle read back as written.
    ipu.write(IPU_TOP, 0x1234);
    assert_eq!(ipu.read(IPU_TOP) as u32, 0x1234);
    // The page mirrors every 0x100 bytes for reads.
    assert_eq!(ipu.read(IPU_TOP + 0x100) as u32, 0x1234);
}

#[test]
fn bdec_non_intra_field_dct() {
    // Non-intra matrix all 16, quantiser scale code 8 (scale 16).
    let mut ipu = ipu_at(0);
    ipu.write(IPU_CMD, cmd(5, 1 << 27));
    let _ = drive(&mut ipu, &qws(&[16; 64]));
    // Coded block pattern '111' (the four luma blocks), then each block's
    // first coefficient '1s' (level 1) and '10' (end of block): level 1
    // gives ((2 + 1) * 16 * 16) >> 5 = 24, a flat (24 + 4) >> 3 = 3, or
    // with the sign set -24 and (-24 + 4) >> 3 = -3.
    let b = Bits::default()
        .s("111")
        .s("10 10")
        .s("10 10")
        .s("11 10")
        .s("10 10")
        .bytes(&[0, 0, 1, 0x00]);
    ipu.push_in(&b.qws());
    // Field DCT (DT): the third block fills the odd rows' left half.
    ipu.write(IPU_CMD, cmd(2, (1 << 25) | (8 << 16)));
    let out = drive(&mut ipu, &[]);
    assert_eq!(out.len(), 48);
    let s: Vec<i16> = bytes_of(&out)
        .chunks(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    for y in 0..16 {
        for x in 0..16 {
            let expect = if y % 2 == 1 && x < 8 { -3 } else { 3 };
            assert_eq!(s[y * 16 + x], expect, "({x}, {y})");
        }
    }
    assert!(s[256..].iter().all(|&v| v == 0), "chroma not coded");
    assert_eq!((ipu.read(IPU_CTRL) as u32 >> 8) & 0x3F, 0x3C, "CBP");
    assert_eq!(ipu.read(IPU_TOP), 0x0000_0100);
}

/// Drives a command by the crate documentation's recipe alone: one run
/// after an IDEC or BDEC write (`start`), then the toIPU and fromIPU rules,
/// each running the IPU only when its waiting flag says so.
fn drive_by_recipe(ipu: &mut Ipu, input: &[u128], start: bool) -> Vec<u128> {
    if start {
        ipu.run();
    }
    let mut input = input.to_vec();
    let mut out = Vec::new();
    for _ in 0..1000 {
        if ipu.data_requested() && !input.is_empty() {
            let n = ipu.in_free().min(input.len());
            ipu.push_in(&input[..n]);
            input.drain(..n);
            if ipu.waiting_on_input() && ipu.in_free() < 8 {
                ipu.run();
            }
        }
        if ipu.out_len() > 0 {
            out.extend(ipu.pop_out(8));
            if ipu.busy() && ipu.waiting_on_output() {
                ipu.run();
            }
        }
        if !ipu.busy() {
            out.extend(ipu.pop_out(8));
            return out;
        }
    }
    panic!("the recipe left the command stuck");
}

#[test]
fn recipe_drives_idec_and_bdec() {
    let mut b = intra_mb(Bits::default().s("1")).s("1").s("1");
    for _ in 0..4 {
        b = b.s("100 10");
    }
    let idec = b.s("00 10 00 10").bytes(&[0, 0, 1, 0x01]).qws();
    let mut ipu = ipu_at(0);
    ipu.write(IPU_CMD, cmd(1, 0));
    let by_recipe = drive_by_recipe(&mut ipu, &idec, true);
    let mut ipu = ipu_at(0);
    ipu.write(IPU_CMD, cmd(1, 0));
    assert_eq!(by_recipe, drive(&mut ipu, &idec));
    assert_eq!(by_recipe.len(), 128);

    let mut ipu = ipu_at(0);
    ipu.write(IPU_CMD, cmd(2, BDEC_INTRA));
    let out = drive_by_recipe(&mut ipu, &bdec_stream().qws(), true);
    check_bdec_output(&mut ipu, &out);

    // CSC starts inside the write and stops for input there.
    let mut ipu = ipu_at(0);
    ipu.write(IPU_CMD, cmd(7, 1));
    assert!(ipu.waiting_on_input());
    let out = drive_by_recipe(&mut ipu, &qws(&flat_mb8(128, 128, 128)), false);
    assert_eq!(out.len(), 64);
}

#[test]
fn non_intra_resumes_between_code_and_level() {
    // From bit 96: CBP '111', a block of four coefficients ('10', then
    // '110' three times, '10'), then three blocks of one. The second
    // block's first code ('1') ends at bit 113, and its sign bit is in the
    // next quadword.
    let stream = Bits::default()
        .v(0, 96)
        .s("111")
        .s("10 110 110 110 10")
        .s("10 10 10 10 10 10")
        .bytes(&[0, 0, 1, 0]);
    let input = stream.qws();
    let decode = |split: bool| {
        let mut ipu = ipu_at(0);
        ipu.write(IPU_CMD, cmd(5, 1 << 27));
        let _ = drive(&mut ipu, &qws(&[16; 64]));
        ipu.write(IPU_CMD, cmd(0, 96));
        ipu.write(IPU_CMD, cmd(2, 8 << 16));
        let rest = if split {
            ipu.push_in(&input[..1]);
            ipu.run();
            assert!(ipu.busy() && ipu.waiting_on_input());
            assert_eq!(ipu.cmd.pos[1], 2, "second block");
            assert_eq!(ipu.cmd.pos[5], 1, "code read, level not");
            &input[1..]
        } else {
            &input[..]
        };
        drive(&mut ipu, rest)
    };
    let whole = decode(false);
    assert_eq!(whole.len(), 48);
    assert!(whole.iter().any(|&q| q != 0));
    assert_eq!(decode(true), whole);
}
