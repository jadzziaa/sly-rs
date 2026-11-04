//! The GPU GS draws what the software GS draws, word for word, over
//! random memory: triangles (lists, strips, fans) and sprites in every
//! frame, depth and texture format, nearest and bilinear, every texture
//! function, wrap mode, alpha and destination alpha test, depth test,
//! blend, fog, scan mask, FBMSK, PABE, COLCLAMP and FBA, UV and STQ (Q of
//! either sign), with CLUTs loaded from memory.
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

/// One case: registers, then primitives, through a GS drawing on two
/// threads with the GPU as its tap.
fn draw(gpu: &Shared, r: &mut Random) -> (Vec<u32>, Vec<u32>, String, usize) {
    let mut gs = Gs::default();
    let random: Vec<u8> = (0..ps2_gs::mem::SIZE).map(|_| r.next() as u8).collect();
    gs.mem.load_bytes(&random);
    gs.set_threads(2);
    gs.set_tap(Some(Box::new(gpu.clone())));
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
    }
    gs.flush();
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
    (cpu, words, case, changed)
}

#[test]
fn the_gpu_draws_what_the_software_gs_draws() {
    let Ok(gpu) = GpuGs::new() else {
        return;
    };
    let gpu = Shared(Arc::new(Mutex::new(gpu)));
    let mut r = Random(0x2545_F491_4F6C_DD1D);
    let (mut drew, mut words) = (0, 0);
    for case in 0..160 {
        let (cpu, gpu_words, what, changed) = draw(&gpu, &mut r);
        let differ = (0..cpu.len()).filter(|&i| cpu[i] != gpu_words[i]).count();
        assert_eq!(differ, 0, "case {case} ({what}): {differ} words differ");
        drew += usize::from(changed > 0);
        words += changed;
    }
    let left_out = gpu.0.lock().map_or(1, |g| g.skipped);
    assert_eq!(left_out, 0, "primitives left out");
    // Tests reject some cases whole, but not most.
    assert!(
        drew >= 100,
        "{drew} of 160 cases drew anything ({words} words)"
    );
}
