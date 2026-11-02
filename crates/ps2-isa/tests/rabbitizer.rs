//! ps2-isa against rabbitizer, an independent R5900 decoder used by the
//! decompilation community. Over all 2^32 words the two must agree on
//! validity, and on the mnemonic of every valid word, except for the
//! documented differences below. Run with `cargo xtask test-full`.

#![allow(
    clippy::cast_possible_truncation,
    reason = "every word counter is below 2^32"
)]

use std::collections::BTreeMap;

use ps2_isa::ee::Insn;
use rabbitizer::{Instruction, InstructionFlags, IsaExtension, Vram};

/// Operations named differently on purpose: (ours, rabbitizer's).
const RENAMED: &[(&str, &str)] = &[
    // rabbitizer writes R5900 `sqrt.s fd, ft` as `c1 <raw>` so modern GNU as
    // accepts it.
    ("sqrt.s", "c1"),
];

/// Base MIPS instructions rabbitizer still accepts in R5900 mode that the
/// R5900 does not implement (it raises a reserved-instruction exception):
/// no 64-bit multiply/divide, no LL/SC, no double precision or 64-bit
/// integer FPU formats, no COP2 word loads, only four FPU compares.
const NOT_ON_R5900: &[&str] = &[
    "ll", "lld", "sc", "scd", "ldc1", "sdc1", "lwc2", "swc2", "dmult", "dmultu", "ddiv", "ddivu",
    "dmfc0", "dmtc0", "cfc0", "ctc0", "dmfc1", "dmtc1", "rfe", "c.un.s", "c.ueq.s", "c.olt.s",
    "c.ult.s", "c.ole.s", "c.ule.s", "c.sf.s", "c.ngle.s", "c.seq.s", "c.ngl.s", "c.nge.s",
    "c.ngt.s",
];

/// Why a disagreement is expected, if it is.
fn explained(ours: (&str, bool), theirs: (&str, bool), word: u32) -> Option<&'static str> {
    let (mnemonic, valid) = theirs;
    if !ours.1 && valid {
        let fpu_format = mnemonic
            .split('.')
            .skip(1)
            .any(|part| part == "d" || part == "l");
        let rounding = ["ceil.", "floor.", "round.", "trunc."]
            .iter()
            .any(|p| mnemonic.starts_with(p));
        // c.lt.s and c.le.s are R5900 instructions only with function 0x34 and
        // 0x36; rabbitizer also accepts the signalling encodings 0x3C, 0x3E.
        let compare_alias = matches!(mnemonic, "c.lt.s" | "c.le.s") && word & 0x3F >= 0x38;
        if fpu_format || rounding || compare_alias || NOT_ON_R5900.contains(&mnemonic) {
            return Some("not an R5900 instruction; rabbitizer accepts the base MIPS one");
        }
        if matches!(mnemonic, "sync" | "sync.p") {
            return Some(
                "sync with stype bits 9..6 set: reserved by the EE manual, ignored by rabbitizer",
            );
        }
        if mnemonic == "c1" {
            return Some(
                "sqrt.s with fs set: reserved by the EE manual; rabbitizer's raw c1 accepts any bits",
            );
        }
    }
    if ours.1 && !valid {
        if mnemonic == "multu" {
            return Some(
                "R5900 multu writes rd (EE manual, PCSX2); rabbitizer has the three-operand form for mult only",
            );
        }
        if mnemonic == "syscall" {
            return Some(
                "syscall's code field is free for software; rabbitizer requires zero in R5900 mode",
            );
        }
    }
    None
}

/// A disagreement class: our (mnemonic, valid) and rabbitizer's.
type Key = (&'static str, bool, &'static str, bool);

fn rabbitizer(word: u32) -> (&'static str, bool) {
    let flags = InstructionFlags::new_extension(IsaExtension::R5900EE).with_enable_pseudos(false);
    let instr = Instruction::new(word, Vram::new(0), flags);
    (instr.opcode().name(), instr.is_valid())
}

fn agrees(ours: (&str, bool), theirs: (&str, bool)) -> bool {
    match (ours.1, theirs.1) {
        (false, false) => true,
        (true, true) => ours.0 == theirs.0 || RENAMED.contains(&(ours.0, theirs.0)),
        _ => false,
    }
}

#[test]
#[ignore = "exhaustive: cargo xtask test-full"]
fn agrees_with_rabbitizer_on_every_word() {
    let threads = std::thread::available_parallelism().map_or(8, |n| n.get() as u64);
    let chunk = (1u64 << 32).div_ceil(threads);
    let mut disagreements: BTreeMap<Key, (u64, u32)> = BTreeMap::new();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                scope.spawn(move || {
                    let mut found: BTreeMap<Key, (u64, u32)> = BTreeMap::new();
                    for word in t * chunk..((t + 1) * chunk).min(1 << 32) {
                        let word = word as u32;
                        let insn = Insn::decode(word);
                        let ours = (insn.op().mnemonic(), insn.is_valid());
                        let theirs = rabbitizer(word);
                        if !agrees(ours, theirs) && explained(ours, theirs, word).is_none() {
                            let entry = found
                                .entry((ours.0, ours.1, theirs.0, theirs.1))
                                .or_insert((0, word));
                            entry.0 += 1;
                        }
                    }
                    found
                })
            })
            .collect();
        for handle in handles {
            for (key, (count, example)) in handle.join().unwrap() {
                let entry = disagreements.entry(key).or_insert((0, example));
                entry.0 += count;
                entry.1 = entry.1.min(example);
            }
        }
    });
    let report: Vec<String> = disagreements
        .iter()
        .map(|((ours, ours_valid, theirs, theirs_valid), (count, example))| {
            format!("{count:>12} words, e.g. {example:08x}: ours {ours} (valid {ours_valid}), rabbitizer {theirs} (valid {theirs_valid})")
        })
        .collect();
    assert!(
        report.is_empty(),
        "{} kinds of disagreement:\n{}",
        report.len(),
        report.join("\n")
    );
}
