//! COP2: VU0 driven by the EE in macro mode, and the moves between the EE
//! and VU0 registers.
//!
//! Upper (FMAC) operations update the MAC and status flags for the
//! components they write; min/max, conversions, `abs` and moves do not.
//! The division unit's result (Q) is ready at once: `vwaitq` has nothing to
//! wait for in macro mode.

use ps2_isa::ee::{Insn, Op};

use crate::{Cpu, Memory, Outcome, float};

/// The value of `vf[reg]` (`vf0` is constant).
#[must_use]
pub fn vf(cpu: &Cpu, reg: u8) -> u128 {
    let v = cpu.vu0.vf[reg as usize];
    u128::from(v[0]) | u128::from(v[1]) << 32 | u128::from(v[2]) << 64 | u128::from(v[3]) << 96
}

/// Sets `vf[reg]` (writes to `vf0` are ignored).
pub fn set_vf(cpu: &mut Cpu, reg: u8, value: u128) {
    if reg != 0 {
        cpu.vu0.vf[reg as usize] = [
            value as u32,
            (value >> 32) as u32,
            (value >> 64) as u32,
            (value >> 96) as u32,
        ];
    }
}

/// Bit masks of the status flags.
mod status {
    pub(super) const Z: u32 = 1;
    pub(super) const S: u32 = 1 << 1;
    pub(super) const U: u32 = 1 << 2;
    pub(super) const O: u32 = 1 << 3;
    pub(super) const I: u32 = 1 << 4;
    pub(super) const D: u32 = 1 << 5;
}

/// Whether component `i` (0 = x … 3 = w) is in the 4-bit `dest` mask.
const fn in_dest(dest: u8, i: usize) -> bool {
    dest & (8 >> i) != 0
}

/// Where an upper operation's second operand comes from.
#[derive(Clone, Copy)]
enum Operand {
    Ft,
    Bc,
    Q,
    I,
}

/// What an upper operation computes.
#[derive(Clone, Copy)]
enum Arith {
    Add,
    Sub,
    Mul,
    Madd,
    Msub,
    Max,
    Min,
}

/// Runs a COP2 instruction.
#[allow(clippy::too_many_lines, reason = "one arm per operation")]
pub(crate) fn execute(cpu: &mut Cpu, mem: &mut Memory, insn: Insn) -> Outcome {
    use Op::*;
    let (ft, fs, fd) = (insn.rt(), insn.rd(), insn.sa());
    let dest = insn.dest();
    let bc = (insn.word() & 3) as usize;
    let fsf = ((insn.word() >> 21) & 3) as usize;
    let ftf = ((insn.word() >> 23) & 3) as usize;
    let (it, is, id) = ((ft & 15) as usize, (fs & 15) as usize, (fd & 15) as usize);
    let upper = |cpu: &mut Cpu, arith: Arith, operand: Operand, to_acc: bool| {
        fmac(cpu, arith, operand, to_acc, fs, ft, fd, dest, bc);
    };
    match insn.op() {
        // Moves between the EE and VU0.
        Qmfc2Ni | Qmfc2I => cpu.set128(ft, vf(cpu, fs)),
        Qmtc2Ni | Qmtc2I => set_vf(cpu, fs, cpu.gpr[ft as usize]),
        Cfc2Ni | Cfc2I => {
            let value = control(cpu, fs);
            cpu.set32(ft, value);
        }
        Ctc2Ni | Ctc2I => return set_control(cpu, fs, cpu.r32(ft)),

        // Upper operations.
        Vadd | Vaddx | Vaddy | Vaddz | Vaddw | Vaddq | Vaddi => {
            upper(cpu, Arith::Add, operand(insn.op()), false)
        }
        Vsub | Vsubx | Vsuby | Vsubz | Vsubw | Vsubq | Vsubi => {
            upper(cpu, Arith::Sub, operand(insn.op()), false)
        }
        Vmul | Vmulx | Vmuly | Vmulz | Vmulw | Vmulq | Vmuli => {
            upper(cpu, Arith::Mul, operand(insn.op()), false)
        }
        Vmadd | Vmaddx | Vmaddy | Vmaddz | Vmaddw | Vmaddq | Vmaddi => {
            upper(cpu, Arith::Madd, operand(insn.op()), false)
        }
        Vmsub | Vmsubx | Vmsuby | Vmsubz | Vmsubw | Vmsubq | Vmsubi => {
            upper(cpu, Arith::Msub, operand(insn.op()), false)
        }
        Vmax | Vmaxx | Vmaxy | Vmaxz | Vmaxw | Vmaxi => {
            upper(cpu, Arith::Max, operand(insn.op()), false)
        }
        Vmini | Vminix | Vminiy | Vminiz | Vminiw | Vminii => {
            upper(cpu, Arith::Min, operand(insn.op()), false)
        }
        Vadda | Vaddax | Vadday | Vaddaz | Vaddaw | Vaddaq | Vaddai => {
            upper(cpu, Arith::Add, operand(insn.op()), true)
        }
        Vsuba | Vsubax | Vsubay | Vsubaz | Vsubaw | Vsubaq | Vsubai => {
            upper(cpu, Arith::Sub, operand(insn.op()), true)
        }
        Vmula | Vmulax | Vmulay | Vmulaz | Vmulaw | Vmulaq | Vmulai => {
            upper(cpu, Arith::Mul, operand(insn.op()), true)
        }
        Vmadda | Vmaddax | Vmadday | Vmaddaz | Vmaddaw | Vmaddaq | Vmaddai => {
            upper(cpu, Arith::Madd, operand(insn.op()), true)
        }
        Vmsuba | Vmsubax | Vmsubay | Vmsubaz | Vmsubaw | Vmsubaq | Vmsubai => {
            upper(cpu, Arith::Msub, operand(insn.op()), true)
        }
        Vopmula | Vopmsub => {
            let (a, b, acc) = (
                cpu.vu0.vf[fs as usize],
                cpu.vu0.vf[ft as usize],
                cpu.vu0.acc,
            );
            let mut out = acc;
            let mut flags = [float::Flags::default(); 4];
            for (i, (j, k)) in [(1, 2), (2, 0), (0, 1)].into_iter().enumerate() {
                if in_dest(dest, i) {
                    (out[i], flags[i]) = if insn.op() == Vopmula {
                        float::mul(a[j], b[k])
                    } else {
                        float::msub(acc[i], a[j], b[k])
                    };
                }
            }
            if insn.op() == Vopmula {
                cpu.vu0.acc = merge(cpu.vu0.acc, out, dest);
            } else {
                write_vf(cpu, fd, out, dest);
            }
            set_flags(cpu, out, flags, dest);
        }
        Vitof0 | Vitof4 | Vitof12 | Vitof15 | Vftoi0 | Vftoi4 | Vftoi12 | Vftoi15 => {
            let scale = match insn.op() {
                Vitof0 | Vftoi0 => 0,
                Vitof4 | Vftoi4 => 4,
                Vitof12 | Vftoi12 => 12,
                _ => 15,
            };
            let a = cpu.vu0.vf[fs as usize];
            let to_float = matches!(insn.op(), Vitof0 | Vitof4 | Vitof12 | Vitof15);
            let out = a.map(|x| {
                if to_float {
                    float::chop(f64::from(x.cast_signed()) / f64::from(1u32 << scale)).0
                } else {
                    float::to_int(float::chop(float::value(x) * f64::from(1u32 << scale)).0)
                }
            });
            write_vf(cpu, ft, out, dest);
        }
        Vabs => {
            let out = cpu.vu0.vf[fs as usize].map(|x| x & 0x7FFF_FFFF);
            write_vf(cpu, ft, out, dest);
        }
        Vclipw => {
            let a = cpu.vu0.vf[fs as usize];
            let limit = float::value(cpu.vu0.vf[ft as usize][3]).abs();
            let mut bits = 0;
            for (i, &x) in a.iter().take(3).enumerate() {
                let x = float::value(x);
                bits |= u32::from(x > limit) << (2 * i);
                bits |= u32::from(x < -limit) << (2 * i + 1);
            }
            cpu.vu0.clip = ((cpu.vu0.clip << 6) | bits) & 0x00FF_FFFF;
        }
        Vnop | Vwaitq => {}

        // Lower operations.
        Vmove => write_vf(cpu, ft, cpu.vu0.vf[fs as usize], dest),
        Vmr32 => {
            let a = cpu.vu0.vf[fs as usize];
            write_vf(cpu, ft, [a[1], a[2], a[3], a[0]], dest);
        }
        Vlqi | Vlqd => {
            if insn.op() == Vlqd {
                cpu.vu0.vi[is] = cpu.vu0.vi[is].wrapping_sub(1) & 0xFFFF;
            }
            let value = vu0_load(mem, cpu.vu0.vi[is]);
            write_vf(cpu, ft, value, dest);
            if insn.op() == Vlqi && is != 0 {
                cpu.vu0.vi[is] = cpu.vu0.vi[is].wrapping_add(1) & 0xFFFF;
            }
            cpu.vu0.vi[0] = 0;
        }
        Vsqi | Vsqd => {
            if insn.op() == Vsqd {
                cpu.vu0.vi[it] = cpu.vu0.vi[it].wrapping_sub(1) & 0xFFFF;
            }
            vu0_store(mem, cpu.vu0.vi[it], cpu.vu0.vf[fs as usize], dest);
            if insn.op() == Vsqi && it != 0 {
                cpu.vu0.vi[it] = cpu.vu0.vi[it].wrapping_add(1) & 0xFFFF;
            }
            cpu.vu0.vi[0] = 0;
        }
        Vdiv | Vsqrt | Vrsqrt => {
            let a = cpu.vu0.vf[fs as usize][fsf];
            let b = cpu.vu0.vf[ft as usize][ftf];
            let (q, flags) = match insn.op() {
                Vdiv => float::div(a, b),
                Vsqrt => float::sqrt(b),
                _ => float::rsqrt(a, b),
            };
            cpu.vu0.q = q;
            let mut st = cpu.vu0.status & !(status::I | status::D);
            st |= if flags.invalid {
                status::I | (status::I << 6)
            } else {
                0
            };
            st |= if flags.divide_by_zero {
                status::D | (status::D << 6)
            } else {
                0
            };
            cpu.vu0.status = st;
        }
        Vmtir => {
            if it != 0 {
                cpu.vu0.vi[it] = cpu.vu0.vf[fs as usize][fsf] & 0xFFFF;
            }
        }
        Vmfir => {
            let v = i32::from(cpu.vu0.vi[is] as u16 as i16).cast_unsigned();
            write_vf(cpu, ft, [v; 4], dest);
        }
        VilwrX | VilwrY | VilwrZ | VilwrW => {
            let component = match insn.op() {
                VilwrX => 0,
                VilwrY => 1,
                VilwrZ => 2,
                _ => 3,
            };
            if it != 0 {
                cpu.vu0.vi[it] = vu0_load(mem, cpu.vu0.vi[is])[component] & 0xFFFF;
            }
        }
        ViswrX | ViswrY | ViswrZ | ViswrW => {
            let value = [cpu.vu0.vi[it] & 0xFFFF; 4];
            vu0_store(mem, cpu.vu0.vi[is], value, dest);
        }
        Vrinit => cpu.vu0.r = 0x3F80_0000 | (cpu.vu0.vf[fs as usize][fsf] & 0x007F_FFFF),
        Vrxor => {
            cpu.vu0.r = 0x3F80_0000 | ((cpu.vu0.r ^ cpu.vu0.vf[fs as usize][fsf]) & 0x007F_FFFF)
        }
        Vrget => write_vf(cpu, ft, [cpu.vu0.r; 4], dest),
        Vrnext => {
            let r = cpu.vu0.r;
            let feedback = ((r >> 4) & 1) ^ ((r >> 22) & 1);
            cpu.vu0.r = 0x3F80_0000 | (((r << 1) ^ feedback) & 0x007F_FFFF);
            write_vf(cpu, ft, [cpu.vu0.r; 4], dest);
        }
        Viadd | Visub | Viand | Vior => {
            let (a, b) = (cpu.vu0.vi[is], cpu.vu0.vi[it]);
            let v = match insn.op() {
                Viadd => a.wrapping_add(b),
                Visub => a.wrapping_sub(b),
                Viand => a & b,
                _ => a | b,
            };
            if id != 0 {
                cpu.vu0.vi[id] = v & 0xFFFF;
            }
        }
        Viaddi => {
            if it != 0 {
                cpu.vu0.vi[it] =
                    cpu.vu0.vi[is].wrapping_add(i32::from(insn.imm5()).cast_unsigned()) & 0xFFFF;
            }
        }
        Vcallms => return Outcome::VuCall(insn.imm15() * 8),
        Vcallmsr => return Outcome::VuCall((cpu.vu0.cmsar0 & 0xFFFF) * 8),
        _ => return Outcome::Reserved,
    }
    Outcome::Next
}

fn operand(op: Op) -> Operand {
    let name = op.mnemonic();
    match name.as_bytes().last() {
        Some(b'q') => Operand::Q,
        Some(b'i') if !matches!(op, Op::Vmini) => Operand::I,
        Some(b'x' | b'y' | b'z' | b'w') if !matches!(op, Op::Vmax) => Operand::Bc,
        _ => Operand::Ft,
    }
}

/// An upper arithmetic operation over the `dest` components.
#[allow(clippy::too_many_arguments, reason = "the instruction's fields")]
fn fmac(
    cpu: &mut Cpu,
    arith: Arith,
    operand: Operand,
    to_acc: bool,
    fs: u8,
    ft: u8,
    fd: u8,
    dest: u8,
    bc: usize,
) {
    let a = cpu.vu0.vf[fs as usize];
    let t = cpu.vu0.vf[ft as usize];
    let acc = cpu.vu0.acc;
    let mut out = if to_acc { acc } else { cpu.vu0.vf[fd as usize] };
    let mut flags = [float::Flags::default(); 4];
    for i in (0..4).filter(|&i| in_dest(dest, i)) {
        let b = match operand {
            Operand::Ft => t[i],
            Operand::Bc => t[bc],
            Operand::Q => cpu.vu0.q,
            Operand::I => cpu.vu0.i,
        };
        (out[i], flags[i]) = match arith {
            Arith::Add => float::add(a[i], b),
            Arith::Sub => float::sub(a[i], b),
            Arith::Mul => float::mul(a[i], b),
            Arith::Madd => float::madd(acc[i], a[i], b),
            Arith::Msub => float::msub(acc[i], a[i], b),
            Arith::Max => (float::max(a[i], b), float::Flags::default()),
            Arith::Min => (float::min(a[i], b), float::Flags::default()),
        };
    }
    if to_acc {
        cpu.vu0.acc = out;
    } else {
        write_vf(cpu, fd, out, dest);
    }
    if !matches!(arith, Arith::Max | Arith::Min) {
        set_flags(cpu, out, flags, dest);
    }
}

/// Updates the MAC and status flags from the components in `dest`.
fn set_flags(cpu: &mut Cpu, out: [u32; 4], flags: [float::Flags; 4], dest: u8) {
    let mut mac = 0;
    for i in (0..4).filter(|&i| in_dest(dest, i)) {
        let bit = 3 - i;
        let value = out[i];
        mac |= u32::from(value << 1 == 0) << bit;
        mac |= u32::from(value >> 31 == 1) << (4 + bit);
        mac |= u32::from(flags[i].underflow) << (8 + bit);
        mac |= u32::from(flags[i].overflow) << (12 + bit);
    }
    cpu.vu0.mac = mac;
    let mut current = 0;
    for (group, flag) in [status::Z, status::S, status::U, status::O]
        .into_iter()
        .enumerate()
    {
        if mac & (0xF << (4 * group)) != 0 {
            current |= flag;
        }
    }
    let sticky = (cpu.vu0.status & 0xFC0) | (current << 6);
    cpu.vu0.status = (cpu.vu0.status & (status::I | status::D)) | current | sticky;
}

fn merge(old: [u32; 4], new: [u32; 4], dest: u8) -> [u32; 4] {
    std::array::from_fn(|i| if in_dest(dest, i) { new[i] } else { old[i] })
}

fn write_vf(cpu: &mut Cpu, reg: u8, value: [u32; 4], dest: u8) {
    if reg != 0 {
        cpu.vu0.vf[reg as usize] = merge(cpu.vu0.vf[reg as usize], value, dest);
    }
}

/// VU0 data memory is 4 KiB, addressed in quadwords that wrap.
fn vu0_slot(address: u32) -> usize {
    ((address & 0xFF) * 16) as usize
}

fn vu0_load(mem: &Memory, address: u32) -> [u32; 4] {
    let at = vu0_slot(address);
    let bytes = &mem.regions.vu0_data[at..at + 16];
    std::array::from_fn(|i| {
        u32::from_le_bytes([
            bytes[4 * i],
            bytes[4 * i + 1],
            bytes[4 * i + 2],
            bytes[4 * i + 3],
        ])
    })
}

fn vu0_store(mem: &mut Memory, address: u32, value: [u32; 4], dest: u8) {
    let at = vu0_slot(address);
    for i in (0..4).filter(|&i| in_dest(dest, i)) {
        mem.regions.vu0_data[at + 4 * i..at + 4 * i + 4].copy_from_slice(&value[i].to_le_bytes());
    }
}

/// `cfc2` sources.
fn control(cpu: &Cpu, reg: u8) -> u32 {
    let vu = &cpu.vu0;
    match reg {
        0..=15 => vu.vi[reg as usize],
        16 => vu.status,
        17 => vu.mac,
        18 => vu.clip,
        20 => vu.r,
        21 => vu.i,
        22 => vu.q,
        26 => vu.tpc,
        27 => vu.cmsar0,
        28 => vu.fbrst,
        _ => 0,
    }
}

/// `ctc2` destinations.
fn set_control(cpu: &mut Cpu, reg: u8, value: u32) -> Outcome {
    let vu = &mut cpu.vu0;
    match reg {
        1..=15 => vu.vi[reg as usize] = value & 0xFFFF,
        16 => vu.status = (vu.status & 0x3F) | (value & 0xFC0),
        18 => vu.clip = value & 0x00FF_FFFF,
        20 => vu.r = value,
        21 => vu.i = value,
        22 => vu.q = value,
        27 => vu.cmsar0 = value & 0xFFFF,
        28 => vu.fbrst = value & 0x0C0C,
        31 => return Outcome::Vu1Call((value & 0xFFFF) * 8),
        _ => {}
    }
    Outcome::Next
}
