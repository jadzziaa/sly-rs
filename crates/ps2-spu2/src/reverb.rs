//! The reverb, as PCSX2's `pcsx2/SPU2/Reverb.cpp` runs it (Dr. Hell's
//! algorithm without the 35-tap FIR), at half the output rate: left on odd
//! samples, right on even ones, through the 39-tap downsampling and
//! upsampling filters of `ReverbResample.cpp`.

use crate::mixer::{Stereo, clamp_mix};
use crate::{Core, Spu2, revb};

const NUM_TAPS: u32 = 39;

/// The downsampling filter's taps, padded to 48 with zeros as PCSX2 keeps
/// them for its vector loads.
const FILTER_DOWN_COEFS: [i16; 48] = [
    -1, 0, 2, 0, -10, 0, 35, 0, -103, 0, 266, 0, -616, 0, 1332, 0, -2960, 0, 10246, 16384, 10246,
    0, -2960, 0, 1332, 0, -616, 0, 266, 0, -103, 0, 35, 0, -10, 0, 2, 0, -1, 0, 0, 0, 0, 0, 0, 0,
    0, 0,
];

/// The upsampling filter: the same taps doubled, clamped to 16 bits.
const FILTER_UP_COEFS: [i16; 48] = {
    let mut ret = [0; 48];
    let mut i = 0;
    while i < NUM_TAPS as usize {
        let x = FILTER_DOWN_COEFS[i] as i32 * 2;
        ret[i] = if x > i16::MAX as i32 {
            i16::MAX
        } else {
            x as i16
        };
        i += 1;
    }
    ret
};

/// `pmulhrsw`: a 16-bit product rounded to its top 16 bits.
fn mul16hrs(a: i16, b: i16) -> i16 {
    ((((i32::from(a) * i32::from(b)) >> 14) + 1) >> 1) as i16
}

/// The filter over 48 samples from `index`, as PCSX2's AVX2 path computes
/// it (`ReverbDownsample_avx`, `ReverbUpsample_avx`): every product rounded
/// to 16 bits, then summed in 16 lanes with saturation, the lanes' halves
/// added, and a pairwise tree (`hadds`). PCSX2's SSE4 path groups the sums
/// in 8 lanes instead, which differs only when a partial sum saturates; its
/// reference functions (exact sums, one shift) are not what it runs.
fn fir(buf: &[i16; 128], index: usize, coefs: &[i16; 48]) -> i32 {
    let m = |j: usize| mul16hrs(buf[index + j], coefs[j]);
    let acc: [i16; 16] =
        std::array::from_fn(|j| m(j).saturating_add(m(16 + j)).saturating_add(m(32 + j)));
    let t: [i16; 8] = std::array::from_fn(|j| acc[j].saturating_add(acc[j + 8]));
    let h1: [i16; 4] = std::array::from_fn(|k| t[2 * k].saturating_add(t[2 * k + 1]));
    let h2 = [h1[0].saturating_add(h1[1]), h1[2].saturating_add(h1[3])];
    i32::from(h2[0].saturating_add(h2[1]))
}

/// PCSX2's `ReverbDownsample`.
fn downsample(core: &Core, right: bool) -> i32 {
    let index = (core.revb_sample_buf_pos.wrapping_sub(NUM_TAPS) & 63) as usize;
    fir(
        &core.revb_down_buf[usize::from(right)],
        index,
        &FILTER_DOWN_COEFS,
    )
}

/// PCSX2's `ReverbUpsample`.
fn upsample(core: &Core) -> Stereo {
    let index = (core.revb_sample_buf_pos.wrapping_sub(NUM_TAPS) & 63) as usize;
    [
        fir(&core.revb_up_buf[0], index, &FILTER_UP_COEFS),
        fir(&core.revb_up_buf[1], index, &FILTER_UP_COEFS),
    ]
}

/// PCSX2's `MUL`: a 16-bit volume times a sample, wrapping as its `int`
/// arithmetic does on x86.
fn mul(x: i32, y: i32) -> i32 {
    x.wrapping_mul(y) >> 15
}

impl Spu2 {
    /// A buffer offset as an address in the effects area, which the
    /// buffer walks through one step every two samples (PCSX2's
    /// `RevbGetIndexer`, all in wrapping 32-bit unsigned arithmetic).
    fn revb_indexer(&self, core: usize, offset: u32) -> u32 {
        let c = &self.cores[core];
        let start = c.effects_start_a & 0x3F_FFFF;
        let end = (c.effects_end_a & 0x3F_FFFF) | 0xFFFF;
        let len = end.wrapping_sub(start).wrapping_add(1);
        let pos = (self.cycles >> 1).wrapping_add(offset);
        // PCSX2 divides by zero when ESA is just past EEA's 64 Ki page.
        let x = pos.checked_rem(len).unwrap_or(pos);
        x.wrapping_add(start) & 0xF_FFFF
    }

    /// One sample through the reverb (PCSX2's `V_Core::DoReverb`).
    pub(crate) fn do_reverb(&mut self, core: usize, input: Stereo) -> Stereo {
        {
            let c = &self.cores[core];
            if c.effects_start_a >= c.effects_end_a {
                return [0, 0];
            }
        }

        let input = [clamp_mix(input[0]), clamp_mix(input[1])];
        {
            let c = &mut self.cores[core];
            let pos = c.revb_sample_buf_pos as usize;
            for (buf, &x) in c.revb_down_buf.iter_mut().zip(&input) {
                buf[pos] = x as i16;
                buf[pos | 64] = x as i16;
            }
        }

        let right = self.cycles & 1 != 0;
        let offsets = self.cores[core].revb_addr;
        let pick = |r: usize, l: usize| if right { offsets[r] } else { offsets[l] };
        let ix = |offset: u32| self.revb_indexer(core, offset);

        let same_src = ix(pick(revb::SAME_R_SRC, revb::SAME_L_SRC));
        let same_dst = ix(pick(revb::SAME_R_DST, revb::SAME_L_DST));
        let same_prv = ix(pick(revb::SAME_R_DST, revb::SAME_L_DST).wrapping_sub(1));

        // The opposite side's DIFF source, as in PCSX2.
        let diff_src = ix(pick(revb::DIFF_L_SRC, revb::DIFF_R_SRC));
        let diff_dst = ix(pick(revb::DIFF_R_DST, revb::DIFF_L_DST));
        let diff_prv = ix(pick(revb::DIFF_R_DST, revb::DIFF_L_DST).wrapping_sub(1));

        let comb1_src = ix(pick(revb::COMB1_R_SRC, revb::COMB1_L_SRC));
        let comb2_src = ix(pick(revb::COMB2_R_SRC, revb::COMB2_L_SRC));
        let comb3_src = ix(pick(revb::COMB3_R_SRC, revb::COMB3_L_SRC));
        let comb4_src = ix(pick(revb::COMB4_R_SRC, revb::COMB4_L_SRC));

        let apf1_dst_off = pick(revb::APF1_R_DST, revb::APF1_L_DST);
        let apf2_dst_off = pick(revb::APF2_R_DST, revb::APF2_L_DST);
        let apf1_src = ix(apf1_dst_off.wrapping_sub(offsets[revb::APF1_SIZE]));
        let apf1_dst = ix(apf1_dst_off);
        let apf2_src = ix(apf2_dst_off.wrapping_sub(offsets[revb::APF2_SIZE]));
        let apf2_dst = ix(apf2_dst_off);

        // Either core's IRQ, if IRQA is in this core's effects area and the
        // effects are on.
        let addrs = [
            same_src, diff_src, same_dst, diff_dst, same_prv, diff_prv, comb1_src, comb2_src,
            comb3_src, comb4_src, apf1_dst, apf1_src, apf2_dst, apf2_src,
        ];
        let (fx_enable, esa, eea) = {
            let c = &self.cores[core];
            (c.fx_enable, c.effects_start_a, c.effects_end_a)
        };
        for i in 0..2 {
            let irqa = self.cores[i].irqa;
            if fx_enable
                && self.cores[i].irq_enable
                && (esa..=eea).contains(&irqa)
                && addrs.contains(&irqa)
            {
                self.set_irq_call(i);
            }
        }

        let vols = self.cores[core].revb_vol;
        let vol = |n: usize| i32::from(vols[n]);
        let m = |addr: u32| i32::from(self.mem(addr));

        let coef = vol(if right {
            revb::IN_COEF_R
        } else {
            revb::IN_COEF_L
        });
        let input = mul(coef, downsample(&self.cores[core], right));

        let same = mul(
            vol(revb::IIR_VOL),
            input
                .wrapping_add(mul(vol(revb::WALL_VOL), m(same_src)))
                .wrapping_sub(m(same_prv)),
        )
        .wrapping_add(m(same_prv));
        let diff = mul(
            vol(revb::IIR_VOL),
            input
                .wrapping_add(mul(vol(revb::WALL_VOL), m(diff_src)))
                .wrapping_sub(m(diff_prv)),
        )
        .wrapping_add(m(diff_prv));

        let mut out = mul(vol(revb::COMB1_VOL), m(comb1_src))
            .wrapping_add(mul(vol(revb::COMB2_VOL), m(comb2_src)))
            .wrapping_add(mul(vol(revb::COMB3_VOL), m(comb3_src)))
            .wrapping_add(mul(vol(revb::COMB4_VOL), m(comb4_src)));

        let apf1 = out.wrapping_sub(mul(vol(revb::APF1_VOL), m(apf1_src)));
        out = m(apf1_src).wrapping_add(mul(vol(revb::APF1_VOL), apf1));
        let apf2 = out.wrapping_sub(mul(vol(revb::APF2_VOL), m(apf2_src)));
        out = m(apf2_src).wrapping_add(mul(vol(revb::APF2_VOL), apf2));

        // The effects always run but only write back when enabled (no$psx).
        // These writes skip the IRQ check, as in PCSX2.
        if fx_enable {
            self.set_mem(same_dst, clamp_mix(same) as u16);
            self.set_mem(diff_dst, clamp_mix(diff) as u16);
            self.set_mem(apf1_dst, clamp_mix(apf1) as u16);
            self.set_mem(apf2_dst, clamp_mix(apf2) as u16);
        }

        let out = clamp_mix(out) as i16;
        let c = &mut self.cores[core];
        let pos = c.revb_sample_buf_pos as usize;
        let (side, other) = (usize::from(right), usize::from(!right));
        c.revb_up_buf[side][pos] = out;
        c.revb_up_buf[other][pos] = 0;
        c.revb_up_buf[side][pos | 64] = out;
        c.revb_up_buf[other][pos | 64] = 0;
        c.revb_sample_buf_pos = (c.revb_sample_buf_pos + 1) & 63;

        upsample(c)
    }
}
