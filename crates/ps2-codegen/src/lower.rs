//! Lowering of one function: its blocks, their transfers with delay slots,
//! and each instruction.
//!
//! Instruction counts: a block of `n` instructions adds `n` to the clock
//! when it starts (a branch-likely's delay slot is counted on the taken path
//! only). An instruction `k` (from 1) of the block leaves `rem = n - k`
//! instructions after it; exits and the hardware's clock subtract `rem` to
//! stand where the interpreter would.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use ps2_analysis::{Function, Image};
use ps2_isa::ee::{Flow, Format, Insn, Op};

use crate::name;

/// COP0 Status.
pub(crate) const COP0_STATUS: usize = 12;
/// COP0 Count, which reads the clock.
const COP0_COUNT: usize = 9;

/// Where an instruction stands, for its exits.
struct Site {
    pc: u32,
    /// Instructions of the block after this one.
    rem: u64,
    /// Exit before it: where the machine resumes (the branch, for a delay
    /// slot) and how many instructions to take back.
    before: (u32, u64),
    /// The boundary after its step: a constant or the `next` variable.
    after: String,
}

impl Site {
    fn before(&self) -> String {
        format!(
            "{{ c.exit_before(0x{:08x}, {}); return; }}",
            self.before.0, self.before.1
        )
    }

    fn after(&self, why: &str) -> String {
        format!(
            "{{ c.exit_at({}, {}, Exit::{why}); return; }}",
            self.after, self.rem
        )
    }
}

/// How a block ends.
enum End {
    /// Into the next block (or out of the function).
    Fall(u32),
    /// A branch or jump at this address, with its delay slot.
    Transfer(u32, Insn),
    /// An instruction that always exits.
    Stop,
}

fn hex(value: u32) -> String {
    format!("0x{value:08x}")
}

/// Emits the function `function`, split at `leaders`; `local` names the
/// functions of the same module, which are called directly.
pub(crate) fn function(
    out: &mut String,
    image: &Image,
    function: &Function,
    leaders: &BTreeSet<u32>,
    local: &BTreeMap<u32, String>,
) {
    let insn = |at: u32| Insn::decode(image.word(at).unwrap_or(0));
    let _ = writeln!(out, "pub fn {}(c: &mut Ctx<'_>) {{", name(function.start));
    out.push_str("    let mut pc = c.cpu.pc;\n    loop {\n        match pc {\n");
    for &start in leaders {
        let mut body = Vec::new();
        let mut at = start;
        let end = loop {
            if at >= function.end {
                break End::Fall(at);
            }
            let i = insn(at);
            if i.op().flow().has_delay_slot() {
                break End::Transfer(at, i);
            }
            body.push((at, i));
            at += 4;
            if always_exits(i) {
                break End::Stop;
            }
            if leaders.contains(&at) {
                break End::Fall(at);
            }
        };
        let n = body.len() as u64
            + match &end {
                End::Transfer(_, i) if i.op().flow().is_likely() => 1,
                End::Transfer(..) => 2,
                _ => 0,
            };
        let _ = writeln!(out, "            {} => {{", hex(start));
        let _ = writeln!(
            out,
            "                if !c.enter({n}, {}) {{ return; }}",
            hex(start)
        );
        for (k, &(at, i)) in body.iter().enumerate() {
            let rem = n - k as u64 - 1;
            let site = Site {
                pc: at,
                rem,
                before: (at, rem + 1),
                after: hex(at + 4),
            };
            instruction(out, &site, i);
        }
        match end {
            End::Fall(next) => {
                let _ = writeln!(out, "                pc = {}; continue;", hex(next));
            }
            End::Stop => {}
            End::Transfer(at, i) => {
                let rem = n - body.len() as u64 - 1;
                let slot = insn(at + 4);
                transfer(out, function, local, at, i, rem, slot);
            }
        }
        out.push_str("            }\n");
    }
    out.push_str("            _ => { c.leave(pc); return; }\n        }\n    }\n}\n\n");
}

/// Instructions after which the machine always takes over.
fn always_exits(i: Insn) -> bool {
    matches!(i.op().flow(), Flow::Syscall | Flow::Break | Flow::Eret) || i.op() == Op::Invalid
}

/// The condition of a conditional branch, if it is one this lowering knows.
fn condition(i: Insn) -> Option<String> {
    let (rs, rt) = (i.rs(), i.rt());
    let s = format!("c.cpu.r64({rs})");
    let signed = format!("(c.cpu.r64({rs}) as i64)");
    Some(match i.op() {
        Op::Beq | Op::Beql => format!("{s} == c.cpu.r64({rt})"),
        Op::Bne | Op::Bnel => format!("{s} != c.cpu.r64({rt})"),
        Op::Blez | Op::Blezl => format!("{signed} <= 0"),
        Op::Bgtz | Op::Bgtzl => format!("{signed} > 0"),
        Op::Bltz | Op::Bltzl | Op::Bltzal | Op::Bltzall => format!("{signed} < 0"),
        Op::Bgez | Op::Bgezl | Op::Bgezal | Op::Bgezall => format!("{signed} >= 0"),
        Op::Bc0f | Op::Bc0fl => "!c.cpu.cpcond0".to_owned(),
        Op::Bc0t | Op::Bc0tl => "c.cpu.cpcond0".to_owned(),
        Op::Bc1f | Op::Bc1fl => "c.cpu.fcr31 & (1 << 23) == 0".to_owned(),
        Op::Bc1t | Op::Bc1tl => "c.cpu.fcr31 & (1 << 23) != 0".to_owned(),
        // CPCOND2 (VU0 running): macro-mode programs finish at once.
        Op::Bc2f | Op::Bc2fl => "true".to_owned(),
        Op::Bc2t | Op::Bc2tl => "false".to_owned(),
        _ => return None,
    })
}

/// A call to `target` with `cpu.pc` already set: direct within the module,
/// through the table otherwise.
fn call(local: &BTreeMap<u32, String>, target: u32) -> String {
    match local.get(&target) {
        Some(f) => format!("c.call_fn({f});"),
        None => format!("c.call({});", hex(target)),
    }
}

/// Goes to `target`: a block of this function, or a tail call.
fn goto(function: &Function, local: &BTreeMap<u32, String>, target: u32) -> String {
    if (function.start..function.end).contains(&target) {
        format!("pc = {}; continue;", hex(target))
    } else {
        format!(
            "c.cpu.pc = {}; {} return;",
            hex(target),
            call(local, target)
        )
    }
}

/// Calls `target` (pc set) and continues at `ret` if it returned there.
fn call_return(local: &BTreeMap<u32, String>, target: u32, ret: u32) -> String {
    format!(
        "c.cpu.pc = {}; {} if c.exiting || c.cpu.pc != {} {{ return; }} pc = {}; continue;",
        hex(target),
        call(local, target),
        hex(ret),
        hex(ret)
    )
}

/// The branch or jump at `at`, `rem` instructions before the end of its
/// block, and its delay slot.
fn transfer(
    out: &mut String,
    function: &Function,
    local: &BTreeMap<u32, String>,
    at: u32,
    i: Insn,
    rem: u64,
    slot: Insn,
) {
    let flow = i.op().flow();
    let fall = at + 8;
    let slot_site = |after: String| Site {
        pc: at + 4,
        rem: 0,
        before: (at, rem + 1),
        after,
    };
    let emit_slot = |out: &mut String, after: String| instruction(out, &slot_site(after), slot);
    let line = |out: &mut String, text: &str| {
        let _ = writeln!(out, "                {text}");
    };
    match flow {
        Flow::Branch | Flow::BranchLikely | Flow::BranchLink | Flow::BranchLinkLikely => {
            let Some(cond) = condition(i) else {
                line(
                    out,
                    &format!("c.exit_before({}, {}); return;", hex(at), rem + 1),
                );
                return;
            };
            let target = i.branch_target(at);
            line(out, &format!("let taken = {cond};"));
            if flow.links() {
                line(out, &format!("c.cpu.set64(31, {});", hex(fall)));
            }
            let taken_path = if flow.links() {
                call_return(local, target, fall)
            } else {
                goto(function, local, target)
            };
            if flow.is_likely() {
                line(out, "if taken {");
                line(out, "c.cpu.cycles += 1;");
                emit_slot(out, hex(target));
                line(out, &taken_path);
                line(out, "}");
            } else {
                line(
                    out,
                    &format!(
                        "let next: u32 = if taken {{ {} }} else {{ {} }};",
                        hex(target),
                        hex(fall)
                    ),
                );
                emit_slot(out, "next".to_owned());
                line(out, &format!("if taken {{ {taken_path} }}"));
            }
            line(out, &format!("pc = {}; continue;", hex(fall)));
        }
        Flow::Jump => {
            let target = i.jump_target(at);
            emit_slot(out, hex(target));
            line(out, &goto(function, local, target));
        }
        Flow::JumpLink => {
            let target = i.jump_target(at);
            line(out, &format!("c.cpu.set64(31, {});", hex(fall)));
            emit_slot(out, hex(target));
            line(out, &call_return(local, target, fall));
        }
        Flow::JumpReg => {
            line(out, &format!("let next = c.cpu.r32({});", i.rs()));
            emit_slot(out, "next".to_owned());
            if i.rs() == 31 {
                line(out, "c.cpu.pc = next; return;");
            } else {
                line(out, "pc = next; continue;");
            }
        }
        Flow::JumpRegLink => {
            line(out, &format!("let next = c.cpu.r32({});", i.rs()));
            line(out, &format!("c.cpu.set64({}, {});", i.rd(), hex(fall)));
            emit_slot(out, "next".to_owned());
            line(
                out,
                &format!(
                    "c.call(next); if c.exiting || c.cpu.pc != {} {{ return; }} pc = {}; continue;",
                    hex(fall),
                    hex(fall)
                ),
            );
        }
        _ => line(
            out,
            &format!("c.exit_before({}, {}); return;", hex(at), rem + 1),
        ),
    }
}

/// Whether `op` is written out rather than run through `Ctx::exec`.
#[must_use]
pub fn lowered_inline(op: Op) -> bool {
    let site = Site {
        pc: 0,
        rem: 0,
        before: (0, 1),
        after: "0".to_owned(),
    };
    inline(&mut String::new(), &site, Insn::decode(0), op)
}

/// Emits one instruction (not a branch or jump).
fn instruction(out: &mut String, site: &Site, i: Insn) {
    if !inline(out, site, i, i.op()) {
        let line = if matches!(
            i.op().format(),
            Format::RtMem | Format::FtMem | Format::VfMem
        ) {
            format!(
                "{{ let (i, m) = c.exec_mem(0x{:08x}, {}, {}); if i {} if m {} }}",
                i.word(),
                hex(site.pc),
                site.rem,
                site.before(),
                site.after("Mmio")
            )
        } else {
            format!(
                "if c.exec(0x{:08x}, {}) {}",
                i.word(),
                hex(site.pc),
                site.before()
            )
        };
        let _ = writeln!(out, "                {line}");
    }
}

/// Writes `op` (decoded from `i`) out, if this lowering knows it; false
/// otherwise.
#[allow(
    clippy::too_many_lines,
    clippy::match_same_arms,
    reason = "one arm per operation, as in the interpreter"
)]
fn inline(out: &mut String, site: &Site, i: Insn, op: Op) -> bool {
    let (rs, rt, rd, sa) = (i.rs(), i.rt(), i.rd(), i.sa());
    let imm32 = i32::from(i.simm()).cast_unsigned();
    let imm64 = i64::from(i.simm()).cast_unsigned();
    let uimm = i.imm();
    let s32 = format!("c.cpu.r32({rs})");
    let t32 = format!("c.cpu.r32({rt})");
    let s64 = format!("c.cpu.r64({rs})");
    let t64 = format!("c.cpu.r64({rt})");
    let address = if imm32 == 0 {
        s32.clone()
    } else {
        format!("{s32}.wrapping_add(0x{imm32:x})")
    };
    let rem = site.rem;
    let load = |set: &str| format!("{{ let v = c.{}({address}, {rem}); {set}; }}", load_fn(op));
    let store = |value: &str, aligned: &str| {
        format!(
            "{{ let a = {aligned}; let v = {value}; if c.{}(a, v, {rem}) {} }}",
            store_fn(op),
            site.after("Mmio")
        )
    };
    let fpu = |expr: &str| format!("c.cpu.fpr[{sa}] = {expr};");
    let (fs, ft) = (rd, rt);
    let fa = format!("c.cpu.fpr[{fs}]");
    let fb = format!("c.cpu.fpr[{ft}]");
    let compare = |cmp: &str| {
        format!(
            "if fl::value({fa}) {cmp} fl::value({fb}) {{ c.cpu.fcr31 |= 1 << 23; }} else {{ c.cpu.fcr31 &= !(1 << 23); }}"
        )
    };
    let trap = |cond: String| format!("if {cond} {}", site.before());
    let text = match op {
        Op::Nop
        | Op::Sync
        | Op::SyncP
        | Op::Cache
        | Op::Pref
        | Op::Tlbr
        | Op::Tlbwi
        | Op::Tlbwr
        | Op::Tlbp => return true,

        // ALU, immediate.
        Op::Addi | Op::Addiu => format!("c.cpu.set32({rt}, {s32}.wrapping_add(0x{imm32:x}));"),
        Op::Daddi | Op::Daddiu => format!("c.cpu.set64({rt}, {s64}.wrapping_add(0x{imm64:x}));"),
        Op::Slti => format!(
            "c.cpu.set64({rt}, u64::from(({s64} as i64) < {}));",
            i.simm()
        ),
        Op::Sltiu => format!("c.cpu.set64({rt}, u64::from({s64} < 0x{imm64:x}));"),
        Op::Andi => format!("c.cpu.set64({rt}, {s64} & 0x{uimm:x});"),
        Op::Ori => format!("c.cpu.set64({rt}, {s64} | 0x{uimm:x});"),
        Op::Xori => format!("c.cpu.set64({rt}, {s64} ^ 0x{uimm:x});"),
        Op::Lui => format!("c.cpu.set64({rt}, 0x{:x});", imm64 << 16),

        // ALU, registers.
        Op::Add | Op::Addu => format!("c.cpu.set32({rd}, {s32}.wrapping_add({t32}));"),
        Op::Sub | Op::Subu => format!("c.cpu.set32({rd}, {s32}.wrapping_sub({t32}));"),
        Op::Dadd | Op::Daddu => format!("c.cpu.set64({rd}, {s64}.wrapping_add({t64}));"),
        Op::Dsub | Op::Dsubu => format!("c.cpu.set64({rd}, {s64}.wrapping_sub({t64}));"),
        Op::And => format!("c.cpu.set64({rd}, {s64} & {t64});"),
        Op::Or => format!("c.cpu.set64({rd}, {s64} | {t64});"),
        Op::Xor => format!("c.cpu.set64({rd}, {s64} ^ {t64});"),
        Op::Nor => format!("c.cpu.set64({rd}, !({s64} | {t64}));"),
        Op::Slt => format!("c.cpu.set64({rd}, u64::from(({s64} as i64) < ({t64} as i64)));"),
        Op::Sltu => format!("c.cpu.set64({rd}, u64::from({s64} < {t64}));"),
        Op::Movz => format!("if {t64} == 0 {{ c.cpu.set64({rd}, {s64}); }}"),
        Op::Movn => format!("if {t64} != 0 {{ c.cpu.set64({rd}, {s64}); }}"),

        // Shifts.
        Op::Sll => format!("c.cpu.set32({rd}, {t32} << {sa});"),
        Op::Srl => format!("c.cpu.set32({rd}, {t32} >> {sa});"),
        Op::Sra => format!("c.cpu.set32({rd}, (({t32} as i32) >> {sa}) as u32);"),
        Op::Sllv => format!("c.cpu.set32({rd}, {t32} << ({s32} & 31));"),
        Op::Srlv => format!("c.cpu.set32({rd}, {t32} >> ({s32} & 31));"),
        Op::Srav => format!("c.cpu.set32({rd}, (({t32} as i32) >> ({s32} & 31)) as u32);"),
        Op::Dsll => format!("c.cpu.set64({rd}, {t64} << {sa});"),
        Op::Dsrl => format!("c.cpu.set64({rd}, {t64} >> {sa});"),
        Op::Dsra => format!("c.cpu.set64({rd}, (({t64} as i64) >> {sa}) as u64);"),
        Op::Dsll32 => format!("c.cpu.set64({rd}, {t64} << {});", u32::from(sa) + 32),
        Op::Dsrl32 => format!("c.cpu.set64({rd}, {t64} >> {});", u32::from(sa) + 32),
        Op::Dsra32 => format!(
            "c.cpu.set64({rd}, (({t64} as i64) >> {}) as u64);",
            u32::from(sa) + 32
        ),
        Op::Dsllv => format!("c.cpu.set64({rd}, {t64} << ({s64} & 63));"),
        Op::Dsrlv => format!("c.cpu.set64({rd}, {t64} >> ({s64} & 63));"),
        Op::Dsrav => format!("c.cpu.set64({rd}, (({t64} as i64) >> ({s64} & 63)) as u64);"),

        // HI and LO.
        Op::Mfhi => format!("c.cpu.set64({rd}, c.cpu.hi as u64);"),
        Op::Mflo => format!("c.cpu.set64({rd}, c.cpu.lo as u64);"),
        Op::Mthi => format!("c.cpu.hi = (c.cpu.hi & !u128::from(u64::MAX)) | u128::from({s64});"),
        Op::Mtlo => format!("c.cpu.lo = (c.cpu.lo & !u128::from(u64::MAX)) | u128::from({s64});"),

        // Loads.
        Op::Lb => load(&format!("c.cpu.set64({rt}, v as i8 as i64 as u64)")),
        Op::Lbu => load(&format!("c.cpu.set64({rt}, u64::from(v))")),
        Op::Lh => load(&format!("c.cpu.set64({rt}, v as i16 as i64 as u64)")),
        Op::Lhu => load(&format!("c.cpu.set64({rt}, u64::from(v))")),
        Op::Lw => load(&format!("c.cpu.set32({rt}, v)")),
        Op::Lwu => load(&format!("c.cpu.set64({rt}, u64::from(v))")),
        Op::Ld => load(&format!("c.cpu.set64({rt}, v)")),
        Op::Lwc1 => load(&format!("c.cpu.fpr[{rt}] = v")),
        Op::Lq => format!("{{ let v = c.lq(({address}) & !15, {rem}); c.cpu.set128({rt}, v); }}"),
        Op::Lqc2 => format!(
            "{{ let v = c.lq(({address}) & !15, {rem}); ps2_core::vu0::set_vf(c.cpu, {rt}, v); }}"
        ),

        // Stores.
        Op::Sb => store(&format!("{t64} as u8"), &address),
        Op::Sh => store(&format!("{t64} as u16"), &address),
        Op::Sw => store(&t32, &address),
        Op::Sd => store(&t64, &address),
        Op::Swc1 => store(&format!("c.cpu.fpr[{rt}]"), &address),
        Op::Sq => store(&format!("c.cpu.gpr[{rt}]"), &format!("({address}) & !15")),
        Op::Sqc2 => store(
            &format!("ps2_core::vu0::vf(c.cpu, {rt})"),
            &format!("({address}) & !15"),
        ),

        // Traps.
        Op::Tge => trap(format!("({s64} as i64) >= ({t64} as i64)")),
        Op::Tgeu => trap(format!("{s64} >= {t64}")),
        Op::Tlt => trap(format!("({s64} as i64) < ({t64} as i64)")),
        Op::Tltu => trap(format!("{s64} < {t64}")),
        Op::Teq => trap(format!("{s64} == {t64}")),
        Op::Tne => trap(format!("{s64} != {t64}")),
        Op::Tgei => trap(format!("({s64} as i64) >= {}", i.simm())),
        Op::Tgeiu => trap(format!("{s64} >= 0x{imm64:x}")),
        Op::Tlti => trap(format!("({s64} as i64) < {}", i.simm())),
        Op::Tltiu => trap(format!("{s64} < 0x{imm64:x}")),
        Op::Teqi => trap(format!("{s64} == 0x{imm64:x}")),
        Op::Tnei => trap(format!("{s64} != 0x{imm64:x}")),

        // COP0.
        Op::Mfc0 if usize::from(rd) == COP0_COUNT => {
            format!("c.cpu.set32({rt}, (c.cpu.cycles - {rem}) as u32);")
        }
        Op::Mfc0 => format!("c.cpu.set32({rt}, c.cpu.cop0[{rd}]);"),
        Op::Mtc0 if usize::from(rd) == COP0_STATUS => {
            format!("c.cpu.cop0[{rd}] = {t32}; {}", site.after("Interrupts"))
        }
        Op::Mtc0 => format!("c.cpu.cop0[{rd}] = {t32};"),
        Op::Ei => format!(
            "c.cpu.cop0[{COP0_STATUS}] |= 1 << 16; {}",
            site.after("Interrupts")
        ),
        Op::Di => format!("c.cpu.cop0[{COP0_STATUS}] &= !(1 << 16);"),

        // COP1.
        Op::Mfc1 => format!("c.cpu.set32({rt}, c.cpu.fpr[{rd}]);"),
        Op::Mtc1 => format!("c.cpu.fpr[{rd}] = {t32};"),
        Op::Cfc1 if rd == 31 => format!("c.cpu.set32({rt}, c.cpu.fcr31);"),
        Op::Cfc1 => format!("c.cpu.set32({rt}, 0x2e00);"),
        Op::Ctc1 if rd == 31 => format!("c.cpu.fcr31 = {t32};"),
        Op::Ctc1 => return true,
        Op::AddS => fpu(&format!("fl::add({fa}, {fb}).0")),
        Op::SubS => fpu(&format!("fl::sub({fa}, {fb}).0")),
        Op::MulS => fpu(&format!("fl::mul({fa}, {fb}).0")),
        Op::DivS => fpu(&format!("fl::div({fa}, {fb}).0")),
        Op::SqrtS => fpu(&format!("fl::sqrt({fb}).0")),
        Op::RsqrtS => fpu(&format!("fl::rsqrt({fa}, {fb}).0")),
        Op::AbsS => fpu(&format!("{fa} & 0x7fff_ffff")),
        Op::NegS => fpu(&format!("{fa} ^ 0x8000_0000")),
        Op::MovS => fpu(&fa),
        Op::AddaS => format!("c.cpu.fpu_acc = fl::add({fa}, {fb}).0;"),
        Op::SubaS => format!("c.cpu.fpu_acc = fl::sub({fa}, {fb}).0;"),
        Op::MulaS => format!("c.cpu.fpu_acc = fl::mul({fa}, {fb}).0;"),
        Op::MaddS => fpu(&format!("fl::madd(c.cpu.fpu_acc, {fa}, {fb}).0")),
        Op::MsubS => fpu(&format!("fl::msub(c.cpu.fpu_acc, {fa}, {fb}).0")),
        Op::MaddaS => format!("c.cpu.fpu_acc = fl::madd(c.cpu.fpu_acc, {fa}, {fb}).0;"),
        Op::MsubaS => format!("c.cpu.fpu_acc = fl::msub(c.cpu.fpu_acc, {fa}, {fb}).0;"),
        Op::MaxS => fpu(&format!("fl::max({fa}, {fb})")),
        Op::MinS => fpu(&format!("fl::min({fa}, {fb})")),
        Op::CvtWS => fpu(&format!("fl::to_int({fa})")),
        Op::CvtSW => fpu(&format!("fl::from_int({fa})")),
        Op::CFS => "c.cpu.fcr31 &= !(1 << 23);".to_owned(),
        Op::CEqS => compare("=="),
        Op::CLtS => compare("<"),
        Op::CLeS => compare("<="),
        _ => return false,
    };
    let _ = writeln!(out, "                {text}");
    true
}

fn load_fn(op: Op) -> &'static str {
    match op {
        Op::Lb | Op::Lbu => "lb",
        Op::Lh | Op::Lhu => "lh",
        Op::Ld => "ld",
        _ => "lw",
    }
}

fn store_fn(op: Op) -> &'static str {
    match op {
        Op::Sb => "sb",
        Op::Sh => "sh",
        Op::Sd => "sd",
        Op::Sq | Op::Sqc2 => "sq",
        _ => "sw",
    }
}
