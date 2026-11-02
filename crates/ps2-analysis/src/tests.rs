//! Discovery, jump tables, graphs, syscall stubs and fingerprints on small
//! hand-assembled programs.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::single_range_in_vec_init,
    reason = "hand-assembled test programs"
)]

use std::ops::Range;

use super::*;

const TEXT: u32 = 0x0010_0000;
const DATA: u32 = 0x0010_1000;

// A few encodings, enough to write test programs.
const NOP: u32 = 0;
const RA: u32 = 31;
fn addiu(rt: u32, rs: u32, imm: i16) -> u32 {
    0x2400_0000 | rs << 21 | rt << 16 | u32::from(imm as u16)
}
fn lui(rt: u32, imm: u16) -> u32 {
    0x3C00_0000 | rt << 16 | u32::from(imm)
}
fn lw(rt: u32, offset: i16, base: u32) -> u32 {
    0x8C00_0000 | base << 21 | rt << 16 | u32::from(offset as u16)
}
fn sll(rd: u32, rt: u32, sa: u32) -> u32 {
    rt << 16 | rd << 11 | sa << 6
}
fn addu(rd: u32, rs: u32, rt: u32) -> u32 {
    rs << 21 | rt << 16 | rd << 11 | 0x21
}
fn sltiu(rt: u32, rs: u32, imm: u16) -> u32 {
    0x2C00_0000 | rs << 21 | rt << 16 | u32::from(imm)
}
fn beq(rs: u32, rt: u32, offset: i16) -> u32 {
    0x1000_0000 | rs << 21 | rt << 16 | u32::from(offset as u16)
}
fn jr(rs: u32) -> u32 {
    rs << 21 | 8
}
fn jal(target: u32) -> u32 {
    0x0C00_0000 | (target >> 2 & 0x03FF_FFFF)
}
fn j(target: u32) -> u32 {
    0x0800_0000 | (target >> 2 & 0x03FF_FFFF)
}
const SYSCALL: u32 = 0xC;

/// An image with `code` at `TEXT` and `data` words at `DATA`.
fn image(code: &[u32], data: &[u32]) -> Image {
    let mut memory = vec![0; (DATA - TEXT) as usize + data.len() * 4 + 4];
    for (i, word) in code.iter().chain(std::iter::repeat_n(&0, 0)).enumerate() {
        memory[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    let at = (DATA - TEXT) as usize;
    for (i, word) in data.iter().enumerate() {
        memory[at + i * 4..at + i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    let text_end = TEXT + 4 * code.len() as u32;
    let sections = vec![
        Section {
            name: ".text".into(),
            range: TEXT..text_end,
            kind: SectionKind::Code,
        },
        Section {
            name: ".rodata".into(),
            range: DATA..DATA + 4 * data.len().max(1) as u32,
            kind: SectionKind::Data,
        },
    ];
    Image::from_memory(TEXT, TEXT, memory, sections)
}

fn ranges(found: &Discovery) -> Vec<Range<u32>> {
    found.functions.iter().map(|f| f.start..f.end).collect()
}

#[test]
fn functions_back_to_back_with_padding() {
    // main: jal f; nop; jr $ra; nop | padding | f: jr $ra; nop
    let f = TEXT + 0x18;
    let code = [jal(f), NOP, jr(RA), NOP, NOP, NOP, jr(RA), NOP];
    let found = discover(&image(&code, &[]), &[]).unwrap();
    assert_eq!(ranges(&found), [TEXT..TEXT + 0x10, f..f + 8]);
    assert_eq!(found.functions[1].origin, Origin::Call);
    assert_eq!(found.padding, [TEXT + 0x10..f]);
}

#[test]
fn a_return_does_not_end_a_function_a_branch_jumps_past() {
    // beq $a0, $zero, +3 ; nop ; jr $ra ; nop ; addiu $v0, $zero, 1 ; jr $ra ; nop
    let code = [beq(4, 0, 3), NOP, jr(RA), NOP, addiu(2, 0, 1), jr(RA), NOP];
    let found = discover(&image(&code, &[]), &[]).unwrap();
    assert_eq!(ranges(&found), [TEXT..TEXT + 0x1C]);
    let cfg = Cfg::build(&image(&code, &[]), &found.functions[0], &found.jump_tables);
    let starts: Vec<u32> = cfg.blocks.iter().map(|b| b.start).collect();
    assert_eq!(starts, [TEXT, TEXT + 8, TEXT + 0x10]);
    assert_eq!(cfg.blocks[0].edges.len(), 2);
    assert!(cfg.blocks[1].exits && cfg.blocks[2].exits);
}

#[test]
fn switch_cases_stay_in_their_function() {
    // sltiu $t0, $a0, 3 ; beq $t0, $zero, default ; sll $t1, $a0, 2
    // lui $t2, hi ; addu $t2, $t2, $t1 ; lw $t3, lo($t2) ; jr $t3 ; nop
    // case0: jr $ra ; nop   case1: jr $ra ; nop   case2/default: jr $ra ; nop
    let case = |i: u32| TEXT + 0x20 + 8 * i;
    let code = [
        sltiu(8, 4, 3),
        beq(8, 0, 11),
        sll(9, 4, 2),
        lui(10, (DATA >> 16) as u16),
        addu(10, 10, 9),
        lw(11, (DATA & 0xFFFF) as i16, 10),
        jr(11),
        NOP,
        jr(RA),
        NOP,
        jr(RA),
        NOP,
        jr(RA),
        NOP,
    ];
    let image = image(&code, &[case(0), case(1), case(2)]);
    let found = discover(&image, &[]).unwrap();
    assert_eq!(found.jump_tables.len(), 1);
    let table = &found.jump_tables[0];
    assert_eq!(
        (table.site, table.table, table.bounded),
        (TEXT + 0x18, DATA, true)
    );
    assert_eq!(table.targets, [case(0), case(1), case(2)]);
    assert_eq!(
        ranges(&found),
        [TEXT..case(2) + 8],
        "the cases belong to the function"
    );
    let cfg = Cfg::build(&image, &found.functions[0], &found.jump_tables);
    let switch_block = cfg.blocks.iter().find(|b| b.end == TEXT + 0x20).unwrap();
    assert_eq!(
        switch_block
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Case)
            .count(),
        3
    );
}

#[test]
fn j_is_a_tail_call() {
    // a: j b ; nop | b: jr $ra ; nop   (b is reached only by j)
    let code = [j(TEXT + 8), NOP, jr(RA), NOP];
    let found = discover(&image(&code, &[]), &[]).unwrap();
    assert_eq!(ranges(&found), [TEXT..TEXT + 8, TEXT + 8..TEXT + 0x10]);
}

#[test]
fn data_pointers_start_functions_only_where_functions_can_start() {
    // f: jr $ra ; nop ; g: addiu ; addiu ; jr $ra ; nop. A pointer to g
    // (after a return) counts; one into the middle of g does not.
    let code = [jr(RA), NOP, addiu(2, 0, 1), addiu(2, 2, 1), jr(RA), NOP];
    let found = discover(&image(&code, &[TEXT + 8, TEXT + 0xC]), &[]).unwrap();
    assert_eq!(ranges(&found), [TEXT..TEXT + 8, TEXT + 8..TEXT + 0x18]);
    assert_eq!(found.functions[1].origin, Origin::Pointer);
}

#[test]
fn syscall_stubs_are_named() {
    let code = [
        addiu(3, 0, 0x40),
        SYSCALL,
        jr(RA),
        NOP,
        addiu(3, 0, -0x43),
        SYSCALL,
        jr(RA),
        NOP,
    ];
    let image = image(&code, &[]);
    let found = discover(&image, &[TEXT + 0x10]).unwrap();
    let stubs = syscall_stubs(&image, &found);
    let names: Vec<_> = stubs.iter().map(|s| (s.number, s.name)).collect();
    assert_eq!(
        names,
        [(0x40, Some("CreateSema")), (-0x43, Some("iSignalSema"))]
    );
}

#[test]
fn fingerprints_ignore_addresses_but_not_code() {
    let body = |callee: u32, hi: u16| {
        [
            addiu(29, 29, -16),
            lui(4, hi),
            addiu(4, 4, 0x10),
            jal(callee),
            NOP,
            addiu(29, 29, 16),
            jr(RA),
            NOP,
        ]
    };
    let print = |code: &[u32]| {
        let image = image(code, &[]);
        let function = Function {
            start: TEXT,
            end: TEXT + 4 * code.len() as u32,
            origin: Origin::Entry,
        };
        fingerprint(&image, &function).unwrap()
    };
    let a = print(&body(0x0020_0000, 0x25));
    assert_eq!(a, print(&body(0x0030_0000, 0x26)), "addresses are masked");
    let mut changed = body(0x0020_0000, 0x25);
    changed[5] = addiu(29, 29, 32);
    assert_ne!(a, print(&changed), "code is not");
}
