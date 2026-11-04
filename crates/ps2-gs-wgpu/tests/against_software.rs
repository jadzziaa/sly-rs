//! The GPU GS draws what the software GS draws, word for word, over
//! random memory: points, lines, triangles (lists, strips, fans; either
//! antialiased) and sprites (UV or STQ) in every
//! frame, depth and texture format, nearest and bilinear, every texture
//! function, wrap mode, alpha and destination alpha test, depth test,
//! blend, fog, scan mask, FBMSK, PABE, COLCLAMP and FBA, UV and STQ (Q of
//! either sign), with CLUTs loaded from memory and transfers into the
//! blocks drawn among the primitives; and the display read-out of what
//! they drew. Each case is drawn twice: with the CPU drawing too, and with
//! the GPU alone (batching on a thread of its own).
//!
//! Skipped on a machine with no GPU that has 64-bit integers in shaders.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "small random values"
)]

use std::sync::{Arc, Mutex};

use ps2_gs::Gs;
use ps2_gs::regs::addr;
use ps2_gs_wgpu::{GpuGs, Shared};

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        from[self.below(from.len() as u64) as usize]
    }
}

/// A host-to-local transfer of random pixels into the frame, the depth
/// buffer or the texture, somewhere in the 64 × 64 they use.
fn transfer(gs: &mut Gs, r: &mut Random, frame_psm: u64, zpsm: u64, tex_psm: u64) {
    let (bp, bw, psm) = r.pick(&[
        (0u64, 1u64, frame_psm),
        (0x40, 1, 0x30 | zpsm),
        (0x80, 2, tex_psm),
    ]);
    let (width, height) = (r.pick(&[8u64, 16, 32]), r.pick(&[4u64, 8]));
    let (left, top) = (r.below(64 - width), r.below(64 - height));
    gs.write(addr::BITBLTBUF, bp << 32 | bw << 48 | psm << 56);
    gs.write(addr::TRXPOS, left << 32 | top << 48);
    gs.write(addr::TRXREG, width | height << 32);
    gs.write(addr::TRXDIR, 0);
    let bits = match psm {
        0x00 | 0x30 | 0x1B | 0x24 | 0x2C => 32,
        0x01 | 0x31 => 24,
        0x13 => 8,
        0x14 => 4,
        _ => 16,
    };
    // T8H, T4HL and T4HH move 8 or 4 bits a pixel.
    let bits = match psm {
        0x1B => 8,
        0x24 | 0x2C => 4,
        _ => bits,
    };
    let qwords = (width * height * bits).div_ceil(128);
    let tag = u128::from(qwords | 1 << 15 | 2 << 58);
    let data: Vec<u128> = std::iter::once(tag)
        .chain((0..qwords).map(|_| u128::from(r.next()) | u128::from(r.next()) << 64))
        .collect();
    gs.gif(2, data);
}

/// The display read-out of the frame drawn, as each reads it: circuit 1
/// alone, or over circuit 2 reading the texture, with or without ALP.
fn pictures(
    gs: &Gs,
    gpu: &Shared,
    r: &mut Random,
    frame_psm: u64,
    tex_psm: u64,
) -> [ps2_gs::Frame; 2] {
    let regs = ps2_gs::Privileged {
        pmode: 1 | r.below(2) << 1 | r.below(2) << 5 | r.below(2) << 7 | r.below(256) << 8,
        smode2: r.below(4),
        dispfb: [
            1 << 9 | frame_psm << 15,
            4 | 2 << 9 | (tex_psm & 0x0F) << 15 | r.below(8) << 32,
        ],
        display: [
            63 << 32 | 63 << 44,
            63 << 32 | 31 << 44 | r.below(16) | r.below(16) << 12,
        ],
        bgcolor: r.next() & 0xFF_FFFF,
    };
    let on_gpu = gpu
        .0
        .lock()
        .map(|mut g| ps2_gs::export::Tap::display(&mut *g, &regs))
        .unwrap_or_default();
    [gs.display(&regs), on_gpu]
}

/// One case: registers, then primitives, through a GS drawing on two
/// threads with the GPU as its tap (and the CPU if `software`), with
/// transfers among them.
fn draw(
    gpu: &Shared,
    r: &mut Random,
    software: bool,
) -> (Vec<u32>, Vec<u32>, String, usize, [ps2_gs::Frame; 2]) {
    let mut gs = Gs::default();
    let random: Vec<u8> = (0..ps2_gs::mem::SIZE).map(|_| r.next() as u8).collect();
    gs.mem.load_bytes(&random);
    gs.set_threads(2);
    gs.set_tap(Some(Box::new(gpu.clone())), software);
    let frame_psm = r.pick(&[0x00u64, 0x01, 0x02, 0x0A]);
    let zpsm = r.pick(&[0u64, 1, 2, 0xA]);
    let tex_psm = r.pick(&[0x00u64, 0x01, 0x02, 0x0A, 0x13, 0x14, 0x1B, 0x24, 0x2C]);
    let kind = r.pick(&[3u64, 4, 5, 6]);
    let tme = r.below(4) != 0;
    // Sprites with STQ textures are not drawn on the GPU yet.
    let fst = if kind == 6 && tme { 1 } else { r.below(2) };
    let attrs =
        kind | r.below(2) << 3 | u64::from(tme) << 4 | r.below(2) << 5 | r.below(2) << 6 | fst << 8;
    let fbmsk = if r.below(4) == 0 {
        r.next() & 0xFFFF_FFFF
    } else {
        0
    };
    let test = r.below(2)
        | r.below(8) << 1
        | r.below(256) << 4
        | r.below(4) << 12
        | r.below(2) << 14
        | r.below(2) << 15
        | r.below(2) << 16
        | r.below(4) << 17;
    let clamp = {
        let (u0, v0) = (r.below(16), r.below(16));
        r.below(16) | u0 << 4 | (u0 + r.below(16)) << 14 | v0 << 24 | (v0 + r.below(16)) << 34
    };
    let regs = [
        (addr::FRAME_1, 1 << 16 | frame_psm << 24 | fbmsk << 32),
        (addr::SCISSOR_1, 63 << 16 | 63 << 48),
        (addr::ZBUF_1, 0x40 | zpsm << 24 | r.below(2) << 32),
        (addr::TEST_1, test),
        (addr::ALPHA_1, r.next() & 0xFF | r.below(256) << 32),
        (
            addr::TEX0_1,
            0x80 | 2 << 14
                | tex_psm << 20
                | 5 << 26
                | 5 << 30
                | r.below(2) << 34
                | r.below(4) << 35
                | 0x100 << 37
                | r.pick(&[0u64, 2]) << 51
                | r.below(32) << 56
                | 1 << 61,
        ),
        // LCM: the filter from MMAG alone.
        (addr::TEX1_1, 1 | r.below(2) << 5),
        (addr::CLAMP_1, clamp),
        (
            addr::TEXA,
            r.next() & 0xFF | r.below(2) << 15 | r.below(256) << 32,
        ),
        (addr::FOGCOL, r.next() & 0xFF_FFFF),
        (addr::PABE, r.below(2)),
        (addr::COLCLAMP, r.below(2)),
        (addr::FBA_1, r.below(2)),
        (addr::SCANMSK, r.below(4)),
    ];
    for (reg, value) in regs {
        gs.write(reg, value);
    }
    gs.write(addr::PRIM, attrs);
    let count = if kind == 6 { 8 } else { 9 };
    for _ in 0..count {
        let q = (0.25 + r.below(100) as f32 / 40.0) * if r.below(8) == 0 { -1.0 } else { 1.0 };
        let st = |r: &mut Random| ((r.below(64) as f32 - 8.0) * q / 32.0).to_bits();
        gs.write(
            addr::RGBAQ,
            r.next() & 0xFFFF_FFFF | u64::from(q.to_bits()) << 32,
        );
        let (s, t) = (st(r), st(r));
        gs.write(addr::ST, u64::from(s) | u64::from(t) << 32);
        gs.write(addr::UV, r.below(40 * 16) | r.below(40 * 16) << 16);
        gs.write(addr::FOG, r.below(256) << 56);
        gs.write(
            addr::XYZ2,
            r.below(72 * 16) | r.below(72 * 16) << 16 | (r.next() & 0xFFFF_FFFF) << 32,
        );
        // Transfers among the primitives, sometimes two in a row, into
        // blocks they use: the GPU must have them in the same order.
        while r.below(4) == 0 {
            transfer(&mut gs, r, frame_psm, zpsm, tex_psm);
        }
    }
    gs.flush();
    let picture = pictures(&gs, gpu, r, frame_psm, tex_psm);
    let cpu = gs.mem.to_words();
    let changed = cpu
        .iter()
        .zip(random.as_chunks::<4>().0)
        .filter(|(w, b)| **w != u32::from_le_bytes(**b))
        .count();
    let words = gpu.0.lock().map(|g| g.read_memory()).unwrap_or_default();
    let case = format!(
        "prim {attrs:#x} frame {frame_psm:#x} fbmsk {fbmsk:#x} zbuf {zpsm:#x} test {test:#x} tex {tex_psm:#x} clamp {clamp:#x}"
    );
    (cpu, words, case, changed, picture)
}

/// `cases` random cases drawn on the GPU at internal resolution `scale`,
/// each compared with the software GS: local memory (the first sample
/// plane) word for word, and the pictures, from the first plane's pixels.
fn compare(scale: u32, cases: usize) {
    let Ok(gpu) = GpuGs::with_scale(scale) else {
        return;
    };
    let gpu = Shared(Arc::new(Mutex::new(gpu)));
    let mut r = Random(0x2545_F491_4F6C_DD1D);
    let (mut drew, mut words) = (0, 0);
    for case in 0..cases {
        let mut again = Random(r.0);
        let (cpu, gpu_words, what, changed, [cpu_picture, gpu_picture]) = draw(&gpu, &mut r, true);
        let differ = (0..cpu.len()).filter(|&i| cpu[i] != gpu_words[i]).count();
        assert_eq!(differ, 0, "case {case} ({what}): {differ} words differ");
        assert_eq!(gpu_picture.factor(), scale);
        assert!(
            cpu_picture == plane_zero(&gpu_picture),
            "case {case} ({what}): the pictures differ"
        );
        // The GPU alone: the same words, the same picture.
        let (_, alone, _, _, [_, alone_picture]) = draw(&gpu, &mut again, false);
        let differ = (0..cpu.len()).filter(|&i| cpu[i] != alone[i]).count();
        assert_eq!(
            differ, 0,
            "case {case} ({what}), the GPU alone: {differ} words differ"
        );
        assert!(
            cpu_picture == plane_zero(&alone_picture),
            "case {case} ({what}), the GPU alone: the pictures differ"
        );
        drew += usize::from(changed > 0);
        words += changed;
    }
    // Tests reject some cases whole, but not most.
    assert!(
        drew * 8 >= cases * 5,
        "{drew} of {cases} cases drew anything ({words} words)"
    );
}

/// The pixels of a picture that its first sample plane gave.
fn plane_zero(frame: &ps2_gs::Frame) -> ps2_gs::Frame {
    let scale = frame.factor() as usize;
    let (width, height) = (frame.width as usize / scale, frame.height as usize / scale);
    let rgb = (0..height)
        .flat_map(|y| (0..width).map(move |x| (y * scale * frame.width as usize + x * scale) * 3))
        .flat_map(|at| frame.rgb[at..at + 3].to_vec())
        .collect();
    ps2_gs::Frame {
        width: width as u32,
        height: height as u32,
        rgb,
        scale: 1,
    }
}

#[test]
fn the_gpu_draws_what_the_software_gs_draws() {
    compare(1, 160);
}

/// At a higher internal resolution the first sample plane is the GS's
/// own: the same memory, the same pixels.
#[test]
fn at_a_higher_resolution_the_first_plane_is_the_gs() {
    compare(2, 60);
    compare(3, 20);
}
