//! The R5900's multimedia instructions (MMI): 128-bit integer SIMD over the
//! GPRs, and the multiply/divide units' two pipelines (HI/LO and HI1/LO1).

use ps2_isa::ee::{Insn, Op};

use crate::Cpu;

fn words(x: u128) -> [u32; 4] {
    [
        x as u32,
        (x >> 32) as u32,
        (x >> 64) as u32,
        (x >> 96) as u32,
    ]
}

fn from_words(w: [u32; 4]) -> u128 {
    u128::from(w[0]) | u128::from(w[1]) << 32 | u128::from(w[2]) << 64 | u128::from(w[3]) << 96
}

fn halves(x: u128) -> [u16; 8] {
    std::array::from_fn(|i| (x >> (16 * i)) as u16)
}

fn from_halves(h: [u16; 8]) -> u128 {
    h.iter()
        .enumerate()
        .fold(0, |acc, (i, &v)| acc | u128::from(v) << (16 * i))
}

fn bytes(x: u128) -> [u8; 16] {
    x.to_le_bytes()
}

fn from_bytes(b: [u8; 16]) -> u128 {
    u128::from_le_bytes(b)
}

fn sx32(v: u32) -> u64 {
    i64::from(v.cast_signed()).cast_unsigned()
}

/// Sets LO and HI of `pipe` (0 or 1) to sign-extended 32-bit values.
fn set_hilo(cpu: &mut Cpu, pipe: u32, lo: u32, hi: u32) {
    let shift = 64 * pipe;
    let mask = !(u128::from(u64::MAX) << shift);
    cpu.lo = (cpu.lo & mask) | u128::from(sx32(lo)) << shift;
    cpu.hi = (cpu.hi & mask) | u128::from(sx32(hi)) << shift;
}

fn hilo(cpu: &Cpu, pipe: u32) -> (u32, u32) {
    (
        (cpu.lo >> (64 * pipe)) as u32,
        (cpu.hi >> (64 * pipe)) as u32,
    )
}

/// Signed division with PCSX2's results for division by zero and overflow.
fn div_signed(a: i32, b: i32) -> (u32, u32) {
    if b == 0 {
        (if a >= 0 { u32::MAX } else { 1 }, a.cast_unsigned())
    } else if a == i32::MIN && b == -1 {
        (a.cast_unsigned(), 0)
    } else {
        ((a / b).cast_unsigned(), (a % b).cast_unsigned())
    }
}

fn div_unsigned(a: u32, b: u32) -> (u32, u32) {
    match a.checked_div(b) {
        Some(q) => (q, a % b),
        None => (u32::MAX, a),
    }
}

/// `mult`, `multu`, `div`, `divu`, `madd`, `maddu` and their pipeline-1
/// forms.
pub(crate) fn muldiv(cpu: &mut Cpu, op: Op, rs: u8, rt: u8, rd: u8, pipe: u32) {
    let (a, b) = (cpu.r32(rs), cpu.r32(rt));
    match op {
        Op::Mult | Op::Mult1 | Op::Madd | Op::Madd1 => {
            let mut product = i64::from(a.cast_signed()) * i64::from(b.cast_signed());
            if matches!(op, Op::Madd | Op::Madd1) {
                let (lo, hi) = hilo(cpu, pipe);
                product =
                    product.wrapping_add(((u64::from(hi) << 32) | u64::from(lo)).cast_signed());
            }
            let product = product.cast_unsigned();
            set_hilo(cpu, pipe, product as u32, (product >> 32) as u32);
            cpu.set32(rd, product as u32);
        }
        Op::Multu | Op::Multu1 | Op::Maddu | Op::Maddu1 => {
            let mut product = u64::from(a) * u64::from(b);
            if matches!(op, Op::Maddu | Op::Maddu1) {
                let (lo, hi) = hilo(cpu, pipe);
                product = product.wrapping_add((u64::from(hi) << 32) | u64::from(lo));
            }
            set_hilo(cpu, pipe, product as u32, (product >> 32) as u32);
            cpu.set32(rd, product as u32);
        }
        Op::Div | Op::Div1 => {
            let (q, r) = div_signed(a.cast_signed(), b.cast_signed());
            set_hilo(cpu, pipe, q, r);
        }
        _ => {
            let (q, r) = div_unsigned(a, b);
            set_hilo(cpu, pipe, q, r);
        }
    }
}

/// Whether `op` is handled here: the MMI operations, which `ps2-isa`'s
/// table lists together from `madd` to `pexcw`.
pub(crate) fn handles(op: Op) -> bool {
    (Op::Madd as u16..=Op::Pexcw as u16).contains(&(op as u16))
}

fn sat_i32(x: i64) -> u32 {
    x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32 as u32
}

fn sat_i16(x: i32) -> u16 {
    x.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16 as u16
}

fn sat_i8(x: i16) -> u8 {
    x.clamp(i16::from(i8::MIN), i16::from(i8::MAX)) as i8 as u8
}

/// Runs an MMI instruction.
#[allow(clippy::too_many_lines, reason = "one arm per operation")]
pub(crate) fn execute(cpu: &mut Cpu, insn: Insn) {
    use Op::*;
    let (rs, rt, rd, sa) = (insn.rs(), insn.rt(), insn.rd(), u32::from(insn.sa()));
    let s = cpu.gpr[rs as usize];
    let t = cpu.gpr[rt as usize];
    let (sw, tw) = (words(s), words(t));
    let (sh, th) = (halves(s), halves(t));
    let (sb, tb) = (bytes(s), bytes(t));
    let w = |f: &dyn Fn(u32, u32) -> u32| from_words(std::array::from_fn(|i| f(sw[i], tw[i])));
    let h = |f: &dyn Fn(u16, u16) -> u16| from_halves(std::array::from_fn(|i| f(sh[i], th[i])));
    let b = |f: &dyn Fn(u8, u8) -> u8| from_bytes(std::array::from_fn(|i| f(sb[i], tb[i])));
    let all = |c: bool| if c { u32::MAX } else { 0 };
    let result: u128 = match insn.op() {
        Madd | Maddu | Mult1 | Multu1 | Div1 | Divu1 | Madd1 | Maddu1 => {
            let pipe = u32::from(!matches!(insn.op(), Madd | Maddu));
            muldiv(cpu, insn.op(), rs, rt, rd, pipe);
            return;
        }
        Mfhi1 => return cpu.set64(rd, (cpu.hi >> 64) as u64),
        Mflo1 => return cpu.set64(rd, (cpu.lo >> 64) as u64),
        Mthi1 => {
            cpu.hi = (cpu.hi & u128::from(u64::MAX)) | (s << 64);
            return;
        }
        Mtlo1 => {
            cpu.lo = (cpu.lo & u128::from(u64::MAX)) | (s << 64);
            return;
        }
        Pmfhi => cpu.hi,
        Pmflo => cpu.lo,
        Pmthi => {
            cpu.hi = s;
            return;
        }
        Pmtlo => {
            cpu.lo = s;
            return;
        }
        Plzcw => {
            let count = |x: u32| {
                let n = if x.cast_signed() < 0 {
                    (!x).leading_zeros()
                } else {
                    x.leading_zeros()
                };
                n - 1
            };
            u128::from(count(sw[0])) | u128::from(count(sw[1])) << 32 | (s & !u128::from(u64::MAX))
        }

        // Word, halfword and byte arithmetic.
        Paddw => w(&|a, c| a.wrapping_add(c)),
        Psubw => w(&|a, c| a.wrapping_sub(c)),
        Paddh => h(&|a, c| a.wrapping_add(c)),
        Psubh => h(&|a, c| a.wrapping_sub(c)),
        Paddb => b(&|a, c| a.wrapping_add(c)),
        Psubb => b(&|a, c| a.wrapping_sub(c)),
        Paddsw => w(&|a, c| sat_i32(i64::from(a.cast_signed()) + i64::from(c.cast_signed()))),
        Psubsw => w(&|a, c| sat_i32(i64::from(a.cast_signed()) - i64::from(c.cast_signed()))),
        Paddsh => h(&|a, c| sat_i16(i32::from(a.cast_signed()) + i32::from(c.cast_signed()))),
        Psubsh => h(&|a, c| sat_i16(i32::from(a.cast_signed()) - i32::from(c.cast_signed()))),
        Paddsb => b(&|a, c| sat_i8(i16::from(a.cast_signed()) + i16::from(c.cast_signed()))),
        Psubsb => b(&|a, c| sat_i8(i16::from(a.cast_signed()) - i16::from(c.cast_signed()))),
        Padduw => w(&|a, c| a.saturating_add(c)),
        Psubuw => w(&|a, c| a.saturating_sub(c)),
        Padduh => h(&|a, c| a.saturating_add(c)),
        Psubuh => h(&|a, c| a.saturating_sub(c)),
        Paddub => b(&|a, c| a.saturating_add(c)),
        Psubub => b(&|a, c| a.saturating_sub(c)),
        Pcgtw => w(&|a, c| all(a.cast_signed() > c.cast_signed())),
        Pcgth => h(&|a, c| all(a.cast_signed() > c.cast_signed()) as u16),
        Pcgtb => b(&|a, c| all(a.cast_signed() > c.cast_signed()) as u8),
        Pceqw => w(&|a, c| all(a == c)),
        Pceqh => h(&|a, c| all(a == c) as u16),
        Pceqb => b(&|a, c| all(a == c) as u8),
        Pmaxw => w(&|a, c| a.cast_signed().max(c.cast_signed()).cast_unsigned()),
        Pminw => w(&|a, c| a.cast_signed().min(c.cast_signed()).cast_unsigned()),
        Pmaxh => h(&|a, c| a.cast_signed().max(c.cast_signed()).cast_unsigned()),
        Pminh => h(&|a, c| a.cast_signed().min(c.cast_signed()).cast_unsigned()),
        Pabsw => from_words(tw.map(|x| {
            if x == 0x8000_0000 {
                0x7FFF_FFFF
            } else {
                x.cast_signed().unsigned_abs()
            }
        })),
        Pabsh => from_halves(th.map(|x| {
            if x == 0x8000 {
                0x7FFF
            } else {
                x.cast_signed().unsigned_abs()
            }
        })),
        Padsbh => from_halves(std::array::from_fn(|i| {
            if i < 4 {
                sh[i].wrapping_sub(th[i])
            } else {
                sh[i].wrapping_add(th[i])
            }
        })),

        // Logic.
        Pand => s & t,
        Por => s | t,
        Pxor => s ^ t,
        Pnor => !(s | t),

        // Shifts.
        Psllh => from_halves(th.map(|x| x << (sa & 15))),
        Psrlh => from_halves(th.map(|x| x >> (sa & 15))),
        Psrah => from_halves(th.map(|x| (x.cast_signed() >> (sa & 15)).cast_unsigned())),
        Psllw => from_words(tw.map(|x| x << sa)),
        Psrlw => from_words(tw.map(|x| x >> sa)),
        Psraw => from_words(tw.map(|x| (x.cast_signed() >> sa).cast_unsigned())),
        Psllvw => {
            u128::from(sx32(tw[0] << (sw[0] & 31))) | u128::from(sx32(tw[2] << (sw[2] & 31))) << 64
        }
        Psrlvw => {
            u128::from(sx32(tw[0] >> (sw[0] & 31))) | u128::from(sx32(tw[2] >> (sw[2] & 31))) << 64
        }
        Psravw => {
            let sra = |x: u32, n: u32| (x.cast_signed() >> (n & 31)).cast_unsigned();
            u128::from(sx32(sra(tw[0], sw[0]))) | u128::from(sx32(sra(tw[2], sw[2]))) << 64
        }
        Qfsrv => {
            let shift = cpu.sa & 127;
            if shift == 0 {
                t
            } else {
                (t >> shift) | (s << (128 - shift))
            }
        }

        // Interleave, pack, copy and exchange.
        Pextlw => from_words([tw[0], sw[0], tw[1], sw[1]]),
        Pextuw => from_words([tw[2], sw[2], tw[3], sw[3]]),
        Pextlh => from_halves([th[0], sh[0], th[1], sh[1], th[2], sh[2], th[3], sh[3]]),
        Pextuh => from_halves([th[4], sh[4], th[5], sh[5], th[6], sh[6], th[7], sh[7]]),
        Pextlb => from_bytes(std::array::from_fn(|i| {
            if i % 2 == 0 { tb[i / 2] } else { sb[i / 2] }
        })),
        Pextub => from_bytes(std::array::from_fn(|i| {
            if i % 2 == 0 {
                tb[8 + i / 2]
            } else {
                sb[8 + i / 2]
            }
        })),
        Ppacw => from_words([tw[0], tw[2], sw[0], sw[2]]),
        Ppach => from_halves([th[0], th[2], th[4], th[6], sh[0], sh[2], sh[4], sh[6]]),
        Ppacb => from_bytes(std::array::from_fn(|i| {
            if i < 8 { tb[2 * i] } else { sb[2 * (i - 8)] }
        })),
        Pext5 => from_words(tw.map(|x| {
            ((x & 0x1F) << 3)
                | (((x >> 5) & 0x1F) << 11)
                | (((x >> 10) & 0x1F) << 19)
                | (((x >> 15) & 1) << 31)
        })),
        Ppac5 => from_words(tw.map(|x| {
            ((x >> 3) & 0x1F)
                | (((x >> 11) & 0x1F) << 5)
                | (((x >> 19) & 0x1F) << 10)
                | (((x >> 31) & 1) << 15)
        })),
        Pinth => from_halves([th[0], sh[4], th[1], sh[5], th[2], sh[6], th[3], sh[7]]),
        Pinteh => from_halves([th[0], sh[0], th[2], sh[2], th[4], sh[4], th[6], sh[6]]),
        Pcpyld => (t & u128::from(u64::MAX)) | (s << 64),
        Pcpyud => (s >> 64) | (t & !u128::from(u64::MAX)),
        Pcpyh => from_halves([th[0], th[0], th[0], th[0], th[4], th[4], th[4], th[4]]),
        Pexeh => from_halves([th[2], th[1], th[0], th[3], th[6], th[5], th[4], th[7]]),
        Prevh => from_halves([th[3], th[2], th[1], th[0], th[7], th[6], th[5], th[4]]),
        Pexch => from_halves([th[0], th[2], th[1], th[3], th[4], th[6], th[5], th[7]]),
        Pexew => from_words([tw[2], tw[1], tw[0], tw[3]]),
        Prot3w => from_words([tw[1], tw[2], tw[0], tw[3]]),
        Pexcw => from_words([tw[0], tw[2], tw[1], tw[3]]),

        // HI/LO transfers.
        PmfhlLw | PmfhlUw | PmfhlSlw | PmfhlLh | PmfhlSh => {
            let (lw, hw) = (words(cpu.lo), words(cpu.hi));
            let (lh, hh) = (halves(cpu.lo), halves(cpu.hi));
            match insn.op() {
                PmfhlLw => from_words([lw[0], hw[0], lw[2], hw[2]]),
                PmfhlUw => from_words([lw[1], hw[1], lw[3], hw[3]]),
                PmfhlSlw => {
                    let sat = |lo: u32, hi: u32| {
                        sx32(sat_i32(
                            ((u64::from(hi) << 32) | u64::from(lo)).cast_signed(),
                        ))
                    };
                    u128::from(sat(lw[0], hw[0])) | u128::from(sat(lw[2], hw[2])) << 64
                }
                PmfhlLh => from_halves([lh[0], lh[2], hh[0], hh[2], lh[4], lh[6], hh[4], hh[6]]),
                _ => {
                    let sat = |x: u32| {
                        sat_i16(
                            x.cast_signed()
                                .clamp(i32::from(i16::MIN), i32::from(i16::MAX)),
                        )
                    };
                    from_halves([
                        sat(lw[0]),
                        sat(lw[1]),
                        sat(hw[0]),
                        sat(hw[1]),
                        sat(lw[2]),
                        sat(lw[3]),
                        sat(hw[2]),
                        sat(hw[3]),
                    ])
                }
            }
        }
        PmthlLw => {
            let (mut lw, mut hw) = (words(cpu.lo), words(cpu.hi));
            (lw[0], hw[0], lw[2], hw[2]) = (sw[0], sw[1], sw[2], sw[3]);
            cpu.lo = from_words(lw);
            cpu.hi = from_words(hw);
            return;
        }

        // Parallel multiply and divide.
        Pmultw | Pmultuw | Pmaddw | Pmadduw | Pmsubw => {
            let mut out = [0u64; 2];
            for (lane, i) in [0usize, 2].into_iter().enumerate() {
                let shift = 64 * lane as u32;
                let lo = (cpu.lo >> shift) as u32;
                let hi = (cpu.hi >> shift) as u32;
                let acc = (u64::from(hi) << 32) | u64::from(lo);
                let product = match insn.op() {
                    Pmultuw | Pmadduw => u64::from(sw[i]) * u64::from(tw[i]),
                    _ => (i64::from(sw[i].cast_signed()) * i64::from(tw[i].cast_signed()))
                        .cast_unsigned(),
                };
                let value = match insn.op() {
                    Pmaddw | Pmadduw => acc.wrapping_add(product),
                    Pmsubw => acc.wrapping_sub(product),
                    _ => product,
                };
                let mask = !(u128::from(u64::MAX) << shift);
                cpu.lo = (cpu.lo & mask) | u128::from(sx32(value as u32)) << shift;
                cpu.hi = (cpu.hi & mask) | u128::from(sx32((value >> 32) as u32)) << shift;
                out[lane] = value;
            }
            u128::from(out[0]) | u128::from(out[1]) << 64
        }
        Pdivw | Pdivuw => {
            for (lane, i) in [0usize, 2].into_iter().enumerate() {
                let (q, r) = if insn.op() == Pdivw {
                    div_signed(sw[i].cast_signed(), tw[i].cast_signed())
                } else {
                    div_unsigned(sw[i], tw[i])
                };
                let shift = 64 * lane as u32;
                let mask = !(u128::from(u64::MAX) << shift);
                cpu.lo = (cpu.lo & mask) | u128::from(sx32(q)) << shift;
                cpu.hi = (cpu.hi & mask) | u128::from(sx32(r)) << shift;
            }
            return;
        }
        Pdivbw => {
            let divisor = i32::from(th[0].cast_signed());
            let (mut q, mut r) = ([0u32; 4], [0u32; 4]);
            for i in 0..4 {
                (q[i], r[i]) = div_signed(sw[i].cast_signed(), divisor);
                r[i] = sx32(i32::from(r[i] as i16).cast_unsigned()) as u32;
            }
            cpu.lo = from_words(q);
            cpu.hi = from_words(r);
            return;
        }
        Pmulth | Pmaddh | Pmsubh | Phmadh | Phmsbh => {
            let p: [i32; 8] = std::array::from_fn(|i| {
                i32::from(sh[i].cast_signed()) * i32::from(th[i].cast_signed())
            });
            let (mut lw, mut hw) = (words(cpu.lo), words(cpu.hi));
            match insn.op() {
                Pmulth | Pmaddh | Pmsubh => {
                    let combine = |acc: u32, p: i32| match insn.op() {
                        Pmaddh => acc.wrapping_add(p.cast_unsigned()),
                        Pmsubh => acc.wrapping_sub(p.cast_unsigned()),
                        _ => p.cast_unsigned(),
                    };
                    for (slot, (target, index)) in [
                        (0, 0),
                        (0, 1),
                        (1, 0),
                        (1, 1),
                        (0, 2),
                        (0, 3),
                        (1, 2),
                        (1, 3),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        let reg = if target == 0 { &mut lw } else { &mut hw };
                        reg[index] = combine(reg[index], p[slot]);
                    }
                }
                _ => {
                    let pair = |a: i32, c: i32| {
                        if insn.op() == Phmadh {
                            a.wrapping_add(c)
                        } else {
                            a.wrapping_sub(c)
                        }
                        .cast_unsigned()
                    };
                    lw[0] = pair(p[1], p[0]);
                    hw[0] = pair(p[3], p[2]);
                    lw[2] = pair(p[5], p[4]);
                    hw[2] = pair(p[7], p[6]);
                }
            }
            cpu.lo = from_words(lw);
            cpu.hi = from_words(hw);
            from_words([lw[0], hw[0], lw[2], hw[2]])
        }
        _ => return,
    };
    cpu.set128(rd, result);
}
