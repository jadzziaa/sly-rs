//! Jump tables: the `switch` statements GCC compiles to an indexed load and
//! `jr`.
//!
//! The pattern, in any order the scheduler chose:
//!
//! ```text
//! sltiu  $t, $i, N          bound check (N entries)
//! beqz   $t, default
//! sll    $x, $i, 2
//! lui    $b, %hi(table)
//! addu   $b, $b, $x         (or daddu; addiu %lo may come first)
//! lw     $r, %lo(table)($b)
//! jr     $r
//! ```
//!
//! Recovery evaluates the registers feeding the `jr` backwards over a short
//! window: the base must come out as a constant (the table) plus one unknown
//! (the scaled index). The bound comes from the nearest `sltiu`; without one,
//! entries are read while they look like case labels near the `jr`.

use ps2_isa::ee::{Insn, Op};

use crate::Image;

/// How far back from the `jr` the pattern may reach, in instructions.
const WINDOW: u32 = 32;
/// Most entries read from a table without a bound check.
const MAX_UNBOUNDED: u32 = 512;
/// How far from the `jr` an unbounded table's targets may be.
const NEAR: u32 = 0x1_0000;

/// A recovered jump table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JumpTable {
    /// Address of the `jr`.
    pub site: u32,
    /// Address of the table.
    pub table: u32,
    /// The case targets, in table order.
    pub targets: Vec<u32>,
    /// Whether the entry count came from a bound check.
    pub bounded: bool,
}

/// A value seen while evaluating backwards: a constant plus at most one
/// unknown term.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Value {
    constant: u32,
    unknown: bool,
}

impl Value {
    const fn known(constant: u32) -> Self {
        Self {
            constant,
            unknown: false,
        }
    }

    const UNKNOWN: Self = Self {
        constant: 0,
        unknown: true,
    };
}

/// Code as decoded words, addressed from `base`.
pub(crate) struct Code<'a> {
    pub(crate) base: u32,
    pub(crate) insns: &'a [Insn],
}

impl Code<'_> {
    pub(crate) fn at(&self, address: u32) -> Option<Insn> {
        let index = address.checked_sub(self.base)? / 4;
        self.insns.get(index as usize).copied()
    }

    /// The instructions before `site`, nearest first, within the window.
    fn before(&self, site: u32) -> impl Iterator<Item = (u32, Insn)> + '_ {
        (1..=WINDOW)
            .filter_map(move |back| site.checked_sub(back * 4))
            .filter_map(move |address| Some((address, self.at(address)?)))
    }
}

/// Tries to recover the jump table used by the `jr` at `site`.
pub(crate) fn recover(image: &Image, code: &Code<'_>, site: u32) -> Option<JumpTable> {
    let jr = code.at(site)?;
    let target_reg = jr.rs();
    // The load that produced the jump target.
    let (load_at, load) = code
        .before(site)
        .find(|(_, insn)| insn.gpr_dest() == Some(target_reg))?;
    if !matches!(load.op(), Op::Lw | Op::Ld | Op::Lwu) {
        return None;
    }
    let base = evaluate(code, load_at, load.rs(), 0);
    if !base.unknown {
        return None;
    }
    let table = base
        .constant
        .wrapping_add(i32::from(load.simm()).cast_unsigned());
    let section = image.section_at(table)?;
    if matches!(
        section.kind,
        crate::SectionKind::Bss | crate::SectionKind::Code
    ) {
        return None;
    }
    let text = image.text().ok()?;
    let bound = code
        .before(site)
        .find_map(|(_, insn)| (insn.op() == Op::Sltiu).then(|| u32::from(insn.imm())));
    let entries = bound.unwrap_or(MAX_UNBOUNDED);
    let mut targets = Vec::new();
    for index in 0..entries {
        let Some(target) = image.word(table + 4 * index) else {
            break;
        };
        let plausible = text.contains(&target)
            && target % 4 == 0
            && code.at(target).is_some_and(Insn::is_valid);
        let near = target.abs_diff(site) < NEAR;
        if !plausible || (bound.is_none() && !near) {
            if bound.is_some() {
                return None;
            }
            break;
        }
        targets.push(target);
    }
    (!targets.is_empty()).then(|| JumpTable {
        site,
        table,
        targets,
        bounded: bound.is_some(),
    })
}

/// The value of `reg` just before the instruction at `at`, as far as the
/// window shows.
fn evaluate(code: &Code<'_>, at: u32, reg: u8, depth: u32) -> Value {
    if reg == 0 {
        return Value::known(0);
    }
    if depth > 8 {
        return Value::UNKNOWN;
    }
    let Some((def_at, def)) = code
        .before(at)
        .find(|(_, insn)| insn.gpr_dest() == Some(reg))
    else {
        return Value::UNKNOWN;
    };
    let eval = |r| evaluate(code, def_at, r, depth + 1);
    match def.op() {
        Op::Lui => Value::known(u32::from(def.imm()) << 16),
        Op::Addiu | Op::Daddiu | Op::Addi | Op::Daddi => {
            let base = eval(def.rs());
            Value {
                constant: base
                    .constant
                    .wrapping_add(i32::from(def.simm()).cast_unsigned()),
                ..base
            }
        }
        Op::Ori => {
            let base = eval(def.rs());
            Value {
                constant: base.constant | u32::from(def.imm()),
                ..base
            }
        }
        Op::Addu | Op::Daddu | Op::Add | Op::Dadd => {
            let (a, b) = (eval(def.rs()), eval(def.rt()));
            if a.unknown && b.unknown {
                Value::UNKNOWN
            } else {
                Value {
                    constant: a.constant.wrapping_add(b.constant),
                    unknown: a.unknown || b.unknown,
                }
            }
        }
        _ => Value::UNKNOWN,
    }
}
