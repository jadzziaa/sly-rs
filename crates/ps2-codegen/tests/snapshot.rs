//! The shape of generated code, on a small hand-assembled program: a caller
//! saving `$ra` around a call with an argument in the delay slot, and a
//! callee with a branch, a load and a store. Behaviour is tested by the
//! recompiled conformance program; this catches changes in the code itself.
//! `UPDATE_SNAPSHOTS=1 cargo test -p ps2-codegen` rewrites the snapshot.

#![allow(clippy::unwrap_used, reason = "a test")]

use ps2_analysis::{Image, Section, SectionKind, discover};

const TEXT: u32 = 0x0010_0000;
const NOP: u32 = 0;

fn i_type(op: u32, rs: u32, rt: u32, imm: i16) -> u32 {
    op << 26 | rs << 21 | rt << 16 | u32::from(imm.cast_unsigned())
}

#[test]
fn small_program() {
    let f = TEXT + 0x1C;
    let code = [
        i_type(0x09, 29, 29, -16), // addiu $sp, $sp, -16
        i_type(0x2B, 29, 31, 0),   // sw $ra, 0($sp)
        0x0C00_0000 | (f >> 2),    // jal f
        i_type(0x09, 0, 4, 5),     // addiu $a0, $zero, 5
        i_type(0x23, 29, 31, 0),   // lw $ra, 0($sp)
        31 << 21 | 8,              // jr $ra
        i_type(0x09, 29, 29, 16),  // addiu $sp, $sp, 16
        i_type(0x04, 4, 0, 3),     // f: beq $a0, $zero, +3
        NOP,
        i_type(0x23, 4, 2, 0), // lw $v0, 0($a0)
        i_type(0x2B, 4, 2, 4), // sw $v0, 4($a0)
        31 << 21 | 8,          // jr $ra
        NOP,
    ];
    let mut memory = Vec::new();
    for word in code {
        memory.extend_from_slice(&word.to_le_bytes());
    }
    let end = TEXT + u32::try_from(memory.len()).unwrap();
    let sections = vec![Section {
        name: ".text".into(),
        range: TEXT..end,
        kind: SectionKind::Code,
    }];
    let image = Image::from_memory(TEXT, TEXT, memory, sections);
    let found = discover(&image, &[]).unwrap();
    let functions: Vec<_> = found.functions.iter().collect();
    let source = ps2_codegen::module(&image, &found, &functions);

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshot.txt");
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &source).unwrap();
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    assert!(
        source == expected,
        "generated code changed; review it and rerun with UPDATE_SNAPSHOTS=1:\n{source}"
    );
}
