use super::*;
use ps2_gs::Gs;

const ONE: u32 = 0x3F80_0000;
const TWO: u32 = 0x4000_0000;
const FOUR: u32 = 0x4080_0000;

/// An upper NOP, and a lower one (`move vf0, vf0`).
const UNOP: u32 = 0x0000_02FF;
const LNOP: u32 = 0x8000_033C;

/// Assembles pairs (upper, lower) into micro memory, checking each half
/// decodes as the mnemonic given.
fn program(pairs: &[(u32, &str, u32, &str)]) -> Vec<u8> {
    let mut code = vec![0; 0x4000];
    for (k, &(upper, um, lower, lm)) in pairs.iter().enumerate() {
        let pair = Pair::from_words(upper, lower);
        assert_eq!(pair.upper_op().mnemonic(), um, "upper of pair {k}");
        if upper & FLAG_I == 0 {
            assert_eq!(pair.lower_op().mnemonic(), lm, "lower of pair {k}");
        }
        code[8 * k..8 * k + 4].copy_from_slice(&lower.to_le_bytes());
        code[8 * k + 4..8 * k + 8].copy_from_slice(&upper.to_le_bytes());
    }
    code
}

fn exec(code: &[u8], regs: &mut Regs, data: &mut [u8], gs: Option<&mut Gs>) -> Run {
    let vu1 = gs.map(|gs| Vu1Link {
        itop: 0,
        top: 0,
        gs,
    });
    run(
        &mut Unit {
            regs,
            code,
            data,
            vu1,
            cache: &mut Cache::default(),
        },
        0,
    )
}

const E: u32 = FLAG_E;
/// `add.xyzw fd, fs, ft`.
const fn add(fd: u32, fs: u32, ft: u32) -> u32 {
    0x01E0_0028 | (ft << 16) | (fs << 11) | (fd << 6)
}
/// `addq.xyzw fd, fs, q`.
const fn addq(fd: u32, fs: u32) -> u32 {
    0x01E0_0020 | (fs << 11) | (fd << 6)
}
/// `div q, fs.x, ft.x`.
const fn div(fs: u32, ft: u32) -> u32 {
    0x8000_03BC | (ft << 16) | (fs << 11)
}
/// `iaddi it, is, imm`.
const fn iaddi(it: u32, is: u32, imm: u32) -> u32 {
    0x8000_0032 | (it << 16) | (is << 11) | ((imm & 31) << 6)
}
/// `move.xyzw ft, fs`.
const fn mov(ft: u32, fs: u32) -> u32 {
    0x81E0_033C | (ft << 16) | (fs << 11)
}

#[test]
fn q_arrives_after_the_divider_latency() {
    let mut regs = crate::reset_regs();
    regs.vf[1] = [FOUR; 4];
    regs.vf[2] = [TWO; 4];
    regs.q = ONE;
    let code = program(&[
        (UNOP, "nop", div(1, 2), "div"),
        // Read Q at once: still the old 1.0.
        (addq(3, 0), "addq", LNOP, "move"),
        (UNOP, "nop", 0x8000_03BF, "waitq"),
        (addq(4, 0) | E, "addq", LNOP, "move"),
        (UNOP, "nop", LNOP, "move"),
    ]);
    let run = exec(&code, &mut regs, &mut vec![0; 0x4000], None);
    assert!(!run.runaway);
    assert_eq!(regs.vf[3][0], ONE);
    assert_eq!(regs.vf[4][0], TWO);
    assert_eq!(regs.vf[4][3], 0x4040_0000, "w: 1.0 + 2.0");
}

#[test]
fn branches_run_their_delay_slot() {
    let mut regs = crate::reset_regs();
    // b to pair 3 (pc + 8 + 2 * 8); the delay slot sets vi1; pair 2 is
    // skipped.
    let b = 0x4000_0000 | 2;
    let code = program(&[
        (UNOP, "nop", b, "b"),
        (UNOP, "nop", iaddi(1, 0, 5), "iaddi"),
        (UNOP, "nop", iaddi(2, 0, 7), "iaddi"),
        (UNOP | E, "nop", iaddi(3, 0, 9), "iaddi"),
        (UNOP, "nop", LNOP, "move"),
    ]);
    exec(&code, &mut regs, &mut vec![0; 0x4000], None);
    assert_eq!(regs.vi[1..4], [5, 0, 9]);
    assert_eq!(regs.tpc, 40);
}

#[test]
fn halves_read_before_either_writes_and_upper_wins() {
    let mut regs = crate::reset_regs();
    regs.vf[1] = [ONE; 4];
    regs.vf[2] = [TWO; 4];
    let code = program(&[
        // Upper vf3 = vf1 + vf1; lower vf1 = vf2, which the upper does not
        // see.
        (add(3, 1, 1), "add", mov(1, 2), "move"),
        // Both write vf4: the upper's value stays.
        (add(4, 0, 0) | E, "add", mov(4, 2), "move"),
        (UNOP, "nop", LNOP, "move"),
    ]);
    exec(&code, &mut regs, &mut vec![0; 0x4000], None);
    assert_eq!(regs.vf[3], [TWO; 4]);
    assert_eq!(regs.vf[1], [TWO; 4]);
    assert_eq!(regs.vf[4], [0, 0, 0, TWO], "vf0 + vf0");
}

#[test]
fn xgkick_sends_a_packet_to_path1() {
    let mut regs = crate::reset_regs();
    let mut data = vec![0u8; 0x4000];
    // At qword 2: a PACKED A+D tag with EOP, one loop, writing SIGNAL.
    let tag: u128 = 1 | (1 << 15) | (1 << 60) | (0xE << 64);
    let ad: u128 = 0x1234 | (0x60 << 64);
    data[32..48].copy_from_slice(&tag.to_le_bytes());
    data[48..64].copy_from_slice(&ad.to_le_bytes());
    regs.vi[5] = 2;
    let code = program(&[
        (UNOP | E, "nop", 0x8000_06FC | (5 << 11), "xgkick"),
        (UNOP, "nop", LNOP, "move"),
    ]);
    let mut gs = Gs::default();
    exec(&code, &mut regs, &mut data, Some(&mut gs));
    assert_eq!(gs.take_events().signal, Some(0x1234));
}
