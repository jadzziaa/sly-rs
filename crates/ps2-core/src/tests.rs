//! Instruction semantics on hand-made encodings.

use ps2_isa::ee::Insn;

use super::*;

fn run(cpu: &mut Cpu, mem: &mut Memory, words: &[u32]) {
    for &word in words {
        let pc = cpu.pc;
        assert_eq!(
            execute(cpu, mem, Insn::decode(word), pc),
            Outcome::Next,
            "{word:08x}"
        );
        cpu.pc += 4;
    }
}

fn machine() -> (Cpu, Memory) {
    (Cpu::default(), Memory::new(Box::new(NoMmio)))
}

#[test]
fn alu_sign_extends_32_bit_results() {
    let (mut cpu, mut mem) = machine();
    // lui $v0, 0x8000 ; addiu $v0, $v0, -1 (0x7FFFFFFF after wrap) ; daddiu $v1, $zero, -1
    run(&mut cpu, &mut mem, &[0x3C02_8000, 0x2442_FFFF, 0x6403_FFFF]);
    assert_eq!(cpu.r64(2), 0x7FFF_FFFF);
    assert_eq!(cpu.r64(3), u64::MAX);
    // The upper 64 bits of a GPR survive 64-bit writes.
    cpu.gpr[4] = 0xAAAA_AAAA_AAAA_AAAA_0000_0000_0000_0000;
    run(&mut cpu, &mut mem, &[0x2404_0005]); // addiu $a0, $zero, 5
    assert_eq!(cpu.gpr[4], 0xAAAA_AAAA_AAAA_AAAA_0000_0000_0000_0005);
}

#[test]
fn loads_and_stores_including_unaligned() {
    let (mut cpu, mut mem) = machine();
    mem.write64(0x1000, 0x8877_6655_4433_2211);
    cpu.gpr[4] = 0x1000;
    // lw $v0, 0($a0) ; lb $v1, 7($a0) ; lhu $a1, 6($a0)
    run(&mut cpu, &mut mem, &[0x8C82_0000, 0x8083_0007, 0x9485_0006]);
    assert_eq!(cpu.r64(2), 0x4433_2211);
    assert_eq!(cpu.r64(3), 0xFFFF_FFFF_FFFF_FF88);
    assert_eq!(cpu.r64(5), 0x8877);
    // lwl $a2, 4($a0) ; lwr $a2, 1($a0): the word at 0x1001.
    run(&mut cpu, &mut mem, &[0x8886_0004, 0x9886_0001]);
    assert_eq!(cpu.r32(6), 0x5544_3322);
    // ldl $a3, 8($a0) ; ldr $a3, 1($a0): the doubleword at 0x1001.
    mem.write64(0x1008, 0x0000_0000_0000_0099);
    run(&mut cpu, &mut mem, &[0x6887_0008, 0x6C87_0001]);
    assert_eq!(cpu.r64(7), 0x9988_7766_5544_3322);
    // sq/lq ignore the low four address bits.
    cpu.gpr[8] = 0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210;
    run(&mut cpu, &mut mem, &[0x7C88_0025, 0x7889_0020]); // sq $t0, 0x25($a0) ; lq $t1, 0x20($a0)
    assert_eq!(cpu.gpr[9], cpu.gpr[8]);
}

#[test]
fn branches_report_their_targets() {
    let (mut cpu, mut mem) = machine();
    cpu.pc = 0x100;
    let outcome = execute(&mut cpu, &mut mem, Insn::decode(0x1000_0003), 0x100); // b +3
    assert_eq!(
        outcome,
        Outcome::Branch {
            target: 0x110,
            taken: true,
            likely: false
        }
    );
    let jal = execute(&mut cpu, &mut mem, Insn::decode(0x0C00_0100), 0x100);
    assert_eq!(
        jal,
        Outcome::Branch {
            target: 0x400,
            taken: true,
            likely: false
        }
    );
    assert_eq!(cpu.r64(31), 0x108);
}

#[test]
fn multiply_and_divide() {
    let (mut cpu, mut mem) = machine();
    cpu.gpr[4] = (-7_i64).cast_unsigned().into();
    cpu.gpr[5] = 2;
    // mult $v0, $a0, $a1 ; div $zero, $a0, $a1 ; mflo $v1 ; mfhi $a2
    run(
        &mut cpu,
        &mut mem,
        &[0x0085_1018, 0x0085_001A, 0x0000_1812, 0x0000_3010],
    );
    assert_eq!(cpu.r64(2).cast_signed(), -14);
    assert_eq!(cpu.r64(3).cast_signed(), -3);
    assert_eq!(cpu.r64(6).cast_signed(), -1);
}

#[test]
fn mmi_lanes() {
    let (mut cpu, mut mem) = machine();
    cpu.gpr[4] = 0x0000_0004_0000_0003_0000_0002_0000_0001;
    cpu.gpr[5] = 0x0000_0040_0000_0030_0000_0020_0000_0010;
    // paddw $v0, $a0, $a1 ; pextlw $v1, $a0, $a1 ; pcpyld $a2, $a0, $a1
    run(&mut cpu, &mut mem, &[0x7085_1008, 0x7085_1C88, 0x7085_3389]);
    assert_eq!(cpu.gpr[2], 0x0000_0044_0000_0033_0000_0022_0000_0011);
    assert_eq!(cpu.gpr[3], 0x0000_0002_0000_0020_0000_0001_0000_0010);
    assert_eq!(cpu.gpr[6], 0x0000_0002_0000_0001_0000_0020_0000_0010);
}

#[test]
fn fpu_and_vu0_macro() {
    let (mut cpu, mut mem) = machine();
    cpu.fpr[0] = 0x3F80_0000; // 1.0
    cpu.fpr[1] = 0x4000_0000; // 2.0
    // add.s $f2, $f0, $f1 ; c.lt.s $f0, $f1
    run(&mut cpu, &mut mem, &[0x4601_0080, 0x4601_0034]);
    assert_eq!(cpu.fpr[2], 0x4040_0000);
    assert_ne!(cpu.fcr31 & (1 << 23), 0);
    // vf1 = (1, 2, 3, 4); vadd.xyzw $vf2, $vf1, $vf1 ; vmulx.xyz $vf3, $vf1, $vf1x
    cpu.vu0.vf[1] = [0x3F80_0000, 0x4000_0000, 0x4040_0000, 0x4080_0000];
    run(&mut cpu, &mut mem, &[0x4BE1_08A8, 0x4BC1_08D8]);
    assert_eq!(
        cpu.vu0.vf[2],
        [0x4000_0000, 0x4080_0000, 0x40C0_0000, 0x4100_0000]
    );
    assert_eq!(cpu.vu0.vf[3], [0x3F80_0000, 0x4000_0000, 0x4040_0000, 0]);
    // vdiv Q, $vf1x, $vf1y: 1 / 2
    run(&mut cpu, &mut mem, &[0x4A81_0BBC]);
    assert_eq!(cpu.vu0.q, 0x3F00_0000);
}

#[test]
fn mmi_range_is_exactly_the_mmi_table() {
    use ps2_isa::ee::{Format, Op};
    let in_range: Vec<Op> = Op::ALL
        .iter()
        .copied()
        .filter(|&op| mmi::handles(op))
        .collect();
    assert_eq!(in_range.first(), Some(&Op::Madd));
    assert_eq!(in_range.last(), Some(&Op::Pexcw));
    assert_eq!(in_range.len(), 103);
    assert!(in_range.iter().all(|op| op.format() != Format::None));
}
