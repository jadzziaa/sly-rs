//! Every word of the three boot executables' `.text`, disassembled by
//! ps2-isa and by rabbitizer: the same validity, and the same text for every
//! valid instruction (after normalising rabbitizer's jump labels).

use std::collections::BTreeMap;

use ps2_isa::ee::Insn;
use rabbitizer::operands::DefaultLabelDisplay;
use rabbitizer::{Instruction, InstructionDisplayFlags, InstructionFlags, IsaExtension, Vram};
use sly_common::Game;
use sly_verify::Executable;

/// rabbitizer's text for `word` at `pc`, in the syntax ps2-isa writes.
fn rabbitizer_text(word: u32, pc: u32) -> (String, bool) {
    let flags = InstructionFlags::new_extension(IsaExtension::R5900EE).with_enable_pseudos(false);
    let display = InstructionDisplayFlags::new()
        .with_opcode_ljust(0)
        .with_named_fpr(false)
        .with_gnu_div(false)
        .with_branch_default_label_display(DefaultLabelDisplay::Absolute);
    let instr = Instruction::new(word, Vram::new(pc), flags);
    let text = instr
        .display::<&str>(&display, None, 0)
        .to_string()
        .replace("func_", "0x");
    (text, instr.is_valid())
}

/// Differences that are expected, with the reason (see also
/// ps2-isa/tests/rabbitizer.rs).
fn explained(ours: &str, theirs: &str) -> Option<&'static str> {
    if ours.starts_with("sqrt.s ") && theirs.starts_with("c1 ") {
        return Some("rabbitizer writes sqrt.s as raw c1 for GNU as");
    }
    // rabbitizer swaps fs and ft of vadda and vmsuba (its `_inv_` operands,
    // matching an assembler's quirk); ps2-isa keeps the manual's ACC, fs, ft.
    if ours.starts_with("vadda.") || ours.starts_with("vmsuba.") {
        let swap = |text: &str| -> Option<String> {
            let (head, operands) = text.split_once(" ACC, ")?;
            let (fs, ft) = operands.split_once(", ")?;
            Some(format!("{head} ACC, {ft}, {fs}"))
        };
        if swap(ours).as_deref() == Some(theirs) {
            return Some("rabbitizer swaps the vadda/vmsuba sources");
        }
    }
    None
}

#[test]
#[ignore = "needs the user's discs: cargo xtask test-full"]
fn text_matches_rabbitizer_in_all_three_executables() {
    let mut mismatches: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut counts = (0usize, 0usize, 0usize);
    for game in Game::ALL {
        let exe = Executable::load(game).unwrap();
        let text = exe.section(".text").expect(".text");
        for (pc, word) in Executable::words(text) {
            let insn = Insn::decode(word);
            let (theirs, theirs_valid) = rabbitizer_text(word, pc);
            counts.0 += 1;
            if !insn.is_valid() || !theirs_valid {
                counts.1 += 1;
                if insn.is_valid() != theirs_valid {
                    let key = format!(
                        "validity: ours {} / rabbitizer {theirs_valid}",
                        insn.op().mnemonic()
                    );
                    let entry = mismatches
                        .entry(key)
                        .or_insert((0, format!("{game} {pc:08x} {word:08x}")));
                    entry.0 += 1;
                }
                continue;
            }
            let ours = insn.display(pc).to_string();
            if ours == theirs || explained(&ours, &theirs).is_some() {
                counts.2 += 1;
                continue;
            }
            let key = format!("text: {}", insn.op().mnemonic());
            let entry = mismatches.entry(key).or_insert((
                0,
                format!("{game} {pc:08x} {word:08x}: ours `{ours}`, rabbitizer `{theirs}`"),
            ));
            entry.0 += 1;
        }
    }
    println!(
        "{} words: {} invalid in both, {} identical or explained",
        counts.0, counts.1, counts.2
    );
    let report: Vec<String> = mismatches
        .iter()
        .map(|(key, (count, example))| format!("{count:>8} {key}: e.g. {example}"))
        .collect();
    assert!(
        report.is_empty(),
        "{} kinds of mismatch:\n{}",
        report.len(),
        report.join("\n")
    );
}
