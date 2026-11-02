//! The semantics of one R5900 instruction: [`execute`] applies it to the
//! state and memory and reports what the machine must do next. Branch
//! delay slots, exceptions, interrupts and the clock are the caller's
//! business (the interpreter's, or generated code's).
//!
//! Behaviour follows PCSX2's interpreter where the hardware manuals are
//! silent: arithmetic does not trap on overflow, division by zero gives
//! PCSX2's values, and `cache`/`pref`/`sync` and the TLB instructions do
//! nothing.

use ps2_isa::ee::{Insn, Op};

use crate::cpu::cop0;
use crate::{Cpu, Memory, float, mmi, vu0};

/// What the machine does after an instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Continue with the next instruction.
    Next,
    /// A branch or jump: the delay slot follows; then control goes to
    /// `target` if `taken`. A not-taken branch-likely skips its delay slot.
    Branch {
        /// Destination.
        target: u32,
        /// Whether the branch is taken.
        taken: bool,
        /// Whether it is a branch-likely.
        likely: bool,
    },
    /// `syscall`.
    Syscall,
    /// `break`.
    Break,
    /// A conditional trap fired.
    Trap,
    /// `eret`.
    Eret,
    /// `ei`, which may let pending interrupts in.
    InterruptsEnabled,
    /// `vcallms`/`vcallmsr`: start the VU0 micro program at this byte
    /// address of micro memory.
    VuCall(u32),
    /// `ctc2` to VU1's CMSAR (register 31): start VU1 at this byte address.
    Vu1Call(u32),
    /// Not an instruction.
    Reserved,
}

/// Runs `insn`, at address `pc`, on `cpu` and `mem`.
#[allow(clippy::too_many_lines, reason = "one arm per operation")]
pub fn execute(cpu: &mut Cpu, mem: &mut Memory, insn: Insn, pc: u32) -> Outcome {
    let (rs, rt, rd, sa) = (insn.rs(), insn.rt(), insn.rd(), insn.sa());
    let imm = i64::from(insn.simm());
    let uimm = u64::from(insn.imm());
    let s = cpu.r64(rs);
    let t = cpu.r64(rt);
    let address = (s as u32).wrapping_add(i32::from(insn.simm()).cast_unsigned());
    let branch = |_: &Cpu, taken: bool, likely: bool| Outcome::Branch {
        target: insn.branch_target(pc),
        taken,
        likely,
    };
    match insn.op() {
        Op::Invalid => return Outcome::Reserved,
        // No caches, prefetch, TLB or pipeline ordering to model.
        Op::Nop
        | Op::Sync
        | Op::SyncP
        | Op::Cache
        | Op::Pref
        | Op::Tlbr
        | Op::Tlbwi
        | Op::Tlbwr
        | Op::Tlbp => {}

        // Jumps and branches.
        Op::J => {
            return Outcome::Branch {
                target: insn.jump_target(pc),
                taken: true,
                likely: false,
            };
        }
        Op::Jal => {
            cpu.set64(31, u64::from(pc.wrapping_add(8)));
            return Outcome::Branch {
                target: insn.jump_target(pc),
                taken: true,
                likely: false,
            };
        }
        Op::Jr => {
            return Outcome::Branch {
                target: s as u32,
                taken: true,
                likely: false,
            };
        }
        Op::Jalr => {
            cpu.set64(rd, u64::from(pc.wrapping_add(8)));
            return Outcome::Branch {
                target: s as u32,
                taken: true,
                likely: false,
            };
        }
        Op::Beq => return branch(cpu, s == t, false),
        Op::Bne => return branch(cpu, s != t, false),
        Op::Blez => return branch(cpu, s.cast_signed() <= 0, false),
        Op::Bgtz => return branch(cpu, s.cast_signed() > 0, false),
        Op::Bltz => return branch(cpu, s.cast_signed() < 0, false),
        Op::Bgez => return branch(cpu, s.cast_signed() >= 0, false),
        Op::Beql => return branch(cpu, s == t, true),
        Op::Bnel => return branch(cpu, s != t, true),
        Op::Blezl => return branch(cpu, s.cast_signed() <= 0, true),
        Op::Bgtzl => return branch(cpu, s.cast_signed() > 0, true),
        Op::Bltzl => return branch(cpu, s.cast_signed() < 0, true),
        Op::Bgezl => return branch(cpu, s.cast_signed() >= 0, true),
        Op::Bltzal | Op::Bgezal | Op::Bltzall | Op::Bgezall => {
            cpu.set64(31, u64::from(pc.wrapping_add(8)));
            let taken = if matches!(insn.op(), Op::Bltzal | Op::Bltzall) {
                s.cast_signed() < 0
            } else {
                s.cast_signed() >= 0
            };
            return branch(cpu, taken, matches!(insn.op(), Op::Bltzall | Op::Bgezall));
        }
        Op::Bc0f | Op::Bc0t | Op::Bc0fl | Op::Bc0tl => {
            let taken = cpu.cpcond0 == matches!(insn.op(), Op::Bc0t | Op::Bc0tl);
            return branch(cpu, taken, matches!(insn.op(), Op::Bc0fl | Op::Bc0tl));
        }
        Op::Bc1f | Op::Bc1t | Op::Bc1fl | Op::Bc1tl => {
            let condition = cpu.fcr31 & (1 << 23) != 0;
            let taken = condition == matches!(insn.op(), Op::Bc1t | Op::Bc1tl);
            return branch(cpu, taken, matches!(insn.op(), Op::Bc1fl | Op::Bc1tl));
        }
        Op::Bc2f | Op::Bc2t | Op::Bc2fl | Op::Bc2tl => {
            // CPCOND2: VU0 running. Macro-mode programs finish at once here.
            let taken = !matches!(insn.op(), Op::Bc2t | Op::Bc2tl);
            return branch(cpu, taken, matches!(insn.op(), Op::Bc2fl | Op::Bc2tl));
        }

        // Exceptions.
        Op::Syscall => return Outcome::Syscall,
        Op::Break => return Outcome::Break,
        Op::Eret => return Outcome::Eret,
        Op::Tge | Op::Tgeu | Op::Tlt | Op::Tltu | Op::Teq | Op::Tne => {
            let fire = match insn.op() {
                Op::Tge => s.cast_signed() >= t.cast_signed(),
                Op::Tgeu => s >= t,
                Op::Tlt => s.cast_signed() < t.cast_signed(),
                Op::Tltu => s < t,
                Op::Teq => s == t,
                _ => s != t,
            };
            if fire {
                return Outcome::Trap;
            }
        }
        Op::Tgei | Op::Tgeiu | Op::Tlti | Op::Tltiu | Op::Teqi | Op::Tnei => {
            let i = imm.cast_unsigned();
            let fire = match insn.op() {
                Op::Tgei => s.cast_signed() >= imm,
                Op::Tgeiu => s >= i,
                Op::Tlti => s.cast_signed() < imm,
                Op::Tltiu => s < i,
                Op::Teqi => s == i,
                _ => s != i,
            };
            if fire {
                return Outcome::Trap;
            }
        }

        // ALU, immediate.
        Op::Addi | Op::Addiu => cpu.set32(rt, (s as u32).wrapping_add(imm as u32)),
        Op::Daddi | Op::Daddiu => cpu.set64(rt, s.wrapping_add(imm.cast_unsigned())),
        Op::Slti => cpu.set64(rt, u64::from(s.cast_signed() < imm)),
        Op::Sltiu => cpu.set64(rt, u64::from(s < imm.cast_unsigned())),
        Op::Andi => cpu.set64(rt, s & uimm),
        Op::Ori => cpu.set64(rt, s | uimm),
        Op::Xori => cpu.set64(rt, s ^ uimm),
        Op::Lui => cpu.set64(rt, (imm << 16).cast_unsigned()),

        // ALU, registers.
        Op::Add | Op::Addu => cpu.set32(rd, (s as u32).wrapping_add(t as u32)),
        Op::Sub | Op::Subu => cpu.set32(rd, (s as u32).wrapping_sub(t as u32)),
        Op::Dadd | Op::Daddu => cpu.set64(rd, s.wrapping_add(t)),
        Op::Dsub | Op::Dsubu => cpu.set64(rd, s.wrapping_sub(t)),
        Op::And => cpu.set64(rd, s & t),
        Op::Or => cpu.set64(rd, s | t),
        Op::Xor => cpu.set64(rd, s ^ t),
        Op::Nor => cpu.set64(rd, !(s | t)),
        Op::Slt => cpu.set64(rd, u64::from(s.cast_signed() < t.cast_signed())),
        Op::Sltu => cpu.set64(rd, u64::from(s < t)),
        Op::Movz => {
            if t == 0 {
                cpu.set64(rd, s);
            }
        }
        Op::Movn => {
            if t != 0 {
                cpu.set64(rd, s);
            }
        }

        // Shifts.
        Op::Sll => cpu.set32(rd, (t as u32) << sa),
        Op::Srl => cpu.set32(rd, (t as u32) >> sa),
        Op::Sra => cpu.set32(rd, ((t as u32).cast_signed() >> sa).cast_unsigned()),
        Op::Sllv => cpu.set32(rd, (t as u32) << (s & 31)),
        Op::Srlv => cpu.set32(rd, (t as u32) >> (s & 31)),
        Op::Srav => cpu.set32(rd, ((t as u32).cast_signed() >> (s & 31)).cast_unsigned()),
        Op::Dsll => cpu.set64(rd, t << sa),
        Op::Dsrl => cpu.set64(rd, t >> sa),
        Op::Dsra => cpu.set64(rd, (t.cast_signed() >> sa).cast_unsigned()),
        Op::Dsll32 => cpu.set64(rd, t << (u32::from(sa) + 32)),
        Op::Dsrl32 => cpu.set64(rd, t >> (u32::from(sa) + 32)),
        Op::Dsra32 => cpu.set64(
            rd,
            (t.cast_signed() >> (u32::from(sa) + 32)).cast_unsigned(),
        ),
        Op::Dsllv => cpu.set64(rd, t << (s & 63)),
        Op::Dsrlv => cpu.set64(rd, t >> (s & 63)),
        Op::Dsrav => cpu.set64(rd, (t.cast_signed() >> (s & 63)).cast_unsigned()),

        // Multiply and divide (pipeline 0; MMI has pipeline 1).
        Op::Mult | Op::Multu | Op::Div | Op::Divu => mmi::muldiv(cpu, insn.op(), rs, rt, rd, 0),
        Op::Mfhi => cpu.set64(rd, cpu.hi as u64),
        Op::Mflo => cpu.set64(rd, cpu.lo as u64),
        Op::Mthi => cpu.hi = (cpu.hi & !u128::from(u64::MAX)) | u128::from(s),
        Op::Mtlo => cpu.lo = (cpu.lo & !u128::from(u64::MAX)) | u128::from(s),
        Op::Mfsa => cpu.set64(rd, u64::from(cpu.sa)),
        Op::Mtsa => cpu.sa = s as u32,
        Op::Mtsab => cpu.sa = (((s as u32) ^ (insn.imm() as u32)) & 0xF) * 8,
        Op::Mtsah => cpu.sa = (((s as u32) ^ (insn.imm() as u32)) & 0x7) * 16,

        // Loads.
        Op::Lb => {
            let v = mem.read8(address);
            cpu.set64(rt, i64::from(v.cast_signed()).cast_unsigned());
        }
        Op::Lbu => {
            let v = mem.read8(address);
            cpu.set64(rt, u64::from(v));
        }
        Op::Lh => {
            let v = mem.read16(address);
            cpu.set64(rt, i64::from(v.cast_signed()).cast_unsigned());
        }
        Op::Lhu => {
            let v = mem.read16(address);
            cpu.set64(rt, u64::from(v));
        }
        Op::Lw => {
            let v = mem.read32(address);
            cpu.set32(rt, v);
        }
        Op::Lwu => {
            let v = mem.read32(address);
            cpu.set64(rt, u64::from(v));
        }
        Op::Ld => {
            let v = mem.read64(address);
            cpu.set64(rt, v);
        }
        Op::Lq => {
            let v = mem.read128(address & !15);
            cpu.set128(rt, v);
        }
        Op::Lwl | Op::Lwr | Op::Ldl | Op::Ldr => unaligned_load(cpu, mem, insn.op(), rt, address),
        Op::Lwc1 => cpu.fpr[rt as usize] = mem.read32(address),
        Op::Lqc2 => {
            let v = mem.read128(address & !15);
            vu0::set_vf(cpu, rt, v);
        }

        // Stores.
        Op::Sb => mem.write8(address, t as u8),
        Op::Sh => mem.write16(address, t as u16),
        Op::Sw => mem.write32(address, t as u32),
        Op::Sd => mem.write64(address, t),
        Op::Sq => mem.write128(address & !15, cpu.gpr[rt as usize]),
        Op::Swl | Op::Swr | Op::Sdl | Op::Sdr => unaligned_store(mem, insn.op(), t, address),
        Op::Swc1 => mem.write32(address, cpu.fpr[rt as usize]),
        Op::Sqc2 => mem.write128(address & !15, vu0::vf(cpu, rt)),

        // COP0.
        Op::Mfc0 => {
            let value = if rd as usize == cop0::COUNT {
                cpu.cycles as u32
            } else {
                cpu.cop0[rd as usize]
            };
            cpu.set32(rt, value);
        }
        Op::Mtc0 => cpu.cop0[rd as usize] = t as u32,
        Op::Ei => {
            cpu.cop0[cop0::STATUS] |= cop0::STATUS_EIE;
            return Outcome::InterruptsEnabled;
        }
        Op::Di => cpu.cop0[cop0::STATUS] &= !cop0::STATUS_EIE,

        // COP1.
        Op::Mfc1 => cpu.set32(rt, cpu.fpr[rd as usize]),
        Op::Mtc1 => cpu.fpr[rd as usize] = t as u32,
        Op::Cfc1 => cpu.set32(rt, if rd == 31 { cpu.fcr31 } else { 0x2E00 }),
        Op::Ctc1 => {
            if rd == 31 {
                cpu.fcr31 = t as u32;
            }
        }
        op if is_fpu(op) => fpu(cpu, insn),

        // MMI and COP2.
        op if mmi::handles(op) => mmi::execute(cpu, insn),
        _ => return vu0::execute(cpu, mem, insn),
    }
    Outcome::Next
}

fn is_fpu(op: Op) -> bool {
    matches!(
        op,
        Op::AddS
            | Op::SubS
            | Op::MulS
            | Op::DivS
            | Op::SqrtS
            | Op::AbsS
            | Op::MovS
            | Op::NegS
            | Op::RsqrtS
            | Op::AddaS
            | Op::SubaS
            | Op::MulaS
            | Op::MaddS
            | Op::MsubS
            | Op::MaddaS
            | Op::MsubaS
            | Op::CvtWS
            | Op::MaxS
            | Op::MinS
            | Op::CFS
            | Op::CEqS
            | Op::CLtS
            | Op::CLeS
            | Op::CvtSW
    )
}

/// COP1 arithmetic. `fs` is in the `rd` field, `ft` in `rt`, `fd` in `sa`.
#[allow(clippy::float_cmp, reason = "c.eq.s compares exactly")]
fn fpu(cpu: &mut Cpu, insn: Insn) {
    let (ft, fs, fd) = (insn.rt() as usize, insn.rd() as usize, insn.sa() as usize);
    let (a, b) = (cpu.fpr[fs], cpu.fpr[ft]);
    let condition = |cpu: &mut Cpu, c: bool| {
        if c {
            cpu.fcr31 |= 1 << 23;
        } else {
            cpu.fcr31 &= !(1 << 23);
        }
    };
    match insn.op() {
        Op::AddS => cpu.fpr[fd] = float::add(a, b).0,
        Op::SubS => cpu.fpr[fd] = float::sub(a, b).0,
        Op::MulS => cpu.fpr[fd] = float::mul(a, b).0,
        Op::DivS => cpu.fpr[fd] = float::div(a, b).0,
        Op::SqrtS => cpu.fpr[fd] = float::sqrt(b).0,
        Op::RsqrtS => cpu.fpr[fd] = float::rsqrt(a, b).0,
        Op::AbsS => cpu.fpr[fd] = a & 0x7FFF_FFFF,
        Op::NegS => cpu.fpr[fd] = a ^ 0x8000_0000,
        Op::MovS => cpu.fpr[fd] = a,
        Op::AddaS => cpu.fpu_acc = float::add(a, b).0,
        Op::SubaS => cpu.fpu_acc = float::sub(a, b).0,
        Op::MulaS => cpu.fpu_acc = float::mul(a, b).0,
        Op::MaddS => cpu.fpr[fd] = float::madd(cpu.fpu_acc, a, b).0,
        Op::MsubS => cpu.fpr[fd] = float::msub(cpu.fpu_acc, a, b).0,
        Op::MaddaS => cpu.fpu_acc = float::madd(cpu.fpu_acc, a, b).0,
        Op::MsubaS => cpu.fpu_acc = float::msub(cpu.fpu_acc, a, b).0,
        Op::MaxS => cpu.fpr[fd] = float::max(a, b),
        Op::MinS => cpu.fpr[fd] = float::min(a, b),
        Op::CvtWS => cpu.fpr[fd] = float::to_int(a),
        Op::CvtSW => cpu.fpr[fd] = float::from_int(a),
        Op::CFS => condition(cpu, false),
        Op::CEqS => condition(cpu, float::value(a) == float::value(b)),
        Op::CLtS => condition(cpu, float::value(a) < float::value(b)),
        Op::CLeS => condition(cpu, float::value(a) <= float::value(b)),
        _ => {}
    }
}

fn unaligned_load(cpu: &mut Cpu, mem: &mut Memory, op: Op, rt: u8, address: u32) {
    let old = cpu.r64(rt);
    match op {
        Op::Lwl | Op::Lwr => {
            let shift = address & 3;
            let word = mem.read32(address & !3);
            let old = old as u32;
            if op == Op::Lwl {
                let keep = 0x00FF_FFFF_u32.checked_shr(8 * shift).unwrap_or(0);
                cpu.set32(rt, (old & keep) | (word << (24 - 8 * shift)));
            } else {
                let keep = if shift == 0 {
                    0
                } else {
                    u32::MAX << (32 - 8 * shift)
                };
                let value = (old & keep) | (word >> (8 * shift));
                if shift == 0 {
                    cpu.set32(rt, value);
                } else {
                    cpu.set64(rt, (cpu.r64(rt) & !u64::from(u32::MAX)) | u64::from(value));
                }
            }
        }
        _ => {
            let shift = u64::from(address & 7);
            let dword = mem.read64(address & !7);
            if op == Op::Ldl {
                let keep = u64::MAX.checked_shr(8 * (shift as u32 + 1)).unwrap_or(0);
                cpu.set64(rt, (old & keep) | (dword << (56 - 8 * shift)));
            } else {
                let keep = if shift == 0 {
                    0
                } else {
                    u64::MAX << (64 - 8 * shift)
                };
                cpu.set64(rt, (old & keep) | (dword >> (8 * shift)));
            }
        }
    }
}

fn unaligned_store(mem: &mut Memory, op: Op, t: u64, address: u32) {
    match op {
        Op::Swl | Op::Swr => {
            let shift = address & 3;
            let aligned = address & !3;
            let word = mem.read32(aligned);
            let t = t as u32;
            let value = if op == Op::Swl {
                let keep = u32::MAX.checked_shl(8 * (shift + 1)).unwrap_or(0);
                (t >> (24 - 8 * shift)) | (word & keep)
            } else {
                let keep = if shift == 0 {
                    0
                } else {
                    u32::MAX >> (32 - 8 * shift)
                };
                (t << (8 * shift)) | (word & keep)
            };
            mem.write32(aligned, value);
        }
        _ => {
            let shift = address & 7;
            let aligned = address & !7;
            let dword = mem.read64(aligned);
            let value = if op == Op::Sdl {
                let keep = u64::MAX.checked_shl(8 * (shift + 1)).unwrap_or(0);
                (t >> (56 - 8 * shift)) | (dword & keep)
            } else {
                let keep = if shift == 0 {
                    0
                } else {
                    u64::MAX >> (64 - 8 * shift)
                };
                (t << (8 * shift)) | (dword & keep)
            };
            mem.write64(aligned, value);
        }
    }
}
