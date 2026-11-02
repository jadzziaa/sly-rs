//! Function discovery in `.text`, from the code alone.
//!
//! GCC lays functions out back to back, aligned and padded with `nop`s, so a
//! linear sweep finds them: a function runs until a return, tail call or
//! backward jump that no pending forward branch reaches past, and the next
//! non-padding word starts the next one. The sweep is seeded with *strong*
//! starts that always begin a function: the entry point, every `jal` target,
//! caller-supplied seeds, and code addresses stored in data or built with
//! `lui`/`addiu` where a function can begin (minus jump-table entries). Jump tables are recovered first
//! so a `switch`'s cases count as the function's own.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use ps2_isa::ee::{Flow, Insn, Op};

use crate::switch::{self, Code, JumpTable};
use crate::{AnalysisError, Image, SectionKind};

/// Why a function start is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Origin {
    /// The ELF entry point.
    Entry,
    /// Supplied by the caller (symbols).
    Seed,
    /// The target of a `jal`.
    Call,
    /// A code address stored in data or built in a register.
    Pointer,
    /// The first word after the previous function and its padding.
    Sweep,
}

/// A function: `[start, end)` in `.text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    /// First instruction.
    pub start: u32,
    /// First address after the function (padding excluded).
    pub end: u32,
    /// The strongest reason the start is known.
    pub origin: Origin,
}

/// Something the sweep could not reconcile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Anomaly {
    /// A branch inside a function targets a strong start of another
    /// function past it, or an address before the function's start.
    BranchOutside {
        /// The branch.
        site: u32,
        /// Its target.
        target: u32,
    },
    /// A function was cut at a strong start while a branch still pointed
    /// past it.
    CutAtStart {
        /// The function that was cut.
        function: u32,
        /// The start that cut it.
        at: u32,
    },
    /// An invalid word inside a function's flow.
    InvalidInFunction {
        /// Its address.
        address: u32,
    },
    /// A `jr` (not `$ra`) whose jump table was not recovered.
    UnresolvedJr {
        /// Its address.
        site: u32,
    },
    /// A code address taken as a pointer that lies inside a function (a
    /// branch passes over it), so it does not start one.
    PointerInside {
        /// The function.
        function: u32,
        /// The address.
        at: u32,
    },
    /// Code after a function that only branches back into it (blocks GCC
    /// left after the function's last return), merged into it.
    MergedOrphan {
        /// The function.
        function: u32,
        /// Where the orphan code began.
        at: u32,
    },
}

/// What [`discover`] found in `.text`.
#[derive(Clone, Debug, Default)]
pub struct Discovery {
    /// The functions, by address.
    pub functions: Vec<Function>,
    /// `nop` padding between functions.
    pub padding: Vec<Range<u32>>,
    /// Words outside any function that are not instructions.
    pub data: Vec<Range<u32>>,
    /// Recovered jump tables, by site.
    pub jump_tables: Vec<JumpTable>,
    /// Things to look at.
    pub anomalies: Vec<Anomaly>,
}

impl Discovery {
    /// The function containing `address`.
    #[must_use]
    pub fn function_at(&self, address: u32) -> Option<&Function> {
        let index = self.functions.partition_point(|f| f.start <= address);
        self.functions[..index].last().filter(|f| address < f.end)
    }
}

/// Discovers the functions of `image`'s `.text`. `seeds` are addresses known
/// to start functions (from symbols); pass none to measure discovery alone.
///
/// # Errors
///
/// [`AnalysisError::Layout`] if the image has no `.text`.
pub fn discover(image: &Image, seeds: &[u32]) -> Result<Discovery, AnalysisError> {
    let text = image.text()?;
    let insns: Vec<Insn> = (text.start..text.end)
        .step_by(4)
        .map(|address| Insn::decode(image.word(address).unwrap_or(0)))
        .collect();
    let code = Code {
        base: text.start,
        insns: &insns,
    };

    // Jump tables first: their entries are case labels, not function starts.
    let mut jump_tables = Vec::new();
    let mut anomalies = Vec::new();
    for (address, insn) in (text.start..).step_by(4).zip(&insns) {
        if insn.is_valid() && insn.op() == Op::Jr && insn.rs() != 31 {
            match switch::recover(image, &code, address) {
                Some(table) => jump_tables.push(table),
                None => anomalies.push(Anomaly::UnresolvedJr { site: address }),
            }
        }
    }
    let table_words: BTreeSet<u32> = jump_tables
        .iter()
        .flat_map(|t| (0u32..).zip(&t.targets).map(move |(i, _)| t.table + 4 * i))
        .collect();
    let cases: BTreeMap<u32, u32> = jump_tables
        .iter()
        .map(|t| (t.site, t.targets.iter().copied().max().unwrap_or(t.site)))
        .collect();
    let case_targets: BTreeSet<u32> = jump_tables
        .iter()
        .flat_map(|t| t.targets.iter().copied())
        .collect();

    let starts = strong_starts(image, &code, &text, seeds, &table_words, &case_targets);
    let mut discovery = sweep(&code, &text, &starts, &cases, anomalies);
    discovery.jump_tables = jump_tables;
    Ok(discovery)
}

/// Every start the sweep must honour, with its strongest origin.
fn strong_starts(
    image: &Image,
    code: &Code<'_>,
    text: &Range<u32>,
    seeds: &[u32],
    table_words: &BTreeSet<u32>,
    case_targets: &BTreeSet<u32>,
) -> BTreeMap<u32, Origin> {
    let mut starts = BTreeMap::new();
    let mut add = |address: u32, origin: Origin| {
        if text.contains(&address)
            && address.is_multiple_of(4)
            && code.at(address).is_some_and(Insn::is_valid)
        {
            let entry = starts.entry(address).or_insert(origin);
            *entry = (*entry).min(origin);
        }
    };
    add(image.entry, Origin::Entry);
    for &seed in seeds {
        add(seed, Origin::Seed);
    }
    let mut pointers = Vec::new();
    for (address, insn) in (text.start..).step_by(4).zip(code.insns) {
        if !insn.is_valid() {
            continue;
        }
        if insn.op() == Op::Jal {
            add(insn.jump_target(address), Origin::Call);
        }
        // lui $r, hi ; addiu/ori $s, $r, lo within a few instructions.
        if insn.op() == Op::Lui {
            let reg = insn.rt();
            for next in (1..=6).filter_map(|i| code.at(address + 4 * i)) {
                if matches!(next.op(), Op::Addiu | Op::Ori) && next.rs() == reg {
                    let low = if next.op() == Op::Ori {
                        u32::from(next.imm())
                    } else {
                        i32::from(next.simm()).cast_unsigned()
                    };
                    let value = if next.op() == Op::Ori {
                        (u32::from(insn.imm()) << 16) | low
                    } else {
                        (u32::from(insn.imm()) << 16).wrapping_add(low)
                    };
                    pointers.push(value);
                    break;
                }
                if next.gpr_dest() == Some(reg) {
                    break;
                }
            }
        }
    }
    // Code addresses stored in data (vtables, callback tables), except
    // jump-table entries and exception landing pads.
    for section in &image.sections {
        if section.kind != SectionKind::Data || section.name == ".gcc_except_table" {
            continue;
        }
        for address in (section.range.start..section.range.end.saturating_sub(3)).step_by(4) {
            if table_words.contains(&address) {
                continue;
            }
            if let Some(value) = image.word(address) {
                pointers.push(value);
            }
        }
    }
    for pointer in pointers {
        if !case_targets.contains(&pointer) && plausible_start(code, pointer) {
            add(pointer, Origin::Pointer);
        }
    }
    starts
}

/// Whether a code address taken from data or a constant can start a
/// function. Integers in data often fall in `.text` by chance, so an address
/// counts only where GCC would put a function: 8-byte aligned, after `nop`
/// padding or right after a return, jump or `eret` and its delay slot.
fn plausible_start(code: &Code<'_>, address: u32) -> bool {
    if !address.is_multiple_of(8) {
        return false;
    }
    let after_padding = address
        .checked_sub(4)
        .and_then(|a| code.at(a))
        .is_some_and(|i| i.word() == 0);
    let after_transfer = address
        .checked_sub(8)
        .and_then(|a| code.at(a))
        .is_some_and(|i| {
            matches!(i.flow(), Flow::Jump | Flow::JumpReg)
                || (i.op() == Op::Beq && i.rs() == 0 && i.rt() == 0)
        });
    let after_eret = address
        .checked_sub(4)
        .and_then(|a| code.at(a))
        .is_some_and(|i| i.op() == Op::Eret);
    after_padding || after_transfer || after_eret
}

/// The linear sweep.
fn sweep(
    code: &Code<'_>,
    text: &Range<u32>,
    starts: &BTreeMap<u32, Origin>,
    cases: &BTreeMap<u32, u32>,
    mut anomalies: Vec<Anomaly>,
) -> Discovery {
    let mut discovery = Discovery::default();
    let mut pos = text.start;
    while pos < text.end {
        let Some(insn) = code.at(pos) else { break };
        if !starts.contains_key(&pos) {
            if insn.word() == 0 {
                push_range(&mut discovery.padding, pos..pos + 4);
                pos += 4;
                continue;
            }
            if !insn.is_valid() {
                push_range(&mut discovery.data, pos..pos + 4);
                pos += 4;
                continue;
            }
        }
        let start = pos;
        let origin = starts.get(&start).copied().unwrap_or(Origin::Sweep);
        let end = function_end(code, text, start, starts, cases, &mut anomalies);
        discovery.functions.push(Function { start, end, origin });
        pos = end.max(start + 4);
    }
    merge_orphans(code, &mut discovery.functions, &mut anomalies);
    discovery.anomalies = anomalies;
    discovery
}

/// Merges sweep-found "functions" whose branches lead back into the function
/// before them: they are that function's orphan blocks.
fn merge_orphans(code: &Code<'_>, functions: &mut Vec<Function>, anomalies: &mut Vec<Anomaly>) {
    let mut merged: Vec<Function> = Vec::with_capacity(functions.len());
    for function in functions.drain(..) {
        if function.origin == Origin::Sweep
            && let Some(previous) = merged.last_mut()
        {
            let back_into_previous = (function.start..function.end).step_by(4).any(|at| {
                code.at(at).is_some_and(|insn| {
                    let target = match insn.flow() {
                        Flow::Branch | Flow::BranchLikely => insn.branch_target(at),
                        Flow::Jump => insn.jump_target(at),
                        _ => return false,
                    };
                    (previous.start..function.start).contains(&target)
                })
            });
            if back_into_previous {
                anomalies.push(Anomaly::MergedOrphan {
                    function: previous.start,
                    at: function.start,
                });
                previous.end = function.end;
                continue;
            }
        }
        merged.push(function);
    }
    *functions = merged;
}

/// Where the function starting at `start` ends.
fn function_end(
    code: &Code<'_>,
    text: &Range<u32>,
    start: u32,
    starts: &BTreeMap<u32, Origin>,
    cases: &BTreeMap<u32, u32>,
    anomalies: &mut Vec<Anomaly>,
) -> u32 {
    // The furthest address a branch seen so far still has to reach.
    let mut limit = start;
    let mut at = start;
    while at < text.end {
        if at != start
            && let Some(&origin) = starts.get(&at)
        {
            if limit <= at {
                return at;
            }
            if origin == Origin::Pointer {
                anomalies.push(Anomaly::PointerInside {
                    function: start,
                    at,
                });
            } else {
                anomalies.push(Anomaly::CutAtStart {
                    function: start,
                    at,
                });
                return at;
            }
        }
        let Some(insn) = code.at(at) else { return at };
        if !insn.is_valid() {
            if at < limit {
                anomalies.push(Anomaly::InvalidInFunction { address: at });
            }
            return at;
        }
        // Control may leave here; the function ends after the delay slot
        // unless a branch still targets something beyond it.
        let leaves = match insn.flow() {
            Flow::Branch | Flow::BranchLikely | Flow::BranchLink | Flow::BranchLinkLikely => {
                let target = insn.branch_target(at);
                if target < start
                    || (target > at && target != start && starts.contains_key(&target))
                {
                    anomalies.push(Anomaly::BranchOutside { site: at, target });
                } else {
                    limit = limit.max(target);
                }
                // `b` (beq $zero, $zero) is unconditional.
                insn.op() == Op::Beq && insn.rs() == 0 && insn.rt() == 0
            }
            // `j` is a tail call: GCC jumps within a function with `b`, and
            // Sony's libraries tail-call with `j`. A backward `j` inside the
            // function is a loop and ends nothing by itself.
            Flow::Jump => true,
            Flow::JumpReg => {
                if let Some(&last_case) = cases.get(&at) {
                    limit = limit.max(last_case);
                }
                true
            }
            Flow::Eret => {
                if limit <= at {
                    return at + 4;
                }
                false
            }
            _ => false,
        };
        if leaves && limit < at + 8 {
            return at + 8;
        }
        at += 4;
    }
    text.end
}

fn push_range(ranges: &mut Vec<Range<u32>>, range: Range<u32>) {
    if range.is_empty() {
        return;
    }
    match ranges.last_mut() {
        Some(last) if last.end == range.start => last.end = range.end,
        _ => ranges.push(range),
    }
}
