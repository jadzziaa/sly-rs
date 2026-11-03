//! VU micro mode: a program of instruction pairs run from micro memory to
//! its end.
//!
//! Each pair's upper (FMAC) and lower halves read the registers as they
//! were before the pair, then the lower result is written and the upper
//! one after it, so an upper write wins. Branches and the E bit take
//! effect after the next pair (the delay slot). Vector results are ready
//! for the next pair, as the hardware's interlocks make them look; the
//! divider (Q) and VU1's EFU (P) deliver after their latencies, so code
//! that reads Q or P early sees the old value, as on the hardware. A pair
//! takes one cycle.
//!
//! Arithmetic is `ps2_core::float`'s, as in macro mode.

use std::fmt::Write as _;
use std::sync::OnceLock;

use ps2_core::{Vu0 as Regs, float};
use ps2_gs::thread::Sink;
pub use ps2_isa::vu::Pair;
use ps2_isa::vu::{FLAG_E, FLAG_I, LowerOp, UpperOp};

/// The longest program run before giving up (a guard against loops).
pub const MAX_PAIRS: u64 = 1 << 22;

/// What VU1 has that VU0 does not.
pub struct Vu1Link<'a> {
    /// What XITOP reads.
    pub itop: u32,
    /// What XTOP reads.
    pub top: u32,
    /// Where XGKICK sends packets (PATH1).
    pub gs: &'a mut dyn Sink,
}

/// A VU and its memories for one program.
pub struct Unit<'a> {
    /// Its registers.
    pub regs: &'a mut Regs,
    /// Micro memory.
    pub code: &'a [u8],
    /// Data memory.
    pub data: &'a mut [u8],
    /// VU1's extras; `None` for VU0.
    pub vu1: Option<Vu1Link<'a>>,
    /// Its decoded pairs.
    pub cache: &'a mut Cache,
    /// Translated microcode, used where micro memory holds it.
    pub translations: &'static [Translation],
    /// Writes to micro memory so far (any count that changes when it is
    /// written): translations are matched against it again when it moves.
    pub code_writes: u64,
}

impl Unit<'_> {
    /// The translation that runs the pair at `pc`, if micro memory holds
    /// one's code there.
    #[inline]
    fn translation(&mut self, pc: u32) -> Option<Translation> {
        if self.translations.is_empty() {
            return None;
        }
        let key = (self.code_writes, self.translations.len());
        if self.cache.resolved != Some(key) {
            self.cache.resolve(self.code, self.translations);
            self.cache.resolved = Some(key);
        }
        let k = (*self.cache.runs.get(pc as usize / 8)?)?;
        Some(self.translations[usize::from(k)])
    }
}

/// How a program ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    /// Pairs run.
    pub pairs: u64,
    /// It ran past the guard without an E bit.
    pub runaway: bool,
}

/// Status flag bits.
mod status {
    pub(super) const Z: u32 = 1;
    pub(super) const S: u32 = 1 << 1;
    pub(super) const U: u32 = 1 << 2;
    pub(super) const O: u32 = 1 << 3;
    pub(super) const I: u32 = 1 << 4;
    pub(super) const D: u32 = 1 << 5;
}

/// An arithmetic operation's second operand.
#[allow(missing_docs, reason = "the manual's names")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    Ft,
    Bc,
    Q,
    I,
}

/// An arithmetic operation.
#[allow(missing_docs, reason = "the manual's names")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arith {
    Add,
    Sub,
    Mul,
    Madd,
    Msub,
    Max,
    Min,
}

/// What an upper operation does.
#[allow(missing_docs, reason = "the manual's names")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Upper {
    /// The operation, its second operand, and whether it writes ACC.
    Arith(Arith, Operand, bool),
    Opmula,
    Opmsub,
    Itof(u32),
    Ftoi(u32),
    Abs,
    Clip,
    Nop,
}

impl Upper {
    /// The Rust expression for it, for translated microcode.
    #[must_use]
    pub fn source(self) -> String {
        match self {
            Self::Arith(a, o, acc) => {
                format!("Upper::Arith(Arith::{a:?}, Operand::{o:?}, {acc})")
            }
            Self::Itof(n) => format!("Upper::Itof({n})"),
            Self::Ftoi(n) => format!("Upper::Ftoi({n})"),
            other => format!("Upper::{other:?}"),
        }
    }
}

/// Upper operations by opcode, worked out from their mnemonics once.
#[must_use]
pub fn upper_class(op: UpperOp) -> Upper {
    static TABLE: OnceLock<Vec<Upper>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = vec![Upper::Nop; UpperOp::ALL.len() + 1];
        for &op in UpperOp::ALL {
            table[op as usize] = classify(op.mnemonic());
        }
        table
    });
    table[op as usize]
}

fn classify(m: &str) -> Upper {
    match m {
        "opmula" => return Upper::Opmula,
        "opmsub" => return Upper::Opmsub,
        "abs" => return Upper::Abs,
        "clipw" => return Upper::Clip,
        "nop" => return Upper::Nop,
        _ => {}
    }
    if let Some(n) = m.strip_prefix("itof") {
        return Upper::Itof(n.parse().unwrap_or(0));
    }
    if let Some(n) = m.strip_prefix("ftoi") {
        return Upper::Ftoi(n.parse().unwrap_or(0));
    }
    let (arith, rest) = [
        ("madd", Arith::Madd),
        ("msub", Arith::Msub),
        ("mini", Arith::Min),
        ("max", Arith::Max),
        ("add", Arith::Add),
        ("sub", Arith::Sub),
        ("mul", Arith::Mul),
    ]
    .into_iter()
    .find_map(|(p, a)| m.strip_prefix(p).map(|rest| (a, rest)))
    .unwrap_or((Arith::Add, ""));
    // An accumulator form has an `a` after the operation (not max/mini).
    let (acc, rest) = match rest.strip_prefix('a') {
        Some(r) if !matches!(arith, Arith::Max | Arith::Min) => (true, r),
        _ => (false, rest),
    };
    let operand = match rest {
        "" => Operand::Ft,
        "q" => Operand::Q,
        "i" => Operand::I,
        _ => Operand::Bc,
    };
    Upper::Arith(arith, operand, acc)
}

/// A pair decoded, kept by address in micro memory; an entry is used
/// only while the memory still holds the same 64 bits there.
#[derive(Clone, Copy)]
struct Decoded {
    raw: u64,
    pair: Pair,
    class: Upper,
}

/// Decoded pairs of a VU's micro memory (one per VU, kept between
/// programs).
#[derive(Clone, Default)]
pub struct Cache {
    slots: Vec<Option<Decoded>>,
    /// For each pair, the translation that runs it, as micro memory was
    /// when `resolved` was taken (writes so far, translations).
    runs: Vec<Option<u16>>,
    resolved: Option<(u64, usize)>,
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cache({} pairs)", self.slots.iter().flatten().count())
    }
}

impl Cache {
    /// Finds the translations whose code micro memory holds in full.
    fn resolve(&mut self, code: &[u8], translations: &[Translation]) {
        self.runs = vec![None; code.len() / 8];
        for (k, t) in translations.iter().enumerate() {
            let at = t.address as usize;
            if code.get(at..at + t.code.len()) == Some(t.code)
                && let Ok(k) = u16::try_from(k)
            {
                self.runs[at / 8..(at + t.code.len()) / 8].fill(Some(k));
            }
        }
    }

    /// The decoded pair at `pc` (micro memory of `size` bytes).
    fn get(&mut self, size: usize, pc: u32, upper: u32, lower: u32) -> (Pair, Upper) {
        if self.slots.len() != size / 8 {
            self.slots = vec![None; size / 8];
        }
        let raw = (u64::from(upper) << 32) | u64::from(lower);
        let slot = &mut self.slots[(pc as usize / 8) % (size / 8)];
        if let Some(d) = slot.filter(|d| d.raw == raw) {
            return (d.pair, d.class);
        }
        let pair = Pair::from_words(upper, lower);
        let class = upper_class(pair.upper_op());
        *slot = Some(Decoded { raw, pair, class });
        (pair, class)
    }
}

/// Runs `op` (an expression of the component `i`) on the components in
/// `dest`, into `out` and `flags`. A macro rather than a function taking a
/// closure, so that it is always inlined and translations specialise it.
macro_rules! each {
    ($dest:expr, $out:expr, $flags:expr, |$i:ident| $op:expr) => {
        for $i in 0..4 {
            if in_dest($dest, $i) {
                ($out[$i], $flags[$i]) = $op;
            }
        }
    };
}

/// The four words of `[u32; 4]` from `x`, through `f`, without a closure
/// (always inlined).
macro_rules! map4 {
    ($x:expr, |$v:ident| $f:expr) => {{
        let x = $x;
        [
            {
                let $v = x[0];
                $f
            },
            {
                let $v = x[1];
                $f
            },
            {
                let $v = x[2];
                $f
            },
            {
                let $v = x[3];
                $f
            },
        ]
    }};
}

/// Whether component `i` (0 = x … 3 = w) is in the 4-bit `dest` mask.
const fn in_dest(dest: u32, i: usize) -> bool {
    dest & (8 >> i) != 0
}

#[allow(clippy::inline_always, reason = "translations specialise it by `dest`")]
#[inline(always)]
const fn merge(old: [u32; 4], new: [u32; 4], dest: u32) -> [u32; 4] {
    let mut out = old;
    let mut i = 0;
    while i < 4 {
        if in_dest(dest, i) {
            out[i] = new[i];
        }
        i += 1;
    }
    out
}

/// The quadword at byte `at` of data memory.
#[allow(clippy::inline_always, reason = "on every load")]
#[inline(always)]
fn load(data: &[u8], at: u32) -> [u32; 4] {
    let at = at as usize;
    let mut q = [0; 16];
    q.copy_from_slice(&data[at..at + 16]);
    let q = u128::from_le_bytes(q);
    [
        q as u32,
        (q >> 32) as u32,
        (q >> 64) as u32,
        (q >> 96) as u32,
    ]
}

/// The MAC flags and the new status for results in `dest`.
#[allow(clippy::inline_always, reason = "translations specialise it by `dest`")]
#[inline(always)]
fn flags(status: u32, out: [u32; 4], f: [float::Flags; 4], dest: u32) -> (u32, u32) {
    let mut mac = 0;
    for i in 0..4 {
        let bit = 3 - i;
        let set = u32::from(out[i] << 1 == 0)
            | ((out[i] >> 31) << 4)
            | (u32::from(f[i].underflow) << 8)
            | (u32::from(f[i].overflow) << 12);
        mac |= set << bit;
    }
    // Only the components written count: dest's bit for x is bit 3, as
    // the MAC flags' is.
    mac &= dest * 0x1111;
    // Z, S, U, O: whether any component set each group.
    let current = [status::Z, status::S, status::U, status::O]
        .iter()
        .enumerate()
        .fold(0, |c, (group, &flag)| {
            if mac & (0xF << (4 * group)) == 0 {
                c
            } else {
                c | flag
            }
        });
    let sticky = (status & 0xFC0) | (current << 6);
    (mac, (status & (status::I | status::D)) | current | sticky)
}

/// A register write a pair makes, applied after both halves have read.
#[derive(Clone, Copy, Debug)]
enum Write {
    Vf(usize, [u32; 4], u32),
    Vi(usize, u32),
}

/// A lower half's register writes (at most two: LQI and friends write a VF
/// and a VI register).
#[derive(Clone, Copy, Debug)]
struct Writes {
    items: [Write; 2],
    len: usize,
}

impl Writes {
    const fn new() -> Self {
        Self {
            // A write to vi0, which is ignored.
            items: [Write::Vi(0, 0); 2],
            len: 0,
        }
    }

    fn push(&mut self, w: Write) {
        self.items[self.len] = w;
        self.len += 1;
    }
}

/// A result on its way: lands at `ready` (in pairs run).
#[derive(Clone, Copy, Debug)]
struct Pending {
    value: u32,
    ready: u64,
    status: u32,
}

/// Divider latencies, in cycles: DIV and SQRT, RSQRT.
const DIV_LATENCY: u64 = 7;
const RSQRT_LATENCY: u64 = 13;

/// Where a program stands between two pairs.
#[derive(Clone, Copy, Debug, Default)]
pub struct State {
    /// The next pair's byte address in micro memory.
    pub pc: u32,
    /// Pairs run (cycles, with the stalls).
    pub cycle: u64,
    q: Option<Pending>,
    p: Option<Pending>,
    /// A taken branch's target, applied after the pair now running (its
    /// delay slot).
    delayed: Option<u32>,
    /// The pair before had the E bit: the next one is the last.
    ending: bool,
}

/// Translated microcode: the function that runs it, and the bytes it was
/// translated from, at `address` in micro memory.
#[derive(Clone, Copy, Debug)]
pub struct Translation {
    /// Byte address in micro memory.
    pub address: u32,
    /// The microcode it runs.
    pub code: &'static [u8],
    /// Runs pairs from `State::pc`, which lies in `code`, until the program
    /// ends (true), or it leaves `code` or reaches [`MAX_PAIRS`] (false).
    pub run: fn(&mut Unit<'_>, &mut State) -> bool,
}

/// Runs a program from byte address `pc` of micro memory to its end,
/// through translations wherever micro memory holds what they were
/// translated from, and the interpreter elsewhere.
pub fn run(unit: &mut Unit<'_>, pc: u32) -> Run {
    static DUMP: OnceLock<Option<std::ffi::OsString>> = OnceLock::new();
    let code_mask = unit.code.len() as u32 - 1;
    let mut s = State {
        pc: pc & code_mask & !7,
        ..State::default()
    };
    let start = s.pc;
    // A debugging aid: with `$SLY_RS_VU_DUMP` set, the memories and integer
    // registers as the program starts are written there if it runs away.
    // They hold the game's microcode, so never into the repository.
    let snapshot = DUMP
        .get_or_init(|| std::env::var_os("SLY_RS_VU_DUMP"))
        .as_ref()
        .map(|path| {
            let vi: Vec<u8> = unit.regs.vi.iter().flat_map(|v| v.to_le_bytes()).collect();
            (path, [unit.code, &*unit.data, &vi].concat())
        });
    loop {
        if s.cycle >= MAX_PAIRS {
            unit.regs.tpc = s.pc;
            report_runaway(unit, start, s.pc);
            if let Some((path, bytes)) = &snapshot
                && !std::path::Path::new(path).exists()
            {
                let _ = std::fs::write(path, bytes);
            }
            return Run {
                pairs: s.cycle,
                runaway: true,
            };
        }
        let ended = if let Some(t) = unit.translation(s.pc) {
            (t.run)(unit, &mut s)
        } else {
            let at = s.pc as usize;
            let word = |k: usize| {
                let b = &unit.code[at + 4 * k..at + 4 * k + 4];
                u32::from_le_bytes([b[0], b[1], b[2], b[3]])
            };
            let (lower, upper) = (word(0), word(1));
            let (pair, class) = unit.cache.get(unit.code.len(), s.pc, upper, lower);
            let pc = s.pc;
            exec(unit, &mut s, pc, pair, class)
        };
        if ended {
            return Run {
                pairs: s.cycle,
                runaway: false,
            };
        }
    }
}

/// Runs the pair `pair` at `pc` (where `s` stands), whose upper operation
/// is `class`: true if it was the program's last.
///
/// Translated microcode calls it with each pair's words as constants, so
/// the compiler keeps only what that pair does.
#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::inline_always,
    reason = "the instruction set, one arm each, with the manual's field names; \
              always inlined so that translations specialise it"
)]
#[inline(always)]
pub fn exec(unit: &mut Unit<'_>, s: &mut State, pc: u32, pair: Pair, class: Upper) -> bool {
    let code_mask = unit.code.len() as u32 - 1;
    let data_mask = unit.data.len() as u32 - 1;
    let (upper, lower) = (pair.upper(), pair.lower());
    // Results that have arrived.
    if let Some(p) = s.q.filter(|p| p.ready <= s.cycle) {
        land_q(p, unit.regs);
        s.q = None;
    }
    if let Some(p) = s.p.filter(|p| p.ready <= s.cycle) {
        unit.regs.p = p.value;
        s.p = None;
    }
    let next_pc = (pc + 8) & code_mask;
    let mut writes = Writes::new();
    let mut branch: Option<u32> = None;
    // WAITQ and WAITP stall the whole pair: its upper half sees the
    // result too.
    if upper & FLAG_I == 0 {
        match pair.lower_op() {
            LowerOp::Waitq => {
                if let Some(p) = s.q.take() {
                    land_q(p, unit.regs);
                    s.cycle = s.cycle.max(p.ready);
                }
            }
            LowerOp::Waitp => {
                if let Some(p) = s.p.take() {
                    unit.regs.p = p.value;
                    s.cycle = s.cycle.max(p.ready);
                }
            }
            _ => {}
        }
    }
    let r = &*unit.regs;

    // Upper half.
    let (dest, ft, fs, fd) = (
        (upper >> 21) & 0xF,
        ((upper >> 16) & 31) as usize,
        ((upper >> 11) & 31) as usize,
        ((upper >> 6) & 31) as usize,
    );
    let bc = (upper & 3) as usize;
    let mut upper_acc: Option<[u32; 4]> = None;
    let mut upper_flags: Option<(u32, u32)> = None;
    let mut upper_clip: Option<u32> = None;
    let mut upper_vf: Option<(usize, [u32; 4])> = None;
    match class {
        Upper::Arith(arith, operand, to_acc) => {
            let (a, t, acc) = (r.vf[fs], r.vf[ft], r.acc);
            let b = match operand {
                Operand::Ft => t,
                Operand::Bc => [t[bc]; 4],
                Operand::Q => [r.q; 4],
                Operand::I => [r.i; 4],
            };
            let mut out = if to_acc { acc } else { r.vf[fd] };
            let mut f = [float::Flags::default(); 4];
            // One loop per operation, the operation chosen once.
            let (o, fl) = (&mut out, &mut f);
            match arith {
                Arith::Add => each!(dest, o, fl, |i| float::add(a[i], b[i])),
                Arith::Sub => each!(dest, o, fl, |i| float::sub(a[i], b[i])),
                Arith::Mul => each!(dest, o, fl, |i| float::mul(a[i], b[i])),
                Arith::Madd => each!(dest, o, fl, |i| float::madd(acc[i], a[i], b[i])),
                Arith::Msub => each!(dest, o, fl, |i| float::msub(acc[i], a[i], b[i])),
                Arith::Max => each!(dest, o, fl, |i| (
                    float::max(a[i], b[i]),
                    float::Flags::default()
                )),
                Arith::Min => each!(dest, o, fl, |i| (
                    float::min(a[i], b[i]),
                    float::Flags::default()
                )),
            }
            if to_acc {
                upper_acc = Some(out);
            } else {
                upper_vf = Some((fd, out));
            }
            if !matches!(arith, Arith::Max | Arith::Min) {
                upper_flags = Some(flags(r.status, out, f, dest));
            }
        }
        op @ (Upper::Opmula | Upper::Opmsub) => {
            let (a, b, acc) = (r.vf[fs], r.vf[ft], r.acc);
            let mut out = if op == Upper::Opmula { acc } else { r.vf[fd] };
            let mut f = [float::Flags::default(); 4];
            for (i, (j, k)) in [(1, 2), (2, 0), (0, 1)].into_iter().enumerate() {
                if in_dest(dest, i) {
                    (out[i], f[i]) = if op == Upper::Opmula {
                        float::mul(a[j], b[k])
                    } else {
                        float::msub(acc[i], a[j], b[k])
                    };
                }
            }
            if op == Upper::Opmula {
                upper_acc = Some(out);
            } else {
                upper_vf = Some((fd, out));
            }
            upper_flags = Some(flags(r.status, out, f, dest));
        }
        Upper::Itof(scale) => {
            let out = map4!(r.vf[fs], |x| float::chop(
                f64::from(x as i32) / f64::from(1u32 << scale)
            )
            .0);
            upper_vf = Some((ft, out));
        }
        Upper::Ftoi(scale) => {
            let out = map4!(r.vf[fs], |x| float::to_int(
                float::chop(float::value(x) * f64::from(1u32 << scale)).0
            ));
            upper_vf = Some((ft, out));
        }
        Upper::Abs => upper_vf = Some((ft, map4!(r.vf[fs], |x| x & 0x7FFF_FFFF))),
        Upper::Clip => {
            let a = r.vf[fs];
            let limit = float::value(r.vf[ft][3]).abs();
            let mut bits = 0;
            for (i, &x) in a.iter().take(3).enumerate() {
                let x = float::value(x);
                bits |= u32::from(x > limit) << (2 * i);
                bits |= u32::from(x < -limit) << (2 * i + 1);
            }
            upper_clip = Some(((r.clip << 6) | bits) & 0x00FF_FFFF);
        }
        Upper::Nop => {}
    }

    // Lower half.
    let (it, is, id) = (
        ((lower >> 16) & 15) as usize,
        ((lower >> 11) & 15) as usize,
        ((lower >> 6) & 15) as usize,
    );
    let (lft, lfs) = (((lower >> 16) & 31) as usize, ((lower >> 11) & 31) as usize);
    let ldest = (lower >> 21) & 0xF;
    let imm11 = ((lower & 0x7FF) << 21) as i32 >> 21;
    let imm15 = (lower & 0x7FF) | ((lower >> 10) & 0x7800);
    let imm5 = (((lower >> 6) & 31) << 27) as i32 >> 27;
    let fsf = ((lower >> 21) & 3) as usize;
    let ftf = ((lower >> 23) & 3) as usize;
    let target = pair.branch_target(pc) & code_mask;
    let qword = |vi: u32, offset: i32| {
        ((vi as i32).wrapping_add(offset) as u32).wrapping_mul(16) & data_mask
    };
    let mut new_i = None;
    let mut new_r = None;
    let mut new_clip = None;
    let mut new_status = None;
    let mut store: Option<(u32, [u32; 4], u32)> = None;
    let mut kick: Option<u32> = None;
    if upper & FLAG_I != 0 {
        new_i = Some(lower);
    } else {
        use LowerOp as L;
        match pair.lower_op() {
            L::Lq => writes.push(Write::Vf(
                lft,
                load(unit.data, qword(r.vi[is], imm11)),
                ldest,
            )),
            L::Sq => store = Some((qword(r.vi[it], imm11), r.vf[lfs], ldest)),
            L::Lqi => {
                writes.push(Write::Vf(lft, load(unit.data, qword(r.vi[is], 0)), ldest));
                writes.push(Write::Vi(is, r.vi[is].wrapping_add(1)));
            }
            L::Lqd => {
                let vi = r.vi[is].wrapping_sub(1);
                writes.push(Write::Vf(lft, load(unit.data, qword(vi, 0)), ldest));
                writes.push(Write::Vi(is, vi));
            }
            L::Sqi => {
                store = Some((qword(r.vi[it], 0), r.vf[lfs], ldest));
                writes.push(Write::Vi(it, r.vi[it].wrapping_add(1)));
            }
            L::Sqd => {
                let vi = r.vi[it].wrapping_sub(1);
                store = Some((qword(vi, 0), r.vf[lfs], ldest));
                writes.push(Write::Vi(it, vi));
            }
            L::Ilw | L::Ilwr => {
                let offset = if pair.lower_op() == L::Ilw { imm11 } else { 0 };
                let v = load(unit.data, qword(r.vi[is], offset));
                let field = (0..4).find(|&i| in_dest(ldest, i)).unwrap_or(0);
                writes.push(Write::Vi(it, v[field]));
            }
            L::Isw | L::Iswr => {
                let offset = if pair.lower_op() == L::Isw { imm11 } else { 0 };
                store = Some((qword(r.vi[is], offset), [r.vi[it] & 0xFFFF; 4], ldest));
            }
            L::Iadd => writes.push(Write::Vi(id, r.vi[is].wrapping_add(r.vi[it]))),
            L::Isub => writes.push(Write::Vi(id, r.vi[is].wrapping_sub(r.vi[it]))),
            L::Iand => writes.push(Write::Vi(id, r.vi[is] & r.vi[it])),
            L::Ior => writes.push(Write::Vi(id, r.vi[is] | r.vi[it])),
            L::Iaddi => writes.push(Write::Vi(it, r.vi[is].wrapping_add(imm5 as u32))),
            L::Iaddiu => writes.push(Write::Vi(it, r.vi[is].wrapping_add(imm15))),
            L::Isubiu => writes.push(Write::Vi(it, r.vi[is].wrapping_sub(imm15))),
            L::Move => writes.push(Write::Vf(lft, r.vf[lfs], ldest)),
            L::Mr32 => {
                let a = r.vf[lfs];
                writes.push(Write::Vf(lft, [a[1], a[2], a[3], a[0]], ldest));
            }
            L::Mtir => writes.push(Write::Vi(it, r.vf[lfs][fsf])),
            L::Mfir => {
                let v = i32::from(r.vi[is] as u16 as i16) as u32;
                writes.push(Write::Vf(lft, [v; 4], ldest));
            }
            op @ (L::Div | L::Sqrt | L::Rsqrt) => {
                // A new division waits for the one under way.
                if let Some(p) = s.q.take() {
                    land_q(p, unit.regs);
                    s.cycle = s.cycle.max(p.ready);
                }
                let r = &*unit.regs;
                let (a, b) = (r.vf[lfs][fsf], r.vf[lft][ftf]);
                let (value, f) = match op {
                    L::Div => float::div(a, b),
                    L::Sqrt => float::sqrt(b),
                    _ => float::rsqrt(a, b),
                };
                let st = if f.invalid { status::I } else { 0 }
                    | if f.divide_by_zero { status::D } else { 0 };
                let latency = if op == L::Rsqrt {
                    RSQRT_LATENCY
                } else {
                    DIV_LATENCY
                };
                s.q = Some(Pending {
                    value,
                    ready: s.cycle + latency,
                    status: st,
                });
            }
            L::Mfp => writes.push(Write::Vf(lft, [r.p; 4], ldest)),
            op @ (L::Esadd
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
            | L::Eexp) => {
                if let Some(p) = s.p.take() {
                    unit.regs.p = p.value;
                    s.cycle = s.cycle.max(p.ready);
                }
                let r = &*unit.regs;
                let (value, latency) = efu(op, r.vf[lfs], r.vf[lfs][fsf]);
                s.p = Some(Pending {
                    value,
                    ready: s.cycle + latency,
                    status: 0,
                });
            }
            L::Rinit => new_r = Some(0x3F80_0000 | (r.vf[lfs][fsf] & 0x007F_FFFF)),
            L::Rxor => new_r = Some(0x3F80_0000 | ((r.r ^ r.vf[lfs][fsf]) & 0x007F_FFFF)),
            L::Rget => writes.push(Write::Vf(lft, [r.r; 4], ldest)),
            L::Rnext => {
                let x = r.r;
                let feedback = ((x >> 4) & 1) ^ ((x >> 22) & 1);
                let next = 0x3F80_0000 | (((x << 1) ^ feedback) & 0x007F_FFFF);
                new_r = Some(next);
                writes.push(Write::Vf(lft, [next; 4], ldest));
            }
            L::Fceq => writes.push(Write::Vi(
                1,
                u32::from((r.clip & 0xFF_FFFF) == lower & 0xFF_FFFF),
            )),
            L::Fcand => writes.push(Write::Vi(1, u32::from(r.clip & lower & 0xFF_FFFF != 0))),
            L::Fcor => writes.push(Write::Vi(
                1,
                u32::from((r.clip | lower) & 0xFF_FFFF == 0xFF_FFFF),
            )),
            L::Fcset => new_clip = Some(lower & 0xFF_FFFF),
            L::Fcget => writes.push(Write::Vi(it, r.clip & 0xFFF)),
            L::Fseq | L::Fsand | L::Fsor | L::Fsset => {
                let imm12 = (lower & 0x7FF) | ((lower >> 10) & 0x800);
                let st = r.status & 0xFFF;
                match pair.lower_op() {
                    L::Fseq => writes.push(Write::Vi(it, u32::from(st == imm12))),
                    L::Fsand => writes.push(Write::Vi(it, st & imm12)),
                    L::Fsor => writes.push(Write::Vi(it, st | imm12)),
                    _ => new_status = Some((r.status & 0x3F) | (imm12 & 0xFC0)),
                }
            }
            L::Fmeq => writes.push(Write::Vi(
                it,
                u32::from(r.mac & 0xFFFF == r.vi[is] & 0xFFFF),
            )),
            L::Fmand => writes.push(Write::Vi(it, r.mac & r.vi[is] & 0xFFFF)),
            L::Fmor => writes.push(Write::Vi(it, (r.mac | r.vi[is]) & 0xFFFF)),
            L::B => branch = Some(target),
            L::Bal => {
                branch = Some(target);
                writes.push(Write::Vi(it, ((pc + 16) / 8) & 0xFFFF));
            }
            L::Jr => branch = Some((r.vi[is] * 8) & code_mask),
            L::Jalr => {
                branch = Some((r.vi[is] * 8) & code_mask);
                writes.push(Write::Vi(it, ((pc + 16) / 8) & 0xFFFF));
            }
            op @ (L::Ibeq | L::Ibne | L::Ibltz | L::Ibgtz | L::Iblez | L::Ibgez) => {
                let (s, t) = (r.vi[is] as u16 as i16, r.vi[it] as u16 as i16);
                let taken = match op {
                    L::Ibeq => s == t,
                    L::Ibne => s != t,
                    L::Ibltz => s < 0,
                    L::Ibgtz => s > 0,
                    L::Iblez => s <= 0,
                    _ => s >= 0,
                };
                if taken {
                    branch = Some(target);
                }
            }
            L::Xtop => writes.push(Write::Vi(it, unit.vu1.as_ref().map_or(0, |v| v.top))),
            L::Xitop => writes.push(Write::Vi(it, unit.vu1.as_ref().map_or(0, |v| v.itop))),
            L::Xgkick => kick = Some(r.vi[is]),
            _ => {}
        }
    }

    // Writes: lower first, then upper, so an upper write wins.
    let regs = &mut *unit.regs;
    for &w in &writes.items[..writes.len] {
        match w {
            Write::Vf(reg, value, dest) if reg != 0 => {
                regs.vf[reg] = merge(regs.vf[reg], value, dest);
            }
            Write::Vi(reg, value) if reg != 0 => regs.vi[reg] = value & 0xFFFF,
            _ => {}
        }
    }
    if let Some((at, value, dest)) = store {
        for i in (0..4).filter(|&i| in_dest(dest, i)) {
            let at = at as usize + 4 * i;
            unit.data[at..at + 4].copy_from_slice(&value[i].to_le_bytes());
        }
    }
    if let Some(v) = new_i {
        regs.i = v;
    }
    if let Some(v) = new_r {
        regs.r = v;
    }
    if let Some(v) = new_clip {
        regs.clip = v;
    }
    if let Some(v) = new_status {
        regs.status = v;
    }
    if let Some((reg, value)) = upper_vf
        && reg != 0
    {
        regs.vf[reg] = merge(regs.vf[reg], value, dest);
    }
    if let Some(acc) = upper_acc {
        regs.acc = merge(regs.acc, acc, dest);
    }
    if let Some((mac, st)) = upper_flags {
        regs.mac = mac;
        regs.status = st;
    }
    if let Some(clip) = upper_clip {
        regs.clip = clip;
    }
    if let Some(address) = kick
        && let Some(link) = unit.vu1.as_mut()
    {
        xgkick(unit.data, address, link.gs);
    }
    s.cycle += 1;

    if s.ending {
        // The pair after the E bit has run: the program is over.
        unit.regs.tpc = next_pc;
        if let Some(p) = s.q {
            land_q(p, unit.regs);
        }
        if let Some(p) = s.p {
            unit.regs.p = p.value;
        }
        return true;
    }
    s.ending = upper & FLAG_E != 0;
    // This pair was a delay slot: jump. Otherwise a branch here jumps
    // after the next pair.
    s.pc = if let Some(target) = s.delayed.take() {
        target
    } else {
        s.delayed = branch;
        next_pc
    };
    false
}
/// Logs where a runaway program is: the next pairs, disassembled, and the
/// integer registers.
fn report_runaway(unit: &Unit<'_>, start: u32, pc: u32) {
    let mask = unit.code.len() as u32 - 1;
    let mut listing = String::new();
    for k in 0..64 {
        let at = (start + 8 * k) & mask;
        let w = |o: u32| {
            let a = (at + o) as usize;
            u32::from_le_bytes([
                unit.code[a],
                unit.code[a + 1],
                unit.code[a + 2],
                unit.code[a + 3],
            ])
        };
        let pair = Pair::from_words(w(4), w(0));
        let _ = write!(listing, "\n  {at:04x}: {}", pair.display(at));
    }
    tracing::debug!(target: "vu", "runaway at {pc:#06x}, vi {:x?}:{listing}", unit.regs.vi);
}

/// A division's result lands in Q, with its status flags.
fn land_q(p: Pending, regs: &mut Regs) {
    regs.q = p.value;
    regs.status = (regs.status & !(status::I | status::D))
        | p.status
        | (regs.status & 0xFC0)
        | (p.status << 6);
}

/// An EFU operation's result and latency.
fn efu(op: LowerOp, v: [u32; 4], f: u32) -> (u32, u64) {
    use LowerOp as L;
    let x = |i: usize| float::value(v[i]);
    let s = float::value(f);
    let (value, latency) = match op {
        L::Esadd => (x(0) * x(0) + x(1) * x(1) + x(2) * x(2), 11),
        L::Ersadd => (1.0 / (x(0) * x(0) + x(1) * x(1) + x(2) * x(2)), 18),
        L::Eleng => ((x(0) * x(0) + x(1) * x(1) + x(2) * x(2)).sqrt(), 18),
        L::Erleng => (1.0 / (x(0) * x(0) + x(1) * x(1) + x(2) * x(2)).sqrt(), 24),
        L::Eatanxy => (x(1).atan2(x(0)), 54),
        L::Eatanxz => (x(2).atan2(x(0)), 54),
        L::Esum => (x(0) + x(1) + x(2) + x(3), 12),
        L::Esqrt => (s.abs().sqrt(), 12),
        L::Ersqrt => (1.0 / s.abs().sqrt(), 18),
        L::Ercpr => (1.0 / s, 12),
        L::Esin => (s.sin(), 29),
        L::Eatan => (s.atan(), 54),
        _ => ((-s).exp(), 44),
    };
    (float::nearest(value).0, latency)
}

/// Sends the GIF packet at quadword `address` of VU1 data memory to the GS
/// on PATH1: tags and their data up to the end of the tag with EOP set.
fn xgkick(data: &[u8], address: u32, gs: &mut dyn Sink) {
    let mask = (data.len() / 16 - 1) as u32;
    let read = |q: u32| {
        let at = ((q & mask) * 16) as usize;
        let mut raw = [0; 16];
        raw.copy_from_slice(&data[at..at + 16]);
        u128::from_le_bytes(raw)
    };
    let mut q = address;
    let mut packet = Vec::new();
    // A guard: a packet never exceeds data memory.
    for _ in 0..=mask {
        let tag = read(q);
        packet.push(tag);
        q += 1;
        let lo = tag as u64;
        let nloop = (lo & 0x7FFF) as u32;
        let eop = lo & 0x8000 != 0;
        let flg = (lo >> 58) & 3;
        let nreg = match (lo >> 60) as u32 {
            0 => 16,
            n => n,
        };
        let qwords = match flg {
            0 => nloop * nreg,
            1 => (nloop * nreg).div_ceil(2),
            _ => nloop,
        };
        for _ in 0..qwords {
            packet.push(read(q));
            q += 1;
        }
        if eop {
            break;
        }
    }
    gs.gif(1, packet);
}

#[cfg(test)]
mod tests;
