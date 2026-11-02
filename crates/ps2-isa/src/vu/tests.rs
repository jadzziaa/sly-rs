//! VU micro decoding against PCSX2's opcode tables (`DisVUmicro.h`), slot by
//! slot, and hand-made pairs for formatting.

use super::*;

/// PCSX2's upper table (`MicroU`), indexed by bits 5..0; `-` for no
/// instruction. 0x3C..0x3F lead to the special tables below.
const PCSX2_UPPER: [&str; 0x3C] = [
    "ADDx", "ADDy", "ADDz", "ADDw", "SUBx", "SUBy", "SUBz", "SUBw", "MADDx", "MADDy", "MADDz",
    "MADDw", "MSUBx", "MSUBy", "MSUBz", "MSUBw", "MAXx", "MAXy", "MAXz", "MAXw", "MINIx", "MINIy",
    "MINIz", "MINIw", "MULx", "MULy", "MULz", "MULw", "MULq", "MAXi", "MULi", "MINIi", "ADDq",
    "MADDq", "ADDi", "MADDi", "SUBq", "MSUBq", "SUBi", "MSUBi", "ADD", "MADD", "MUL", "MAX", "SUB",
    "MSUB", "OPMSUB", "MINI", "-", "-", "-", "-", "-", "-", "-", "-", "-", "-", "-", "-",
];

/// PCSX2's upper special tables (`_UPPER_FD_00` … `_11`), one per bits 1..0,
/// indexed by bits 10..6 (slots 12..31 are all empty). Two corrections to
/// PCSX2's disassembler: its slot 1 says `SUBx`…`SUBw` where the instruction
/// is `SUBAx`…`SUBAw` (PCSX2's interpreter tables have SUBA), and it names
/// `CLIPw` just `CLIP`.
const PCSX2_UPPER_SPECIAL: [[&str; 12]; 4] = [
    [
        "ADDAx", "SUBAx", "MADDAx", "MSUBAx", "ITOF0", "FTOI0", "MULAx", "MULAq", "ADDAq", "SUBAq",
        "ADDA", "SUBA",
    ],
    [
        "ADDAy", "SUBAy", "MADDAy", "MSUBAy", "ITOF4", "FTOI4", "MULAy", "ABS", "MADDAq", "MSUBAq",
        "MADDA", "MSUBA",
    ],
    [
        "ADDAz", "SUBAz", "MADDAz", "MSUBAz", "ITOF12", "FTOI12", "MULAz", "MULAi", "ADDAi",
        "SUBAi", "MULA", "OPMULA",
    ],
    [
        "ADDAw", "SUBAw", "MADDAw", "MSUBAw", "ITOF15", "FTOI15", "MULAw", "CLIPw", "MADDAi",
        "MSUBAi", "-", "NOP",
    ],
];

/// PCSX2's lower table (`MicroL`), indexed by bits 31..25, as (index, name);
/// every other index is empty except 0x40 (the type-1/3 tables).
const PCSX2_LOWER: [(u32, &str); 28] = [
    (0x00, "LQ"),
    (0x01, "SQ"),
    (0x04, "ILW"),
    (0x05, "ISW"),
    (0x08, "IADDIU"),
    (0x09, "ISUBIU"),
    (0x10, "FCEQ"),
    (0x11, "FCSET"),
    (0x12, "FCAND"),
    (0x13, "FCOR"),
    (0x14, "FSEQ"),
    (0x15, "FSSET"),
    (0x16, "FSAND"),
    (0x17, "FSOR"),
    (0x18, "FMEQ"),
    (0x1A, "FMAND"),
    (0x1B, "FMOR"),
    (0x1C, "FCGET"),
    (0x20, "B"),
    (0x21, "BAL"),
    (0x24, "JR"),
    (0x25, "JALR"),
    (0x28, "IBEQ"),
    (0x29, "IBNE"),
    (0x2C, "IBLTZ"),
    (0x2D, "IBGTZ"),
    (0x2E, "IBLEZ"),
    (0x2F, "IBGEZ"),
];

/// PCSX2's `LowerOP` table (bits 5..0 with bit 31 set).
const PCSX2_LOWER_OP: [(u32, &str); 5] = [
    (0x30, "IADD"),
    (0x31, "ISUB"),
    (0x32, "IADDI"),
    (0x34, "IAND"),
    (0x35, "IOR"),
];

/// PCSX2's `LowerOP_T3_00` … `_11` tables, one per bits 1..0, indexed by bits
/// 10..6 from 12 (lower slots are empty).
const PCSX2_LOWER_SPECIAL: [[&str; 20]; 4] = [
    [
        "MOVE", "LQI", "DIV", "MTIR", "RNEXT", "-", "-", "-", "-", "-", "-", "-", "-", "MFP",
        "XTOP", "XGKICK", "ESADD", "EATANxy", "ESQRT", "ESIN",
    ],
    [
        "MR32", "SQI", "SQRT", "MFIR", "RGET", "-", "-", "-", "-", "-", "-", "-", "-", "-",
        "XITOP", "-", "ERSADD", "EATANxz", "ERSQRT", "EATAN",
    ],
    [
        "-", "LQD", "RSQRT", "ILWR", "RINIT", "-", "-", "-", "-", "-", "-", "-", "-", "-", "-",
        "-", "ELENG", "ESUM", "ERCPR", "EEXP",
    ],
    [
        "-", "SQD", "WAITQ", "ISWR", "RXOR", "-", "-", "-", "-", "-", "-", "-", "-", "-", "-", "-",
        "ERLENG", "-", "WAITP", "-",
    ],
];

fn name(mnemonic: &str) -> String {
    if mnemonic == "(invalid)" {
        "-".to_owned()
    } else {
        mnemonic.to_ascii_lowercase()
    }
}

#[test]
fn upper_tables_match_pcsx2() {
    for (funct, expected) in (0u32..).zip(PCSX2_UPPER) {
        assert_eq!(
            name(upper::decode(funct).mnemonic()),
            expected.to_ascii_lowercase(),
            "funct {funct:#x}"
        );
    }
    for (bc, table) in (0u32..).zip(PCSX2_UPPER_SPECIAL) {
        for fd in 0..32 {
            let expected = table.get(fd as usize).copied().unwrap_or("-");
            let op = upper::decode(0x3C | bc | (fd << 6));
            assert_eq!(
                name(op.mnemonic()),
                expected.to_ascii_lowercase(),
                "bc {bc} fd {fd:#x}"
            );
        }
    }
}

#[test]
fn lower_tables_match_pcsx2() {
    for index in 0..0x80u32 {
        if index == 0x40 {
            continue;
        }
        let expected = PCSX2_LOWER
            .iter()
            .find(|(i, _)| *i == index)
            .map_or("-", |(_, n)| n);
        assert_eq!(
            name(lower::decode(index << 25).mnemonic()),
            expected.to_ascii_lowercase(),
            "{index:#x}"
        );
    }
    for funct in 0..0x3Cu32 {
        let expected = PCSX2_LOWER_OP
            .iter()
            .find(|(i, _)| *i == funct)
            .map_or("-", |(_, n)| n);
        assert_eq!(
            name(lower::decode(0x8000_0000 | funct).mnemonic()),
            expected.to_ascii_lowercase()
        );
    }
    for (bc, table) in (0u32..).zip(PCSX2_LOWER_SPECIAL) {
        for fd in 0..32u32 {
            let expected = fd
                .checked_sub(12)
                .and_then(|i| table.get(i as usize))
                .copied()
                .unwrap_or("-");
            let op = lower::decode(0x8000_003C | bc | (fd << 6));
            assert_eq!(
                name(op.mnemonic()),
                expected.to_ascii_lowercase(),
                "bc {bc} fd {fd:#x}"
            );
        }
    }
}

#[test]
fn pairs_are_written_with_flags_and_targets() {
    let text = |upper, lower, pc| Pair::from_words(upper, lower).display(pc).to_string();
    // nop | nop (move.xyzw $vf0, $vf0 is the lower no-op)
    assert_eq!(text(0x0000_02FF, 0x8000_033C, 0), "nop | nop");
    assert_eq!(text(0x4000_02FF, 0x8000_033C, 0), "nop[e] | nop");
    // loi: the I flag makes the lower word an immediate.
    let loi = Pair::from_words(0x8000_02FF, 0x3F80_0000);
    assert_eq!(loi.lower_op(), LowerOp::Loi);
    assert_eq!(loi.display(0).to_string(), "nop | loi 0x3F800000");
    // addx.xyzw $vf1, $vf2, $vf3x | xgkick $vi1
    assert_eq!(
        text(0x01E3_1040, 0x8000_0EFC, 0),
        "addx.xyzw $vf1, $vf2, $vf3x | xgkick $vi1"
    );
    // b back one pair from 0x10: the target is 0x10 + 8 - 16.
    assert_eq!(text(0x0000_02FF, 0x4000_07FE, 0x10), "nop | b 0x0008");
    // lq.xyz $vf4, 2($vi5)
    assert_eq!(
        text(0x0000_02FF, 0x01C4_2802, 0),
        "nop | lq.xyz $vf4, 2($vi5)"
    );
}

#[test]
fn reserved_bits_make_halves_invalid() {
    // Upper bits 26..25 are reserved.
    assert!(!Pair::from_words(0x0200_02FF, 0x8000_033C).upper_is_valid());
    // b with a register field set.
    assert!(!Pair::from_words(0x0000_02FF, 0x4001_0001).lower_is_valid());
    assert!(Pair::from_words(0x0000_02FF, 0x4000_0001).is_valid());
}
