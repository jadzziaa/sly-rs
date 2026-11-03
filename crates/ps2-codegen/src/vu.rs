//! Translation of VU microcode overlays into Rust that runs on `ps2-vu`'s
//! interface (`ps2_vu::micro`): each pair becomes the statements it needs,
//! with the VU's registers and pipeline in locals.
//!
//! The overlays `dvp-ld` linked as one program (same id, consecutive
//! addresses) are cut into pieces of about 128 pairs, preferably
//! where control does not fall through, but not inside an inner loop (from
//! a backward branch's target to the branch): leaving a piece hands the
//! state back, which a loop should not do every time round. A piece's
//! pairs are split into basic blocks (leaders: the piece's first pair,
//! branch targets, the pair after a delay slot or a program's last pair,
//! and the pair after one the generator leaves to the interpreter). A
//! piece is a function: a loop over a `match` on the block to run, entered
//! only at a leader (`Translation::entries`), with everything the
//! interpreter keeps in `micro::State` in locals:
//!
//! - the registers (a copy the compiler breaks into scalars);
//! - the cycle, which a block advances by one a pair;
//! - Q's and P's results under way, landed where they are read (Q and the
//!   status flags, P), as the interpreter lands them at the start of the
//!   pair that reads them;
//! - the flags as `micro::State` carries them: four snapshots, visible
//!   from this cycle and the next three. A pair that sets flags makes the
//!   last one; every pair shifts them by one. Within a block the shift is
//!   a renaming of locals; between blocks it costs a few moves.
//!
//! A pair that waits (WAITQ, WAITP, or DIV or an EFU instruction while
//! the unit is busy) moves the cycle and the flags on by the cycles waited.
//! The generator leaves to the interpreter what it does not handle: FSSET
//! (Sly 1 never runs one), and a branch or E bit next to a branch or E
//! bit; a piece leaves its code there and the interpreter goes on to the
//! next leader. Semantics are the interpreter's: every operation comes from
//! `ps2_vu::micro`'s functions, which `exec` uses too, and `vu-bench`
//! checks translated against interpreted programs.
//!
//! At run time a translation is used only where micro memory holds its code
//! byte for byte; anything else runs in the interpreter.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use ps2_analysis::Overlay;
use ps2_isa::vu::{FLAG_E, FLAG_I, LowerOp, Pair};
use ps2_vu::micro::{Arith, Operand, Upper, upper_class};

/// Pairs a piece aims for (see [`pieces`]).
const PIECE: usize = 128;

/// Overlays that make one program: consecutive code with one overlay id.
struct Program {
    address: u32,
    code: Vec<u8>,
    names: Vec<String>,
}

/// The overlay id in a `dvp-ld` section name
/// (`.DVP.overlay..<vu address>.<id>.<n>.<part>`).
fn overlay_id(name: &str) -> Option<&str> {
    name.strip_prefix(".DVP.overlay..")?.split('.').nth(1)
}

fn programs(overlays: &[Overlay]) -> Vec<Program> {
    let mut programs: Vec<Program> = Vec::new();
    for overlay in overlays {
        if overlay.code.is_empty() {
            continue;
        }
        if let Some(last) = programs.last_mut()
            && overlay_id(&overlay.name).is_some()
            && last.names.last().map(|n| overlay_id(n)) == Some(overlay_id(&overlay.name))
            && last.address as usize + last.code.len() == overlay.vu_address as usize
        {
            last.code.extend_from_slice(&overlay.code);
            last.names.push(overlay.name.clone());
            continue;
        }
        programs.push(Program {
            address: overlay.vu_address,
            code: overlay.code.clone(),
            names: vec![overlay.name.clone()],
        });
    }
    programs
}

/// A piece of a program: pairs `start..end` of its code.
struct Piece<'a> {
    program: &'a Program,
    start: usize,
    end: usize,
}

/// Whether control never falls through from pair `k` to the next: it ends
/// a program, or is the delay slot of an unconditional jump.
fn no_fall_through(pairs: &[Pair], k: usize) -> bool {
    let after_end = k > 0 && pairs[k - 1].upper() & FLAG_E != 0;
    let jump = |p: &Pair| {
        p.upper() & FLAG_I == 0
            && matches!(
                p.lower_op(),
                LowerOp::B | LowerOp::Bal | LowerOp::Jr | LowerOp::Jalr
            )
    };
    after_end || (k > 0 && jump(&pairs[k - 1]))
}

/// One pair of a piece.
#[derive(Clone, Copy)]
struct Op {
    pc: u32,
    pair: Pair,
    class: Upper,
}

impl Op {
    fn lower_op(self) -> Option<LowerOp> {
        (self.pair.upper() & FLAG_I == 0).then(|| self.pair.lower_op())
    }

    fn jumps(self) -> bool {
        matches!(
            self.lower_op(),
            Some(
                LowerOp::B
                    | LowerOp::Bal
                    | LowerOp::Jr
                    | LowerOp::Jalr
                    | LowerOp::Ibeq
                    | LowerOp::Ibne
                    | LowerOp::Ibltz
                    | LowerOp::Ibgtz
                    | LowerOp::Iblez
                    | LowerOp::Ibgez
            )
        )
    }

    fn ends(self) -> bool {
        self.pair.upper() & FLAG_E != 0
    }

    /// A static branch target (byte address, before wrapping).
    fn target(self) -> Option<u32> {
        (self.jumps() && !matches!(self.lower_op(), Some(LowerOp::Jr | LowerOp::Jalr)))
            .then(|| self.pair.branch_target(self.pc))
    }
}

/// How a block ends.
enum End {
    /// Control goes on to the pair at this byte address.
    Fall(u32),
    /// The branch at this index decides, after its delay slot.
    Branch(usize),
    /// The program ends.
    Program,
}

/// A piece cut into blocks.
struct Blocks {
    ops: Vec<Op>,
    /// Leaders, and each block's pairs (indices into `ops`) and end.
    blocks: Vec<(usize, Vec<usize>, End)>,
}

fn blocks(p: &Piece<'_>) -> Blocks {
    let code = &p.program.code[8 * p.start..8 * p.end];
    let address = p.program.address + 8 * u32::try_from(p.start).unwrap_or(0);
    let ops: Vec<Op> = (address..)
        .step_by(8)
        .zip(code.as_chunks::<8>().0)
        .map(|(pc, b)| {
            let word = |at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
            let pair = Pair::from_words(word(4), word(0));
            Op {
                pc,
                pair,
                class: upper_class(pair.upper_op()),
            }
        })
        .collect();
    let n = ops.len();
    let plain = |k: usize| ops[k].lower_op() != Some(LowerOp::Fsset);
    // A branch's delay slot, or the pair after an E bit, runs in the same
    // block: it must be plain and neither branch nor (for a delay slot)
    // end the program.
    let supported: Vec<bool> = (0..n)
        .map(|k| {
            let o = ops[k];
            let next_ok = |end_ok: bool| {
                k + 1 < n && plain(k + 1) && !ops[k + 1].jumps() && (end_ok || !ops[k + 1].ends())
            };
            plain(k)
                && (!o.jumps() || (!o.ends() && next_ok(false)))
                && (!o.ends() || o.jumps() || next_ok(true))
        })
        .collect();
    let index = |pc: u32| -> Option<usize> {
        let first = ops.first()?.pc;
        let k = usize::try_from(pc.checked_sub(first)? / 8).ok()?;
        (pc.is_multiple_of(8) && k < n).then_some(k)
    };
    let mut leaders = BTreeSet::new();
    for k in 0..n {
        if !supported[k] {
            continue;
        }
        let after = |d: usize, f: fn(Op) -> bool| k >= d && f(ops[k - d]);
        if k == 0 || !supported[k - 1] || after(2, Op::jumps) || after(2, Op::ends) {
            leaders.insert(k);
        }
    }
    for k in 0..n {
        if supported[k]
            && let Some(t) = ops[k].target().and_then(index)
            && supported[t]
        {
            leaders.insert(t);
        }
    }
    let blocks = leaders
        .iter()
        .map(|&leader| {
            let mut pairs = Vec::new();
            let mut k = leader;
            let end = loop {
                pairs.push(k);
                if ops[k].jumps() {
                    pairs.push(k + 1);
                    break End::Branch(k);
                }
                if ops[k].ends() {
                    pairs.push(k + 1);
                    break End::Program;
                }
                k += 1;
                if k >= n || !supported[k] || leaders.contains(&k) {
                    break End::Fall(ops[k - 1].pc + 8);
                }
            };
            (leader, pairs, end)
        })
        .collect();
    Blocks { ops, blocks }
}

/// A vector register as an expression (`vf0` reads as a constant: pieces
/// check that it holds it).
fn vf(n: u32) -> String {
    if n == 0 {
        "VF0".to_owned()
    } else {
        format!("r.vf[{n}]")
    }
}

/// An integer register as an expression (`vi0` is zero, checked likewise).
fn vi(n: u32) -> String {
    if n == 0 {
        "0u32".to_owned()
    } else {
        format!("r.vi[{n}]")
    }
}

/// The statements for one pair.
#[derive(Default)]
struct PairCode {
    /// Before anything: hand over to the interpreter, land what is read.
    top: String,
    /// Results, from the registers as they were before the pair.
    compute: String,
    /// Register writes, then stores and I and R.
    writes: String,
    /// The flags it sets, as changes to `next`.
    flags: String,
    /// After the flags.
    after: String,
}

/// Takes the result under way in `unit` (`q` or `p`), waiting for it if
/// it is late: the flags move on by the cycles waited, as
/// `micro::State::advance` moves them. `land` lands it (on `x`).
fn wait(unit: &str, land: &str) -> String {
    format!(
        "        if let Some(x) = {unit}.take() {{\n            \
         {land}\n            \
         if x.ready > cycle {{\n                \
         match x.ready - cycle {{\n                    \
         1 => (w0, w1, w2) = (w1, w2, w3),\n                    \
         2 => (w0, w1, w2) = (w2, w3, w3),\n                    \
         _ => (w0, w1, w2) = (w3, w3, w3),\n                \
         }}\n                \
         cycle = x.ready;\n            }}\n        }}\n"
    )
}

const TAKE_Q: &str = "land_q(x, &mut r);";
const TAKE_P: &str = "r.p = x.value;";

const LAND_Q: &str = "        if let Some(x) = q\n            && x.ready <= cycle\n        {\n            \
                      land_q(x, &mut r);\n            q = None;\n        }\n";
const LAND_P: &str = "        if let Some(x) = p\n            && x.ready <= cycle\n        {\n            \
                      r.p = x.value;\n            p = None;\n        }\n";

#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "one arm per operation, with the manual's field names"
)]
fn upper(o: Op, c: &mut PairCode) {
    let w = o.pair.upper();
    let (dest, ft, fs, fd, bc) = (
        (w >> 21) & 0xF,
        (w >> 16) & 31,
        (w >> 11) & 31,
        (w >> 6) & 31,
        w & 3,
    );
    let write = |target: &str, reg: u32| -> String {
        if target == "acc" {
            format!("        r.acc = merge(r.acc, u, 0x{dest:x});\n")
        } else if reg == 0 {
            String::new()
        } else {
            format!("        r.vf[{reg}] = merge(r.vf[{reg}], u, 0x{dest:x});\n")
        }
    };
    let set_flags = "        next = next.with_mac(umac, ucur);\n";
    match o.class {
        Upper::Arith(arith, operand, to_acc) => {
            let (a, t) = (vf(fs), vf(ft));
            let b = match operand {
                Operand::Ft => t,
                Operand::Bc => format!("[{t}[{bc}]; 4]"),
                Operand::Q => {
                    c.top.push_str(LAND_Q);
                    "[r.q; 4]".to_owned()
                }
                Operand::I => "[r.i; 4]".to_owned(),
            };
            let old = if to_acc { "r.acc".to_owned() } else { vf(fd) };
            let result = match arith {
                Arith::Add => format!("lanes::add({a}, {b})"),
                Arith::Sub => format!("lanes::sub({a}, {b})"),
                Arith::Mul => format!("lanes::mul({a}, {b})"),
                Arith::Madd => format!("lanes::madd(r.acc, {a}, {b})"),
                Arith::Msub => format!("lanes::msub(r.acc, {a}, {b})"),
                Arith::Max => format!("float::x4::max({a}, {b})"),
                Arith::Min => format!("float::x4::min({a}, {b})"),
            };
            let _ = writeln!(
                c.compute,
                "        let u = merge({old}, {result}, 0x{dest:x});"
            );
            if !matches!(arith, Arith::Max | Arith::Min) {
                let _ = writeln!(
                    c.compute,
                    "        let (umac, ucur) = flags(u, 0x{dest:x});"
                );
                c.flags.push_str(set_flags);
            }
            c.writes
                .push_str(&write(if to_acc { "acc" } else { "vf" }, fd));
        }
        Upper::Opmula | Upper::Opmsub => {
            let mula = o.class == Upper::Opmula;
            let (a, b) = (vf(fs), vf(ft));
            let _ = writeln!(
                c.compute,
                "        let (x, y) = ({{ let a = {a}; [a[1], a[2], a[0], 0] }}, {{ let b = {b}; [b[2], b[0], b[1], 0] }});"
            );
            let terms = if mula {
                "lanes::mul(x, y)"
            } else {
                "lanes::msub(r.acc, x, y)"
            };
            let old = if mula { "r.acc".to_owned() } else { vf(fd) };
            let _ = writeln!(
                c.compute,
                "        let u = merge({old}, {terms}, 0x{:x});\n        \
                 let (umac, ucur) = flags(u, 0x{dest:x});",
                dest & 0xE
            );
            c.flags.push_str(set_flags);
            c.writes
                .push_str(&write(if mula { "acc" } else { "vf" }, fd));
        }
        Upper::Itof(n) | Upper::Ftoi(n) => {
            let f = if matches!(o.class, Upper::Itof(_)) {
                "from_fixed"
            } else {
                "to_fixed"
            };
            let _ = writeln!(
                c.compute,
                "        let u = float::x4::{f}({}, {n});",
                vf(fs)
            );
            c.writes.push_str(&write("vf", ft));
        }
        Upper::Abs => {
            let _ = writeln!(
                c.compute,
                "        let u = {{ let a = {}; [a[0] & 0x7FFF_FFFF, a[1] & 0x7FFF_FFFF, a[2] & 0x7FFF_FFFF, a[3] & 0x7FFF_FFFF] }};",
                vf(fs)
            );
            c.writes.push_str(&write("vf", ft));
        }
        Upper::Clip => {
            let _ = writeln!(
                c.compute,
                "        let uclip = clip(w3.clip, {}, {}[3]);",
                vf(fs),
                vf(ft)
            );
            c.flags.push_str("        next.clip = uclip;\n");
        }
        Upper::Nop => {}
    }
}

/// The lower half; what the block's branch decides is `go` (a bool, or the
/// target for JR and JALR).
#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::similar_names,
    reason = "one arm per operation, with the manual's field names"
)]
fn lower(o: Op, c: &mut PairCode) {
    use LowerOp as L;
    let w = o.pair.lower();
    let Some(op) = o.lower_op() else {
        let _ = writeln!(c.writes, "        r.i = 0x{w:08x};");
        return;
    };
    let (it, is, id) = ((w >> 16) & 15, (w >> 11) & 15, (w >> 6) & 15);
    let (lft, lfs) = ((w >> 16) & 31, (w >> 11) & 31);
    let ldest = (w >> 21) & 0xF;
    let imm11 = (((w & 0x7FF) << 21).cast_signed() >> 21).cast_unsigned();
    let imm15 = (w & 0x7FF) | ((w >> 10) & 0x7800);
    let imm5 = ((((w >> 6) & 31) << 27).cast_signed() >> 27).cast_unsigned();
    let fsf = (w >> 21) & 3;
    let ftf = (w >> 23) & 3;
    let imm24 = w & 0xFF_FFFF;
    let imm12 = (w & 0x7FF) | ((w >> 10) & 0x800);
    let pc = o.pc;
    let link = ((pc + 16) / 8) & 0xFFFF;
    let compute = &mut c.compute;
    let writes = &mut c.writes;
    let set_vf = |writes: &mut String, reg: u32, value: &str| {
        if reg != 0 {
            let _ = writeln!(
                writes,
                "        r.vf[{reg}] = merge(r.vf[{reg}], {value}, 0x{ldest:x});"
            );
        }
    };
    let set_vi = |writes: &mut String, reg: u32, value: &str| {
        if reg != 0 {
            let _ = writeln!(writes, "        r.vi[{reg}] = {value} & 0xFFFF;");
        }
    };
    let store = |c: &mut String, at: &str, value: &str| {
        let _ = writeln!(c, "        store_q(data, {at}, {value}, 0x{ldest:x});");
    };
    match op {
        L::Lq => {
            let _ = writeln!(
                compute,
                "        let lv = load_q(data, {}.wrapping_add(0x{imm11:x}));",
                vi(is)
            );
            set_vf(writes, lft, "lv");
        }
        L::Sq => {
            let _ = writeln!(
                compute,
                "        let (lv, la) = ({}, {}.wrapping_add(0x{imm11:x}));",
                vf(lfs),
                vi(it)
            );
            store(writes, "la", "lv");
        }
        L::Lqi => {
            let _ = writeln!(
                compute,
                "        let (lv, li) = (load_q(data, {0}), {0}.wrapping_add(1));",
                vi(is)
            );
            set_vf(writes, lft, "lv");
            set_vi(writes, is, "li");
        }
        L::Lqd => {
            let _ = writeln!(
                compute,
                "        let li = {}.wrapping_sub(1);\n        let lv = load_q(data, li);",
                vi(is)
            );
            set_vf(writes, lft, "lv");
            set_vi(writes, is, "li");
        }
        L::Sqi => {
            let _ = writeln!(
                compute,
                "        let (lv, la, li) = ({}, {1}, {1}.wrapping_add(1));",
                vf(lfs),
                vi(it)
            );
            set_vi(writes, it, "li");
            store(writes, "la", "lv");
        }
        L::Sqd => {
            let _ = writeln!(
                compute,
                "        let li = {}.wrapping_sub(1);\n        let (lv, la) = ({}, li);",
                vi(it),
                vf(lfs)
            );
            set_vi(writes, it, "li");
            store(writes, "la", "lv");
        }
        L::Ilw | L::Ilwr => {
            let offset = if op == L::Ilw { imm11 } else { 0 };
            let field = (0..4).find(|&i| ldest & (8 >> i) != 0).unwrap_or(0);
            let _ = writeln!(
                compute,
                "        let li = load_q(data, {}.wrapping_add(0x{offset:x}))[{field}];",
                vi(is)
            );
            set_vi(writes, it, "li");
        }
        L::Isw | L::Iswr => {
            let offset = if op == L::Isw { imm11 } else { 0 };
            let _ = writeln!(
                compute,
                "        let (lv, la) = ([{} & 0xFFFF; 4], {}.wrapping_add(0x{offset:x}));",
                vi(it),
                vi(is)
            );
            store(writes, "la", "lv");
        }
        L::Iadd | L::Isub | L::Iand | L::Ior | L::Iaddi | L::Iaddiu | L::Isubiu => {
            let (s, t) = (vi(is), vi(it));
            let (value, reg) = match op {
                L::Iadd => (format!("{s}.wrapping_add({t})"), id),
                L::Isub => (format!("{s}.wrapping_sub({t})"), id),
                L::Iand => (format!("{s} & {t}"), id),
                L::Ior => (format!("{s} | {t}"), id),
                L::Iaddi => (format!("{s}.wrapping_add(0x{imm5:x})"), it),
                L::Iaddiu => (format!("{s}.wrapping_add(0x{imm15:x})"), it),
                _ => (format!("{s}.wrapping_sub(0x{imm15:x})"), it),
            };
            let _ = writeln!(compute, "        let li = {value};");
            set_vi(writes, reg, "li");
        }
        L::Move => {
            let _ = writeln!(compute, "        let lv = {};", vf(lfs));
            set_vf(writes, lft, "lv");
        }
        L::Mr32 => {
            let _ = writeln!(
                compute,
                "        let lv = {{ let a = {}; [a[1], a[2], a[3], a[0]] }};",
                vf(lfs)
            );
            set_vf(writes, lft, "lv");
        }
        L::Mtir => {
            let _ = writeln!(compute, "        let li = {}[{fsf}];", vf(lfs));
            set_vi(writes, it, "li");
        }
        L::Mfir => {
            let _ = writeln!(
                compute,
                "        let lv = [i32::from({} as u16 as i16) as u32; 4];",
                vi(is)
            );
            set_vf(writes, lft, "lv");
        }
        L::Div | L::Sqrt | L::Rsqrt => {
            // A division waits for the one under way, after the upper half
            // has read Q.
            compute.push_str(&wait("q", TAKE_Q));
            let _ = writeln!(
                compute,
                "        q = Some(divide(LowerOp::{op:?}, {}[{fsf}], {}[{ftf}], cycle));",
                vf(lfs),
                vf(lft)
            );
        }
        // WAITQ and WAITP wait before anything reads: the upper half sees
        // the result too.
        L::Waitq => c.top.push_str(&wait("q", TAKE_Q)),
        L::Waitp => c.top.push_str(&wait("p", TAKE_P)),
        L::Mfp => {
            c.top.push_str(LAND_P);
            let _ = writeln!(compute, "        let lv = [r.p; 4];");
            set_vf(writes, lft, "lv");
        }
        L::Esadd
        | L::Ersadd
        | L::Eleng
        | L::Erleng
        | L::Eatanxy
        | L::Eatanxz
        | L::Esum
        | L::Esqrt
        | L::Ersqrt
        | L::Ercpr
        | L::Esin
        | L::Eatan
        | L::Eexp => {
            compute.push_str(&wait("p", TAKE_P));
            let _ = writeln!(
                compute,
                "        p = Some(efu_at(LowerOp::{op:?}, {0}, {0}[{fsf}], cycle));",
                vf(lfs)
            );
        }
        L::Rinit | L::Rxor | L::Rnext => {
            let value = match op {
                L::Rinit => format!("0x3F80_0000 | ({}[{fsf}] & 0x007F_FFFF)", vf(lfs)),
                L::Rxor => format!("0x3F80_0000 | ((r.r ^ {}[{fsf}]) & 0x007F_FFFF)", vf(lfs)),
                _ => "rnext(r.r)".to_owned(),
            };
            let _ = writeln!(compute, "        let lr = {value};");
            if op == L::Rnext {
                set_vf(writes, lft, "[lr; 4]");
            }
            writes.push_str("        r.r = lr;\n");
        }
        L::Rget => {
            let _ = writeln!(compute, "        let lv = [r.r; 4];");
            set_vf(writes, lft, "lv");
        }
        L::Fceq | L::Fcand | L::Fcor => {
            let value = match op {
                L::Fceq => format!("u32::from((w0.clip & 0xFF_FFFF) == 0x{imm24:x})"),
                L::Fcand => format!("u32::from(w0.clip & 0x{imm24:x} != 0)"),
                _ => format!("u32::from((w0.clip | 0x{imm24:x}) & 0xFF_FFFF == 0xFF_FFFF)"),
            };
            let _ = writeln!(compute, "        let li = {value};");
            set_vi(writes, 1, "li");
        }
        L::Fcset => {
            let _ = writeln!(c.flags, "        next.clip = 0x{imm24:x};");
        }
        L::Fcget => {
            let _ = writeln!(compute, "        let li = w0.clip & 0xFFF;");
            set_vi(writes, it, "li");
        }
        L::Fseq | L::Fsand | L::Fsor => {
            c.top.push_str(LAND_Q);
            let st = "((w0.status | (r.status & DIVIDER_STATUS)) & 0xFFF)";
            let value = match op {
                L::Fseq => format!("u32::from({st} == 0x{imm12:x})"),
                L::Fsand => format!("{st} & 0x{imm12:x}"),
                _ => format!("{st} | 0x{imm12:x}"),
            };
            let _ = writeln!(compute, "        let li = {value};");
            set_vi(writes, it, "li");
        }
        L::Fmeq | L::Fmand | L::Fmor => {
            let s = vi(is);
            let value = match op {
                L::Fmeq => format!("u32::from(w0.mac & 0xFFFF == {s} & 0xFFFF)"),
                L::Fmand => format!("w0.mac & {s} & 0xFFFF"),
                _ => format!("(w0.mac | {s}) & 0xFFFF"),
            };
            let _ = writeln!(compute, "        let li = {value};");
            set_vi(writes, it, "li");
        }
        L::B | L::Bal => {
            if op == L::Bal {
                set_vi(writes, it, &format!("0x{link:x}u32"));
            }
        }
        L::Jr | L::Jalr => {
            let _ = writeln!(compute, "        let go = ({} * 8) & mask;", vi(is));
            if op == L::Jalr {
                set_vi(writes, it, &format!("0x{link:x}u32"));
            }
        }
        L::Ibeq | L::Ibne | L::Ibltz | L::Ibgtz | L::Iblez | L::Ibgez => {
            let (s, t) = (vi(is), vi(it));
            let test = match op {
                L::Ibeq => "a == b",
                L::Ibne => "a != b",
                L::Ibltz => "a < 0",
                L::Ibgtz => "a > 0",
                L::Iblez => "a <= 0",
                _ => "a >= 0",
            };
            let _ = writeln!(
                compute,
                "        let go = {{ let (a, b) = ({s} as u16 as i16, {t} as u16 as i16); {test} }};"
            );
        }
        L::Xtop | L::Xitop => {
            let v = if op == L::Xtop { "top" } else { "itop" };
            set_vi(writes, it, v);
        }
        L::Xgkick => {
            let _ = writeln!(compute, "        let lk = {};", vi(is));
            c.after.push_str(
                "        if let Some(link) = vu1.as_mut() {\n            \
                 xgkick(&data[..], lk, &mut *link.gs);\n        }\n",
            );
        }
        _ => {}
    }
}

fn pair_code(o: Op) -> String {
    let mut c = PairCode::default();
    upper(o, &mut c);
    let mut lower_code = PairCode::default();
    lower(o, &mut lower_code);
    // Upper results first, then the lower half's (both from the registers
    // before the pair); the lower half's writes, then the upper's.
    let flags = format!("{}{}", lower_code.flags, c.flags);
    let mut out = format!(
        "        // 0x{:04x}: {}\n{}{}{}{}{}",
        o.pc,
        o.pair.display(o.pc),
        lower_code.top,
        c.top,
        c.compute,
        lower_code.compute,
        lower_code.writes,
    );
    out.push_str(&c.writes);
    if flags.is_empty() {
        out.push_str("        (w0, w1, w2) = (w1, w2, w3);\n");
    } else {
        let _ = write!(
            out,
            "        let mut next = w3;\n{flags}        (w0, w1, w2, w3) = (w1, w2, w3, next);\n"
        );
    }
    out.push_str(&lower_code.after);
    out.push_str("        cycle += 1;\n");
    out
}

#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "the function's text, in order"
)]
fn piece(out: &mut String, k: usize, p: &Piece<'_>) -> Option<(u32, Vec<u32>)> {
    let Blocks { ops, blocks } = blocks(p);
    if blocks.is_empty() {
        return None;
    }
    let address = ops[0].pc;
    let end = ops.last().map_or(address, |o| o.pc + 8);
    let code = &p.program.code[8 * p.start..8 * p.end];
    let _ = writeln!(out, "\nstatic CODE_{k}: [u8; {}] = [", code.len());
    for line in code.chunks(16) {
        let bytes: Vec<String> = line.iter().map(|b| format!("0x{b:02x}")).collect();
        let _ = writeln!(out, "    {},", bytes.join(", "));
    }
    out.push_str("];\n\n");
    let small = end <= 0x1000;
    let _ = write!(
        out,
        "/// Pairs {}..{} of {}\n\
         fn piece_{k}(u: &mut Unit<'_>, s: &mut State) -> bool {{\n    \
         match (u.code.len(), u.data.len()) {{\n        \
         (0x4000, 0x4000) => body_{k}::<0x4000>(u, s),\n",
        p.start,
        p.end,
        p.program.names.join(", ")
    );
    if small {
        let _ = writeln!(out, "        (0x1000, 0x1000) => body_{k}::<0x1000>(u, s),");
    }
    let _ = write!(
        out,
        "        _ => step(u, s),\n    }}\n}}\n\n\
         #[allow(unused_mut, unused_variables, unused_assignments, unreachable_code, clippy::all)]\n\
         fn body_{k}<const N: usize>(u: &mut Unit<'_>, s: &mut State) -> bool {{\n    \
         let Some(pipe) = s.pipe() else {{\n        return step(u, s);\n    }};\n    \
         if u.regs.vf[0] != VF0 || u.regs.vi[0] != 0 {{\n        return step(u, s);\n    }}\n    \
         let mask = N as u32 - 1;\n    \
         let (top, itop) = u.vu1.as_ref().map_or((0, 0), |v| (v.top, v.itop));\n    \
         let Unit {{ regs, data, vu1, .. }} = &mut *u;\n    \
         let data = <&mut [u8; N]>::try_from(&mut **data).expect(\"sizes checked\");\n    \
         let mut r = (**regs).clone();\n    \
         let Pipe {{ mut cycle, mut q, mut p, flags: [mut w0, mut w1, mut w2, mut w3] }} = pipe;\n    \
         let mut at = s.pc;\n    \
         let limit = limit(cycle);\n    \
         let exit = 'run: loop {{\n        \
         if cycle >= limit {{\n            break 'run LEAVE;\n        }}\n        \
         match at {{\n"
    );
    for (leader, pairs, end) in &blocks {
        let _ = writeln!(out, "            0x{:04x} => {{", ops[*leader].pc);
        let mut body = String::new();
        for &i in pairs {
            body.push_str("{\n");
            body.push_str(&pair_code(ops[i]));
            body.push_str("}\n");
        }
        match *end {
            End::Fall(next) => {
                let _ = writeln!(body, "at = 0x{next:04x} & mask;");
            }
            End::Branch(b) => {
                let o = ops[b];
                let after = o.pc + 16;
                // `go` lives in the branch pair's scope: hoist it.
                body = body.replacen("{\n", "let go;\n{\n", 1);
                body = body.replace("        let go = ", "        go = ");
                match (o.lower_op(), o.target()) {
                    (Some(LowerOp::B | LowerOp::Bal), Some(t)) => {
                        let _ = writeln!(body, "at = 0x{t:04x} & mask;");
                        body = body.replacen("let go;\n", "", 1);
                    }
                    (_, Some(t)) => {
                        let _ = writeln!(
                            body,
                            "at = if go {{ 0x{t:04x} & mask }} else {{ 0x{after:04x} & mask }};"
                        );
                    }
                    _ => {
                        let _ = writeln!(body, "at = go;");
                    }
                }
            }
            End::Program => {
                let last = ops[*pairs.last().unwrap_or(leader)].pc;
                let _ = write!(
                    body,
                    "r.tpc = 0x{:04x} & mask;\n\
                     (w0, w1, w2) = (w3, w3, w3);\n\
                     if let Some(x) = q.take() {{\n    land_q(x, &mut r);\n}}\n\
                     if let Some(x) = p.take() {{\n    r.p = x.value;\n}}\n\
                     break 'run END;\n",
                    last + 8
                );
            }
        }
        for line in body.lines() {
            let _ = writeln!(out, "                {line}");
        }
        out.push_str("            }\n");
    }
    out.push_str(
        "            _ => break 'run LEAVE,\n        }\n    };\n    \
         w0.shown(&mut r);\n    **regs = r;\n    \
         s.resume(at, Pipe { cycle, q, p, flags: [w0, w1, w2, w3] });\n    \
         exit == END\n}\n",
    );
    let entries = blocks.iter().map(|(l, _, _)| ops[*l].pc).collect();
    Some((address, entries))
}

/// `program`'s pieces: at least [`PIECE`] pairs, cut after a pair control
/// does not fall through from, or (from twice that) anywhere else outside
/// an inner loop (one of at most [`PIECE`] pairs); at three times that,
/// anywhere. Larger functions compile to slower code.
fn pieces(program: &Program) -> Vec<Piece<'_>> {
    let pairs: Vec<Pair> = program
        .code
        .as_chunks::<8>()
        .0
        .iter()
        .map(|b| {
            let word = |at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
            Pair::from_words(word(4), word(0))
        })
        .collect();
    let n = pairs.len();
    // Whether the boundary after pair k lies within a loop.
    let mut inside = vec![false; n];
    for (b, &pair) in pairs.iter().enumerate() {
        let o = Op {
            pc: program.address + 8 * u32::try_from(b).unwrap_or(0),
            pair,
            class: Upper::Nop,
        };
        if let Some(t) = o.target()
            && let Some(t) = t.checked_sub(program.address)
            && let Ok(t) = usize::try_from(t / 8)
            && t <= b
            && b - t <= PIECE
        {
            inside[t..=b].fill(true);
        }
    }
    let mut pieces = Vec::new();
    let mut start = 0;
    for (k, &within) in inside.iter().enumerate() {
        let size = k + 1 - start;
        let free = !within;
        if (free && size >= PIECE && no_fall_through(&pairs, k))
            || (free && size >= 2 * PIECE)
            || size >= 3 * PIECE
        {
            pieces.push(Piece {
                program,
                start,
                end: k + 1,
            });
            start = k + 1;
        }
    }
    if start < n {
        pieces.push(Piece {
            program,
            start,
            end: n,
        });
    }
    pieces
}

/// Rust source translating `overlays`: a `TRANSLATIONS` table for
/// `ps2_vu::micro::run`.
#[must_use]
pub fn module(overlays: &[Overlay]) -> String {
    let programs = programs(overlays);
    let pieces: Vec<Piece<'_>> = programs.iter().flat_map(pieces).collect();
    let mut body = String::new();
    let mut table = String::new();
    for (k, p) in pieces.iter().enumerate() {
        if let Some((address, entries)) = piece(&mut body, k, p) {
            let list: Vec<String> = entries.iter().map(|e| format!("0x{e:04x}")).collect();
            let _ = writeln!(
                table,
                "    Translation {{ address: 0x{address:04x}, code: &CODE_{k}, entries: &[{}], run: piece_{k} }},",
                list.join(", ")
            );
        }
    }
    format!(
        "// Generated by ps2-codegen: do not edit.\n\
         use ps2_vu::micro::{{\n    \
         DIVIDER_STATUS, LowerOp, Pipe, State, Translation, Unit, clip, divide, efu_at, flags,\n    \
         float, land_q, lanes, limit, load_q, merge, rnext, step, store_q, xgkick,\n}};\n\n\
         /// `vf0`.\nconst VF0: [u32; 4] = [0, 0, 0, 0x3F80_0000];\n\
         /// How a piece ends: it left its code, or the program ended.\n\
         const LEAVE: u8 = 0;\nconst END: u8 = 2;\n\n\
         /// The overlays' translations.\n\
         pub static TRANSLATIONS: &[Translation] = &[\n{table}];\n{body}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay(name: &str, vu_address: u32, pairs: usize) -> Overlay {
        Overlay {
            name: name.to_owned(),
            vu_address,
            ee_address: 0,
            code: vec![0; 8 * pairs],
        }
    }

    #[test]
    fn consecutive_parts_of_one_overlay_make_one_program() {
        let overlays = [
            overlay(".DVP.overlay..0x0.437275.54.0", 0, 2),
            overlay(".DVP.overlay..0x10.437275.924.1", 0x10, 1),
            overlay(".DVP.overlay..0x0.436667.33.0", 0, 1),
            overlay(".DVP.overlay..0x20.436667.787.1", 0x20, 1),
        ];
        let p = programs(&overlays);
        let shape: Vec<(u32, usize)> = p.iter().map(|p| (p.address, p.code.len())).collect();
        assert_eq!(shape, [(0, 24), (0, 8), (0x20, 8)]);
        let source = module(&overlays);
        assert!(
            source.contains("entries: &[0x0020], run: piece_2"),
            "{source}"
        );
    }

    fn code(name: &str, words: &[(u32, u32)]) -> Overlay {
        Overlay {
            name: name.to_owned(),
            vu_address: 0,
            ee_address: 0,
            code: words
                .iter()
                .flat_map(|&(upper, lower)| {
                    lower.to_le_bytes().into_iter().chain(upper.to_le_bytes())
                })
                .collect(),
        }
    }

    #[test]
    fn a_loop_is_cut_at_its_branch() {
        // 0: nop | iaddiu vi1, vi1, 1; 1: nop | ibne vi1, vi2, 0 (back to
        // 0); 2: nop | nop (delay slot); 3: nop[e] | nop; 4: nop | nop.
        let nop = 0x0000_02FF;
        let lnop = 0x8000_0330;
        let ibne = (0x29 << 25) | (2 << 16) | (1 << 11) | 0x7FE;
        let iaddiu = (0x08 << 25) | (1 << 16) | (1 << 11) | 1;
        let pairs = [
            (nop, iaddiu),
            (nop, ibne),
            (nop, lnop),
            (nop | FLAG_E, lnop),
            (nop, lnop),
        ];
        let source = module(&[code(".DVP.overlay..0x0.1.2.0", &pairs)]);
        assert!(source.contains("entries: &[0x0000, 0x0018]"), "{source}");
        assert!(
            source.contains("at = if go { 0x0000 & mask } else { 0x0018 & mask };"),
            "{source}"
        );
        assert!(source.contains("break 'run END;"));
    }

    #[test]
    fn pieces_end_outside_inner_loops() {
        // 300 pairs: an unconditional B at 127 (control does not fall
        // through after its delay slot, 128), inside a loop from 100 to a
        // branch back at 140.
        let (nop, lnop) = (0x0000_02FF, 0x8000_0330);
        let b = (0x20 << 25) | 1;
        // IBNE with an offset of -41 pairs (to pair 100).
        let back = (0x29 << 25) | (2 << 16) | (1 << 11) | 0x7D7;
        let mut pairs = vec![(nop, lnop); 300];
        pairs[127].1 = b;
        pairs[140].1 = back;
        let overlay = code(".DVP.overlay..0x0.1.2.0", &pairs);
        let programs = programs(&[overlay]);
        let ends: Vec<usize> = pieces(&programs[0]).iter().map(|p| p.end).collect();
        assert!(ends.iter().all(|&e| !(101..=141).contains(&e)), "{ends:?}");
        assert_eq!(ends.last(), Some(&300));
    }
}
