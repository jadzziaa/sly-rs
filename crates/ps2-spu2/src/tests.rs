use crate::envelope::{PHASE_ATTACK, PHASE_RELEASE, PHASE_STOPPED, PHASE_SUSTAIN};
use crate::mixer::decode_block;
use crate::{Spu2, revb};

/// A core register's address.
fn reg(core: u32, off: u32) -> u32 {
    0x1F90_0000 + 0x400 * core + off
}

/// A voice parameter's address (VOLL 0, VOLR 2, PITCH 4, ADSR1 6, ADSR2 8,
/// ENVX 0xA, VOLXL 0xC, VOLXR 0xE).
fn vreg(core: u32, voice: u32, param: u32) -> u32 {
    reg(core, voice * 0x10 + param)
}

/// A voice address's register (SSA 0, LSAX 4, NAX 8; high halfword first).
fn areg(core: u32, voice: u32, param: u32) -> u32 {
    reg(core, 0x1C0 + voice * 12 + param)
}

/// The register at 0x760 on: MVOLL 0 .. `IN_COEF_R` 19.
fn ext(core: u32, n: u32) -> u32 {
    0x1F90_0760 + 0x28 * core + 2 * n
}

/// Writes a 20-bit address register (high halfword, then low).
fn write_addr(spu: &mut Spu2, hi_reg: u32, value: u32) {
    spu.write16(hi_reg, (value >> 16) as u16);
    spu.write16(hi_reg + 2, value as u16);
}

/// Puts halfwords into sound memory at `addr` (in halfwords).
fn poke(spu: &mut Spu2, addr: u32, data: &[u16]) {
    for (i, h) in data.iter().enumerate() {
        let at = (addr as usize + i) * 2;
        spu.ram_mut()[at..at + 2].copy_from_slice(&h.to_le_bytes());
    }
}

/// A block: header (shift field and filter in the low byte, flags in the
/// high byte), every data byte the same.
fn block(header: u16, byte: u8) -> [u16; 8] {
    let h = u16::from(byte) | (u16::from(byte) << 8);
    [header, h, h, h, h, h, h, h]
}

/// Sets up `voice` on `core` to play from `start`: full volumes (voice
/// and master), pitch 0x1000 (one sample per sample), given ADSR.
fn setup_voice(spu: &mut Spu2, core: u32, voice: u32, start: u32, adsr1: u16, adsr2: u16) {
    spu.write16(vreg(core, voice, 0), 0x3FFF);
    spu.write16(vreg(core, voice, 2), 0x3FFF);
    spu.write16(vreg(core, voice, 4), 0x1000);
    spu.write16(vreg(core, voice, 6), adsr1);
    spu.write16(vreg(core, voice, 8), adsr2);
    write_addr(spu, areg(core, voice, 0), start);
    spu.write16(ext(core, 0), 0x3FFF);
    spu.write16(ext(core, 1), 0x3FFF);
}

fn key_on(spu: &mut Spu2, core: u32, voice: u32) {
    spu.write16(reg(core, 0x1A0 + 2 * (voice / 16)), 1 << (voice % 16));
}

fn key_off(spu: &mut Spu2, core: u32, voice: u32) {
    spu.write16(reg(core, 0x1A4 + 2 * (voice / 16)), 1 << (voice % 16));
}

fn endx(spu: &mut Spu2, core: u32) -> u32 {
    u32::from(spu.read16(reg(core, 0x340))) | (u32::from(spu.read16(reg(core, 0x342))) << 16)
}

/// Fast attack (shift 0), no decay (shift 15 never steps in the sample it
/// starts), sustain level 15.
const FAST_ADSR1: u16 = 0x00FF;
/// Linear increasing sustain, fast linear release.
const FAST_ADSR2: u16 = 0x0000;

/// Two blocks of a constant 28,672 (nibble 7, shift 0): the first starts
/// the loop, the second ends it and loops.
fn looping_sample(spu: &mut Spu2, at: u32) {
    poke(spu, at, &block(0x0400, 0x77));
    poke(spu, at + 8, &block(0x0300, 0x77));
}

#[test]
fn adpcm_shift_and_nibbles() {
    // Shift field 12, filter 0: each sample is its sign-extended nibble,
    // low nibble first.
    let raw = [0x000C, 0x2301, 0x67F8_u16 as i16, 0, 0, 0, 0, 0x7F00];
    let (mut p1, mut p2) = (0, 0);
    let out = decode_block(&raw, &mut p1, &mut p2);
    assert_eq!(&out[..8], &[1, 0, 3, 2, -8, -1, 7, 6]);
    assert_eq!(&out[24..], &[0, 0, -1, 7]);
    assert_eq!((p1, p2), (7, -1));

    // Shift field 0: nibble 1 is 0x1000, nibble 8 is -0x8000.
    let raw = [0x0000, 0x0081, 0, 0, 0, 0, 0, 0];
    let out = decode_block(&raw, &mut p1, &mut p2);
    assert_eq!(&out[..3], &[0x1000, -0x8000, 0]);
}

#[test]
fn adpcm_prediction_filter() {
    // Filter 1 (60/64 of the last sample) from a history of 64, silent
    // data: (60 * 64 + 32) >> 6 = 60, then 56, 53, 50...
    let raw = [0x001C, 0, 0, 0, 0, 0, 0, 0];
    let (mut p1, mut p2) = (64, 0);
    let out = decode_block(&raw, &mut p1, &mut p2);
    assert_eq!(&out[..4], &[60, 56, 53, 50]);
    assert_eq!(p1, i32::from(out[27]));
    assert_eq!(p2, i32::from(out[26]));

    // Filter 4 (122, -60) clamps at 16 bits: 0x7000 + (122 * 0x7FFF - 60 *
    // 0x7FFF + 32) >> 6 overflows.
    let raw = [0x0040, 0x0007, 0, 0, 0, 0, 0, 0];
    let (mut p1, mut p2) = (0x7FFF, 0x7FFF);
    let out = decode_block(&raw, &mut p1, &mut p2);
    assert_eq!(out[0], 0x7FFF);
}

#[test]
fn looping_voice_plays_and_sets_endx() {
    let mut spu = Spu2::new();
    looping_sample(&mut spu, 0x5000);
    setup_voice(&mut spu, 1, 0, 0x5000, FAST_ADSR1, FAST_ADSR2);
    assert_eq!(endx(&mut spu, 1) & 1, 1, "ENDX starts set");
    key_on(&mut spu, 1, 0);

    let first = spu.step();
    assert_eq!(endx(&mut spu, 1) & 1, 0, "key on clears ENDX");
    assert_eq!(spu.read16(reg(1, 0x1A0)), 0, "KON is taken");
    assert_eq!(spu.read16(vreg(1, 0, 0xA)), 14336, "one attack step");
    assert_ne!(first, [0, 0], "sounds from the first sample");

    let mut loudest = 0;
    let mut looped_at = None;
    for n in 1..400 {
        let out = spu.step();
        loudest = loudest.max(out[0].unsigned_abs());
        if looped_at.is_none() && endx(&mut spu, 1) & 1 != 0 {
            looped_at = Some(n);
        }
    }
    assert!(loudest > 1000, "audible: {loudest}");
    // The FIFO takes a halfword (4 samples) in each of the first 5 samples,
    // then one every 4: the 14th, which ends the second block, at sample 40.
    assert_eq!(looped_at, Some(40), "ENDX set at the loop end");
    assert_ne!(spu.read16(vreg(1, 0, 0xA)), 0, "still playing");
    let nax = u32::from(spu.read16(areg(1, 0, 8))) << 16 | u32::from(spu.read16(areg(1, 0, 10)));
    assert!((0x5000..0x5010).contains(&nax), "NAX in the loop: {nax:#x}");
}

#[test]
fn non_looping_voice_stops() {
    let mut spu = Spu2::new();
    poke(&mut spu, 0x5000, &block(0x0000, 0x77));
    poke(&mut spu, 0x5008, &block(0x0100, 0x77));
    // The loop end goes to LSAX: the BIOS's silent loop.
    setup_voice(&mut spu, 1, 0, 0x5000, FAST_ADSR1, FAST_ADSR2);
    write_addr(&mut spu, areg(1, 0, 4), 0x2800);
    key_on(&mut spu, 1, 0);

    let mut stopped_at = None;
    for n in 0..400 {
        spu.step();
        if stopped_at.is_none() && spu.read16(vreg(1, 0, 0xA)) == 0 {
            stopped_at = Some(n);
        }
    }
    assert_eq!(stopped_at, Some(40), "the voice stops at the loop end");
    assert_eq!(endx(&mut spu, 1) & 1, 1, "ENDX set");
    assert_eq!(spu.cores[1].voices[0].adsr.phase, PHASE_STOPPED);
    assert_eq!(spu.step(), [0, 0], "silent");
    // A stopped voice keeps walking memory: here the BIOS's loop.
    let nax = spu.cores[1].voices[0].next_a;
    assert!((0x2800..0x2808).contains(&nax), "NAX {nax:#x}");
}

#[test]
fn attack_reaches_max_on_time() {
    // ADSR1 0x34FF: linear attack, shift 13, step 0. PCSX2's
    // `V_ADSR::Calculate` adds counter_inc = 0x8000 >> (13 - 11) = 0x2000
    // a sample, so the level steps every 4th sample, by
    // level_inc = (7 - 0) << max(0, 11 - 13) = 7. 0x7FFF / 7 = 4681 steps
    // exactly: 18,724 samples, counting the one that keys on.
    let mut spu = Spu2::new();
    looping_sample(&mut spu, 0x5000);
    setup_voice(&mut spu, 1, 0, 0x5000, 0x34FF, FAST_ADSR2);
    key_on(&mut spu, 1, 0);
    for _ in 0..18_723 {
        spu.step();
    }
    assert_eq!(spu.read16(vreg(1, 0, 0xA)), 32_760);
    assert_eq!(spu.cores[1].voices[0].adsr.phase, PHASE_ATTACK);
    spu.step();
    assert_eq!(spu.read16(vreg(1, 0, 0xA)), 0x7FFF);
    // Decay (shift 15) does not step in its first sample, and the level is
    // already at sustain level 15's 0x8000.
    spu.step();
    assert_eq!(spu.cores[1].voices[0].adsr.phase, PHASE_SUSTAIN);
    assert_eq!(spu.read16(vreg(1, 0, 0xA)), 0x7FFF);
}

#[test]
fn key_off_releases() {
    let mut spu = Spu2::new();
    looping_sample(&mut spu, 0x5000);
    // Release: linear, shift 11 (one step of 8 a sample).
    setup_voice(&mut spu, 1, 0, 0x5000, FAST_ADSR1, 0x000B);
    key_on(&mut spu, 1, 0);
    for _ in 0..10 {
        spu.step();
    }
    assert_eq!(spu.read16(vreg(1, 0, 0xA)), 0x7FFF);
    key_off(&mut spu, 1, 0);
    assert_eq!(spu.read16(reg(1, 0x1A4)), 1, "KOFF waits for the sample");
    spu.step();
    assert_eq!(spu.cores[1].voices[0].adsr.phase, PHASE_RELEASE);
    // Exponential decrease is off: -8 << 0 a sample.
    assert_eq!(spu.read16(vreg(1, 0, 0xA)), 0x7FFF - 8);
    for _ in 0..=0x7FF7 / 8 {
        spu.step();
    }
    assert_eq!(spu.read16(vreg(1, 0, 0xA)), 0);
    assert_eq!(spu.cores[1].voices[0].adsr.phase, PHASE_STOPPED);
}

#[test]
fn irqa_on_a_voice_raises_the_interrupt() {
    let mut spu = Spu2::new();
    looping_sample(&mut spu, 0x5000);
    setup_voice(&mut spu, 1, 0, 0x5000, FAST_ADSR1, FAST_ADSR2);
    // Core 0 watches the second block; the voice is core 1's: either
    // core's IRQ sees every voice.
    write_addr(&mut spu, reg(0, 0x19C), 0x5008);
    spu.write16(reg(0, 0x19A), 0x0040);
    key_on(&mut spu, 1, 0);
    let mut raised = None;
    for n in 0..100 {
        spu.step();
        if spu.take_irq()[0] {
            raised = Some(n);
            break;
        }
    }
    assert!(raised.is_some(), "IRQ raised");
    assert_eq!(spu.read16(0x1F90_07C2) & 4, 4, "SPDIF IRQ info bit");
    // Once until the game clears the bit.
    for _ in 0..200 {
        spu.step();
    }
    assert_eq!(spu.take_irq(), [false, false]);
    // Turning the interrupt off clears the bit; on again, the next pass
    // raises it.
    spu.write16(reg(0, 0x19A), 0);
    assert_eq!(spu.read16(0x1F90_07C2) & 4, 0);
    spu.write16(reg(0, 0x19A), 0x0040);
    for _ in 0..200 {
        spu.step();
    }
    assert_eq!(spu.take_irq(), [true, false]);
}

#[test]
fn irqa_on_the_data_port() {
    let mut spu = Spu2::new();
    write_addr(&mut spu, reg(1, 0x19C), 0x1_2345);
    spu.write16(reg(1, 0x19A), 0x0040);
    write_addr(&mut spu, reg(0, 0x1A8), 0x1_2344);
    spu.write16(reg(0, 0x1AC), 0xAAAA);
    assert_eq!(spu.take_irq(), [false, false]);
    spu.write16(reg(0, 0x1AC), 0xBBBB);
    assert_eq!(spu.take_irq(), [false, true]);
    assert_eq!(&spu.ram()[0x2_4688..0x2_468C], &[0xAA, 0xAA, 0xBB, 0xBB]);
    assert_eq!(spu.read16(reg(0, 0x1A8)), 1);
    assert_eq!(spu.read16(reg(0, 0x1AA)), 0x2346);
}

#[test]
fn registers_read_back_as_989snd_writes_them() {
    let mut spu = Spu2::new();
    // Voice 5 of core 1.
    spu.write16(vreg(1, 5, 0), 0x3FFF);
    spu.write16(vreg(1, 5, 2), 0x1234);
    spu.write16(vreg(1, 5, 4), 0x0800);
    spu.write16(vreg(1, 5, 6), 0x80FF);
    spu.write16(vreg(1, 5, 8), 0x5FC3);
    assert_eq!(spu.read16(vreg(1, 5, 0)), 0x3FFF);
    assert_eq!(spu.read16(vreg(1, 5, 2)), 0x1234);
    assert_eq!(
        spu.read16(vreg(1, 5, 0xC)),
        0x7FFE,
        "VOLXL: the volume doubled"
    );
    assert_eq!(spu.read16(vreg(1, 5, 0xE)), 0x2468);
    assert_eq!(spu.read16(vreg(1, 5, 4)), 0x0800);
    assert_eq!(spu.read16(vreg(1, 5, 6)), 0x80FF);
    assert_eq!(spu.read16(vreg(1, 5, 8)), 0x5FC3);
    // VOLX is read-only; ENVX is not.
    spu.write16(vreg(1, 5, 0xC), 0);
    assert_eq!(spu.read16(vreg(1, 5, 0xC)), 0x7FFE);
    spu.write16(vreg(1, 5, 0xA), 0x1234);
    assert_eq!(spu.read16(vreg(1, 5, 0xA)), 0x1234);

    // Addresses keep 8-halfword alignment; NAX reads with bit 0 set.
    write_addr(&mut spu, areg(1, 5, 0), 0x2_3457);
    write_addr(&mut spu, areg(1, 5, 4), 0x2_3460);
    write_addr(&mut spu, areg(1, 5, 8), 0x2_3470);
    assert_eq!(spu.read16(areg(1, 5, 0)), 2);
    assert_eq!(spu.read16(areg(1, 5, 2)), 0x3450);
    assert_eq!(spu.read16(areg(1, 5, 4)), 2);
    assert_eq!(spu.read16(areg(1, 5, 6)), 0x3460);
    assert_eq!(spu.read16(areg(1, 5, 8)), 2);
    assert_eq!(spu.read16(areg(1, 5, 10)), 0x3471);

    // Mixing and core registers.
    for (off, value) in [
        (0x188, 0xFFFF),
        (0x18A, 0x00FF),
        (0x18C, 0x0001),
        (0x190, 0x8000),
        (0x194, 0x0000),
        (0x198, 0x0FF0),
        (0x19A, 0xC080),
        (0x180, 0x0006),
        (0x184, 0x0010),
    ] {
        spu.write16(reg(0, off), value);
        assert_eq!(spu.read16(reg(0, off)), value, "{off:#x}");
    }
    assert!(spu.cores[0].voices[1].modulated && spu.cores[0].voices[2].modulated);
    assert!(spu.cores[0].voices[4].noise);
    assert!(spu.cores[0].fx_enable);
    write_addr(&mut spu, reg(0, 0x19C), 0x0_1000);
    assert_eq!(spu.read16(reg(0, 0x19E)), 0x1000);
    write_addr(&mut spu, reg(0, 0x1A8), 0x0_2000);
    assert_eq!(spu.read16(reg(0, 0x1AA)), 0x2000);

    // ESA, the reverb's offsets and EEA (its low halfword ignores writes).
    write_addr(&mut spu, reg(0, 0x2E0), 0x0_E000);
    assert_eq!(spu.read16(reg(0, 0x2E2)), 0xE000);
    write_addr(
        &mut spu,
        reg(0, 0x2E4 + 4 * revb::APF1_SIZE as u32),
        0x0_01FB,
    );
    assert_eq!(spu.cores[0].revb_addr[revb::APF1_SIZE], 0x01FB);
    spu.write16(reg(0, 0x33C), 0x000E);
    spu.write16(reg(0, 0x33E), 0x1234);
    assert_eq!(spu.read16(reg(0, 0x33C)), 0x000E);
    assert_eq!(spu.read16(reg(0, 0x33E)), 0xFFFF);
    // Core 1's write table swaps the DIFF sources (PCSX2's quirk).
    write_addr(&mut spu, reg(1, 0x324), 0x0_0100);
    assert_eq!(spu.cores[1].revb_addr[revb::DIFF_R_SRC], 0x0100);
    assert_eq!(spu.read16(reg(1, 0x32A)), 0x0100);

    // Master, effect and reverb volumes.
    for n in 0..20 {
        if n == 8 || n == 9 {
            continue;
        }
        spu.write16(ext(1, n), 0x1000 + n as u16);
        assert_eq!(spu.read16(ext(1, n)), 0x1000 + n as u16, "volume {n}");
    }
    assert_eq!(spu.read16(ext(1, 8)), 0x2000, "MVOLXL");

    // Writing ENDX clears it.
    assert_eq!(spu.read16(reg(0, 0x340)), 0xFFFF);
    spu.write16(reg(0, 0x340), 0);
    assert_eq!(spu.read16(reg(0, 0x340)), 0);
    assert_eq!(spu.read16(reg(0, 0x342)), 0x00FF);

    // Unmapped space reads back what was written.
    spu.write16(0x1F90_07E0, 0xBEEF);
    assert_eq!(spu.read16(0x1F90_07E0), 0xBEEF);
}

/// Core 1, voice 0 into the reverb only; the reverb's buffers chained so
/// its output comes back within a few hundred samples.
fn reverb_setup(spu: &mut Spu2, fx: bool) {
    looping_sample(spu, 0x5000);
    setup_voice(spu, 1, 0, 0x5000, FAST_ADSR1, FAST_ADSR2);
    for off in [0x188, 0x18A, 0x190, 0x192] {
        spu.write16(reg(1, off), 0);
    }
    for off in [0x18C, 0x194] {
        spu.write16(reg(1, off), 1);
    }
    write_addr(spu, reg(1, 0x2E0), 0xF_0000);
    // EEA: the top of memory (its low halfword is always 0xFFFF).
    spu.write16(reg(1, 0x33C), 0x000F);
    let mut offset = |n: usize, value: u32| write_addr(spu, reg(1, 0x2E4 + 4 * n as u32), value);
    offset(revb::SAME_L_DST, 0x100);
    offset(revb::SAME_R_DST, 0x180);
    offset(revb::COMB1_L_SRC, 0xF8);
    offset(revb::COMB1_R_SRC, 0x178);
    offset(revb::APF1_L_DST, 0x400);
    offset(revb::APF1_R_DST, 0x480);
    offset(revb::APF1_SIZE, 0x8);
    offset(revb::APF2_L_DST, 0x500);
    offset(revb::APF2_R_DST, 0x580);
    offset(revb::APF2_SIZE, 0x8);
    // EVOLL/R, IIR, COMB1, IN_COEF_L/R.
    for (n, value) in [
        (2, 0x7FFF),
        (3, 0x7FFF),
        (10, 0x7FFF),
        (11, 0x7FFF),
        (18, 0x7FFF),
        (19, 0x7FFF),
    ] {
        spu.write16(ext(1, n), value);
    }
    spu.write16(reg(1, 0x19A), if fx { 0x0080 } else { 0 });
    key_on(spu, 1, 0);
}

#[test]
fn reverb_carries_a_wet_voice() {
    let mut spu = Spu2::new();
    reverb_setup(&mut spu, true);
    let mut first = None;
    for n in 0..2000 {
        let out = spu.step();
        if first.is_none() && out[0] != 0 && out[1] != 0 {
            first = Some(n);
        }
    }
    let first = first.expect("the reverb sounds");
    assert!(first > 30, "through the buffer's delays: {first}");
    // The wet mix went to sound memory (core 1's at 0x1C00).
    let wet = spu.mem(0x1C00 + 100);
    assert!(wet > 1000, "wet voice mix {wet}");

    // Without the effect enable bit the reverb writes nothing back: silence.
    let mut spu = Spu2::new();
    reverb_setup(&mut spu, false);
    for _ in 0..2000 {
        assert_eq!(spu.step(), [0, 0]);
    }
}

#[test]
fn plain_dma_and_statx() {
    let mut spu = Spu2::new();
    assert_eq!(spu.read16(reg(0, 0x344)) & 0x80, 0x80, "power-on STATX");
    // libsd sets a DMA mode in ATTR before a transfer.
    spu.write16(reg(0, 0x19A), 0x0020);
    write_addr(&mut spu, reg(0, 0x1A8), 0x6000);
    spu.set_dma_busy(0, true);
    assert_eq!(spu.read16(reg(0, 0x344)) & 0x80, 0);
    let data: Vec<u16> = (0..0x500).collect();
    spu.dma_write(0, &data);
    spu.set_dma_busy(0, false);
    assert_eq!(spu.read16(reg(0, 0x344)) & 0x80, 0x80);
    assert_eq!(spu.read16(reg(0, 0x1AA)), 0x6500, "TSA follows");
    assert_eq!(spu.mem(0x6000 + 0x4FF), 0x4FF);

    write_addr(&mut spu, reg(0, 0x1A8), 0x6000);
    assert_eq!(spu.dma_read(0, 0x500), data);

    // Passing IRQA is raised when the transfer ends.
    write_addr(&mut spu, reg(1, 0x19C), 0x6123);
    spu.write16(reg(1, 0x19A), 0x0040);
    write_addr(&mut spu, reg(0, 0x1A8), 0x6000);
    spu.set_dma_busy(0, true);
    spu.dma_write(0, &data);
    assert_eq!(spu.take_irq(), [false, false]);
    spu.set_dma_busy(0, false);
    assert_eq!(spu.take_irq(), [false, true]);
}

#[test]
fn autodma_fills_the_input_buffer() {
    let mut spu = Spu2::new();
    spu.write16(reg(0, 0x1B0), 1);
    assert!(spu.adma_wants_data(0));
    assert!(!spu.adma_wants_data(1));
    for _ in 0..0x10 {
        spu.step();
    }
    // Starved at output position 0: ADMAS reads back with its high bits
    // set.
    assert_eq!(spu.read16(reg(0, 0x1B0)), 0xFFFD);
    // Two blocks, each 0x100 halfwords left then 0x100 right.
    let data: Vec<u16> = (0..0x400).map(|i| i as u16 | 0x8000).collect();
    spu.dma_write(0, &data);
    assert!(!spu.adma_wants_data(0));
    assert_eq!(spu.read16(reg(0, 0x1B0)), 1);
    // Output is in the buffer's first half: the first block goes straight
    // into the second.
    assert_eq!(spu.mem(0x2100) as u16, 0x8000);
    assert_eq!(spu.mem(0x2300) as u16, 0x8100);
    let mut wanted_at = None;
    for _ in 0..0x200 {
        let pos = spu.out_pos;
        spu.step();
        if wanted_at.is_none() && spu.adma_wants_data(0) {
            wanted_at = Some(pos);
        }
    }
    // The second block goes into the first half when output reaches 0x100,
    // and the transfer completes three samples later.
    assert_eq!(spu.mem(0x2000) as u16, 0x8200);
    assert_eq!(spu.mem(0x2200) as u16, 0x8300);
    assert_eq!(wanted_at, Some(0x103));
    assert_eq!(spu.read16(reg(0, 0x1B0)), 0xFFFD, "starved again at 0x180");
}
