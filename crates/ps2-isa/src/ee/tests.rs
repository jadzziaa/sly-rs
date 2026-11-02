//! Hand-made encodings; the exhaustive comparison with rabbitizer is in
//! `tests/rabbitizer.rs`.

use super::*;

fn text(word: u32, pc: u32) -> String {
    Insn::decode(word).display(pc).to_string()
}

#[test]
fn common_instructions() {
    let cases = [
        (0x0000_0000, "nop"),
        (0x27BD_FFE0, "addiu $sp, $sp, -0x20"),
        (0x3C02_0028, "lui $v0, 0x28"),
        (0x3421_8000, "ori $at, $at, 0x8000"),
        (0x7FBF_0010, "sq $ra, 0x10($sp)"),
        (0xDFB0_FFF8, "ld $s0, -0x8($sp)"),
        (0x0003_1880, "sll $v1, $v1, 2"),
        (0x0080_E02D, "daddu $gp, $a0, $zero"),
        (0x03E0_0008, "jr $ra"),
        (0x0040_F809, "jalr $v0"),
        (0x0060_2009, "jalr $a0, $v1"),
        (0x0000_000C, "syscall 0"),
        (0x0000_01CD, "break 0, 7"),
        (0x0000_000F, "sync"),
        (0x0000_040F, "sync.p"),
        (0x00A8_0019, "multu $a1, $t0"),
        (0x0046_0018, "mult $zero, $v0, $a2"),
        (0x0044_2018, "mult $a0, $v0, $a0"),
        (0x7043_0000, "madd $zero, $v0, $v1"),
        (0x7062_1389, "pcpyld $v0, $v1, $v0"),
        (0x7019_C874, "psllh $t9, $t9, 1"),
        (0x4200_0018, "eret"),
        (0x4002_4800, "mfc0 $v0, $9"),
        (0x4600_6300, "add.s $f12, $f12, $f0"),
        (0x4600_0144, "sqrt.s $f5, $f0"),
        (0x4600_0832, "c.eq.s $f1, $f0"),
        (0xC7AC_0034, "lwc1 $f12, 0x34($sp)"),
        (0x4822_0800, "qmfc2.ni $v0, $vf1"),
        (0x4842_0001, "cfc2.i $v0, $vi0"),
        (0xD8A1_0000, "lqc2 $vf1, 0x0($a1)"),
        (0x4BE1_233C, "vmove.xyzw $vf1, $vf4"),
        (0x4A61_03BC, "vdiv Q, $vf0w, $vf1x"),
        (0x4A01_03BD, "vsqrt Q, $vf1x"),
        (0x4A00_02FF, "vnop"),
        (0x4A00_03BF, "vwaitq"),
        (0x4A00_00B8, "vcallms 0x10"),
        (0x4BC2_0AFE, "vopmula.xyz ACC, $vf1, $vf2"),
    ];
    for (word, expected) in cases {
        let insn = Insn::decode(word);
        assert!(insn.is_valid(), "{word:08x} should be valid");
        assert_eq!(text(word, 0x0010_0000), expected, "{word:08x}");
    }
}

#[test]
fn branch_and_jump_targets() {
    // beq $v0, $zero, . (a branch to itself)
    assert_eq!(text(0x1040_FFFF, 0x0010_0010), "beq $v0, $zero, 0x00100010");
    assert_eq!(
        Insn::decode(0x1040_FFFF).to_string(),
        "beq $v0, $zero, . + 4 + -0x4"
    );
    // jal 0x001F9EA8 from the 0x0... region
    assert_eq!(text(0x0C07_E7AA, 0x0010_0000), "jal 0x001F9EA8");
    assert_eq!(
        Insn::decode(0x0C07_E7AA).jump_target(0x8000_0000),
        0x801F_9EA8
    );
}

#[test]
fn control_flow_kinds() {
    let flow = |word| Insn::decode(word).flow();
    assert_eq!(flow(0x03E0_0008), Flow::JumpReg);
    assert_eq!(flow(0x0040_F809), Flow::JumpRegLink);
    assert_eq!(flow(0x5040_0005), Flow::BranchLikely);
    assert!(flow(0x5040_0005).is_likely() && flow(0x5040_0005).has_delay_slot());
    assert!(flow(0x0C07_E7AA).links());
    assert!(!flow(0x4200_0018).has_delay_slot());
    assert_eq!(flow(0x2442_B300), Flow::Next);
}

#[test]
fn reserved_bits_make_words_invalid() {
    // dsra32 with a non-zero rs field.
    let insn = Insn::decode(0x00FF_00FF);
    assert_eq!(insn.op(), Op::Dsra32);
    assert!(!insn.is_valid());
    assert_eq!(insn.display(0).to_string(), ".word 0x00FF00FF");
    // Unknown primary opcode.
    assert_eq!(Insn::decode(0x7400_0000).op(), Op::Invalid);
    // lui with a non-zero rs field.
    assert!(!Insn::decode(0x3C22_0028).is_valid());
}

#[test]
fn mnemonics_are_unique() {
    let mut names: Vec<_> = Op::ALL.iter().map(|op| op.mnemonic()).collect();
    names.sort_unstable();
    let total = names.len();
    names.dedup();
    assert_eq!(names.len(), total);
}

/// Decoding depends only on the bits an operation's class fixes: clearing
/// every other bit of a valid word keeps its operation. Exhaustive over all
/// 2^32 words; run with `cargo xtask test-full`.
#[test]
#[ignore = "exhaustive: cargo xtask test-full"]
fn decoding_depends_only_on_fixed_bits() {
    for_all_words(|word| {
        let insn = Insn::decode(word);
        if insn.is_valid() && !matches!(insn.op(), Op::Nop | Op::Sll) {
            let fixed = Insn::decode(word & insn.op().class().fixed_mask());
            assert_eq!(fixed.op(), insn.op(), "{word:08x}");
        }
    });
}

/// Runs `check` on every 32-bit word, spread over all cores.
pub(crate) fn for_all_words(check: impl Fn(u32) + Sync) {
    let threads = std::thread::available_parallelism().map_or(8, |n| n.get() as u32);
    let chunk = (1u64 << 32).div_ceil(u64::from(threads));
    std::thread::scope(|scope| {
        for t in 0..u64::from(threads) {
            let check = &check;
            scope.spawn(move || {
                let start = t * chunk;
                let end = ((t + 1) * chunk).min(1 << 32);
                for word in start..end {
                    check(word as u32);
                }
            });
        }
    });
}

#[test]
fn destination_registers() {
    let dest = |word| Insn::decode(word).gpr_dest();
    assert_eq!(dest(0x27BD_FFE0), Some(29)); // addiu $sp
    assert_eq!(dest(0x8C44_0000), Some(4)); // lw $a0
    assert_eq!(dest(0xAFA2_0020), None); // sw
    assert_eq!(dest(0x0080_E02D), Some(28)); // daddu $gp
    assert_eq!(dest(0x0C07_E7AA), Some(31)); // jal
    assert_eq!(dest(0x0060_2009), Some(4)); // jalr $a0, $v1
    assert_eq!(dest(0x4405_0000), Some(5)); // mfc1 $a1
    assert_eq!(dest(0x4481_0800), None); // mtc1
    assert_eq!(dest(0x4822_0800), Some(2)); // qmfc2.ni $v0
    assert_eq!(dest(0x0000_0000), None); // nop
    assert_eq!(dest(0x0043_001A), None); // div
}
