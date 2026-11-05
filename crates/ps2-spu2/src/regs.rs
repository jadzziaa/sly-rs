//! Register reads and writes, as PCSX2's `RegTable.cpp` (reads) and
//! `spu2sys.cpp`'s write handlers (`tbl_reg_writes`) map them. The two
//! tables differ in places; both are kept.

use crate::{Spu2, spdif};

/// What a register address holds, as PCSX2's `regtable` points at it.
#[derive(Clone, Copy, Debug)]
enum Slot {
    /// VOLL or VOLR as written (0 left, 1 right).
    VoiceVol(usize, usize, usize),
    Pitch(usize, usize),
    Adsr1(usize, usize),
    Adsr2(usize, usize),
    /// ENVX: the envelope's level.
    Envx(usize, usize),
    /// VOLXL or VOLXR: the current volume.
    VoiceVolX(usize, usize, usize),
    /// SSA, LSAX, NAX (0, 1, 2), high (true) or low halfword.
    VoiceAddr(usize, usize, usize, bool),
    Pmon(usize, bool),
    Non(usize, bool),
    Vmixl(usize, bool),
    Vmixel(usize, bool),
    Vmixr(usize, bool),
    Vmixer(usize, bool),
    Mmix(usize),
    Attr(usize),
    Irqa(usize, bool),
    KeyOn(usize, bool),
    KeyOff(usize, bool),
    Tsa(usize, bool),
    Admas(usize),
    Esa(usize, bool),
    /// One of the reverb's buffer offsets.
    RevbAddr(usize, usize, bool),
    Eea(usize, bool),
    Endx(usize, bool),
    Statx(usize),
    MasterVol(usize, usize),
    FxVol(usize, usize),
    ExtVol(usize, usize),
    InpVol(usize, usize),
    /// MVOLXL or MVOLXR: the current master volume.
    MasterVolX(usize, usize),
    /// One of the reverb's volumes.
    RevbVol(usize, usize),
    Spdif(usize),
    /// Nothing behind it: reads back what was written.
    Raw(u32),
}

/// The slot a register address (0..0x800, even) reads from.
fn slot(mem: u32) -> Slot {
    if mem >= 0x760 {
        return ext_slot(mem);
    }
    let core = usize::from(mem & 0x400 != 0);
    let off = mem & 0x3FF;
    match off {
        0x000..0x180 => {
            let voice = (off >> 4) as usize;
            match (off & 0xF) >> 1 {
                0 => Slot::VoiceVol(core, voice, 0),
                1 => Slot::VoiceVol(core, voice, 1),
                2 => Slot::Pitch(core, voice),
                3 => Slot::Adsr1(core, voice),
                4 => Slot::Adsr2(core, voice),
                5 => Slot::Envx(core, voice),
                6 => Slot::VoiceVolX(core, voice, 0),
                _ => Slot::VoiceVolX(core, voice, 1),
            }
        }
        0x180 | 0x182 => Slot::Pmon(core, off == 0x182),
        0x184 | 0x186 => Slot::Non(core, off == 0x186),
        0x188 | 0x18A => Slot::Vmixl(core, off == 0x18A),
        0x18C | 0x18E => Slot::Vmixel(core, off == 0x18E),
        0x190 | 0x192 => Slot::Vmixr(core, off == 0x192),
        0x194 | 0x196 => Slot::Vmixer(core, off == 0x196),
        0x198 => Slot::Mmix(core),
        0x19A => Slot::Attr(core),
        0x19C | 0x19E => Slot::Irqa(core, off == 0x19C),
        0x1A0 | 0x1A2 => Slot::KeyOn(core, off == 0x1A2),
        0x1A4 | 0x1A6 => Slot::KeyOff(core, off == 0x1A6),
        0x1A8 | 0x1AA => Slot::Tsa(core, off == 0x1A8),
        0x1B0 => Slot::Admas(core),
        0x1C0..0x2E0 => {
            let n = off - 0x1C0;
            let voice = (n / 12) as usize;
            let param = ((n % 12) / 2) as usize;
            Slot::VoiceAddr(core, voice, param / 2, param.is_multiple_of(2))
        }
        0x2E0 | 0x2E2 => Slot::Esa(core, off == 0x2E0),
        0x2E4..0x33C => {
            let n = off - 0x2E4;
            Slot::RevbAddr(core, (n / 4) as usize, n.is_multiple_of(4))
        }
        0x33C | 0x33E => Slot::Eea(core, off == 0x33C),
        0x340 | 0x342 => Slot::Endx(core, off == 0x342),
        0x344 => Slot::Statx(core),
        _ => Slot::Raw(mem),
    }
}

/// Slots from 0x760: the cores' volumes and reverb volumes, then SPDIF.
fn ext_slot(mem: u32) -> Slot {
    match mem {
        0x760..0x7B0 => {
            let core = ((mem - 0x760) / 0x28) as usize;
            let n = ((mem - 0x760) % 0x28) / 2;
            match n {
                0 | 1 => Slot::MasterVol(core, n as usize),
                2 | 3 => Slot::FxVol(core, (n - 2) as usize),
                4 | 5 => Slot::ExtVol(core, (n - 4) as usize),
                6 | 7 => Slot::InpVol(core, (n - 6) as usize),
                8 | 9 => Slot::MasterVolX(core, (n - 8) as usize),
                _ => Slot::RevbVol(core, (n - 10) as usize),
            }
        }
        0x7C0..0x7CE => Slot::Spdif(((mem - 0x7C0) / 2) as usize),
        _ => Slot::Raw(mem),
    }
}

/// A 32-bit register's high or low halfword.
fn half(value: u32, hi: bool) -> u16 {
    if hi {
        (value >> 16) as u16
    } else {
        value as u16
    }
}

/// Replaces a 32-bit register's high or low halfword.
fn set_half(target: &mut u32, hi: bool, value: u16) {
    *target = if hi {
        (*target & 0xFFFF) | (u32::from(value) << 16)
    } else {
        (*target & 0xFFFF_0000) | u32::from(value)
    };
}

impl Spu2 {
    /// PCSX2's `SPU2read`.
    pub(crate) fn read_reg(&self, address: u32) -> u16 {
        let mem = address & 0xFFFF;
        if mem >= 0x800 {
            return self.raw[((mem & 0x1FFF) >> 1) as usize];
        }
        // PCSX2 means a read of the data port (0x1AC) to read sound memory
        // at TSA, but its test compares the masked address with
        // 0x1F9001AC and never matches: the read returns the raw cell.
        self.load(slot(mem & !1))
    }

    /// What a slot reads as.
    fn load(&self, slot: Slot) -> u16 {
        match slot {
            Slot::VoiceVol(c, v, lr) => self.cores[c].voices[v].volume[lr].reg,
            Slot::Pitch(c, v) => self.cores[c].voices[v].pitch,
            Slot::Adsr1(c, v) => self.cores[c].voices[v].adsr.reg1,
            Slot::Adsr2(c, v) => self.cores[c].voices[v].adsr.reg2,
            Slot::Envx(c, v) => self.cores[c].voices[v].adsr.value as u16,
            Slot::VoiceVolX(c, v, lr) => self.cores[c].voices[v].volume[lr].value as u16,
            Slot::VoiceAddr(c, v, which, hi) => {
                let voice = &self.cores[c].voices[v];
                let addr = [voice.start_a, voice.loop_start_a, voice.next_a][which];
                half(addr, hi)
            }
            Slot::Pmon(c, hi) => half(self.cores[c].pmon, hi),
            Slot::Non(c, hi) => half(self.cores[c].non, hi),
            Slot::Vmixl(c, hi) => half(self.cores[c].vmixl, hi),
            Slot::Vmixel(c, hi) => half(self.cores[c].vmixel, hi),
            Slot::Vmixr(c, hi) => half(self.cores[c].vmixr, hi),
            Slot::Vmixer(c, hi) => half(self.cores[c].vmixer, hi),
            Slot::Mmix(c) => self.cores[c].mmix,
            Slot::Attr(c) => self.cores[c].attr,
            Slot::Irqa(c, hi) => half(self.cores[c].irqa, hi),
            Slot::KeyOn(c, hi) => half(self.cores[c].key_on, hi),
            Slot::KeyOff(c, hi) => half(self.cores[c].key_off, hi),
            Slot::Tsa(c, hi) => half(self.cores[c].tsa, hi),
            Slot::Admas(c) => self.cores[c].auto_dma_ctrl,
            Slot::Esa(c, hi) => half(self.cores[c].effects_start_a, hi),
            Slot::RevbAddr(c, n, hi) => half(self.cores[c].revb_addr[n], hi),
            Slot::Eea(c, hi) => half(self.cores[c].effects_end_a, hi),
            Slot::Endx(c, hi) => half(self.cores[c].endx, hi),
            Slot::Statx(c) => self.cores[c].statx,
            Slot::MasterVol(c, lr) => self.cores[c].master_vol[lr].reg,
            Slot::FxVol(c, lr) => self.cores[c].fx_vol[lr] as u16,
            Slot::ExtVol(c, lr) => self.cores[c].ext_vol[lr] as u16,
            Slot::InpVol(c, lr) => self.cores[c].inp_vol[lr] as u16,
            Slot::MasterVolX(c, lr) => self.cores[c].master_vol[lr].value as u16,
            Slot::RevbVol(c, n) => self.cores[c].revb_vol[n] as u16,
            Slot::Spdif(n) => self.spdif[n],
            Slot::Raw(mem) => self.raw[((mem & 0x1FFF) >> 1) as usize],
        }
    }

    /// A plain store into a slot: PCSX2's `*(regtable[addr >> 1]) = value`,
    /// for the registers its write handlers do nothing special with.
    fn store(&mut self, slot: Slot, value: u16) {
        match slot {
            Slot::Mmix(c) => self.cores[c].mmix = value,
            Slot::Attr(c) => self.cores[c].attr = value,
            Slot::Irqa(c, hi) => set_half(&mut self.cores[c].irqa, hi, value),
            Slot::KeyOn(c, hi) => set_half(&mut self.cores[c].key_on, hi, value),
            Slot::KeyOff(c, hi) => set_half(&mut self.cores[c].key_off, hi, value),
            Slot::Tsa(c, hi) => set_half(&mut self.cores[c].tsa, hi, value),
            Slot::Admas(c) => self.cores[c].auto_dma_ctrl = value,
            Slot::Esa(c, hi) => set_half(&mut self.cores[c].effects_start_a, hi, value),
            Slot::RevbAddr(c, n, hi) => set_half(&mut self.cores[c].revb_addr[n], hi, value),
            Slot::Eea(c, hi) => set_half(&mut self.cores[c].effects_end_a, hi, value),
            Slot::Statx(c) => self.cores[c].statx = value,
            Slot::RevbVol(c, n) => self.cores[c].revb_vol[n] = value as i16,
            Slot::Spdif(n) => self.spdif[n] = value,
            Slot::Raw(mem) => self.raw[((mem & 0x1FFF) >> 1) as usize] = value,
            // Every other slot has its own write handler.
            _ => {}
        }
    }

    /// PCSX2's `SPU2write` (`SPU2_FastWrite`).
    pub(crate) fn write_reg(&mut self, address: u32, value: u16) {
        let mem = address & 0x7FE;
        if mem >= 0x760 {
            self.write_ext(mem, value);
            return;
        }
        let core = usize::from(mem & 0x400 != 0);
        let off = mem & 0x3FF;
        match off {
            0x000..0x180 => {
                self.write_voice_param(core, (off >> 4) as usize, (off & 0xF) >> 1, value);
            }
            0x1C0..0x2E0 => {
                let n = off - 0x1C0;
                self.write_voice_addr(core, (n / 12) as usize, (n % 12) / 2, value);
            }
            _ => self.write_core(core, off, value),
        }
    }

    /// PCSX2's `RegWrite_VoiceParams`.
    fn write_voice_param(&mut self, core: usize, voice: usize, param: u32, value: u16) {
        let vc = &mut self.cores[core].voices[voice];
        match param {
            0 => vc.volume[0].reg_set(value),
            1 => vc.volume[1].reg_set(value),
            2 => vc.pitch = value,
            3 => {
                vc.adsr.reg1 = value;
                vc.adsr.update_cache();
            }
            4 => {
                vc.adsr.reg2 = value;
                vc.adsr.update_cache();
            }
            // ENVX is writable; VOLXL and VOLXR are, to no effect.
            5 => vc.adsr.value = i32::from(value),
            _ => {}
        }
    }

    /// PCSX2's `RegWrite_VoiceAddr`. Start and loop addresses keep 8-halfword
    /// alignment; NAX is writable (the decoder carries on from there).
    fn write_voice_addr(&mut self, core: usize, voice: usize, param: u32, value: u16) {
        let vc = &mut self.cores[core].voices[voice];
        let hi = (u32::from(value) & 0x0F) << 16;
        let lo = u32::from(value) & 0xFFF8;
        match param {
            0 => vc.start_a = hi | (vc.start_a & 0xFFF8),
            1 => vc.start_a = (vc.start_a & 0x0F_0000) | lo,
            2 => {
                vc.loop_mode = true;
                vc.loop_start_a = hi | (vc.loop_start_a & 0xFFF8);
            }
            3 => {
                vc.loop_mode = true;
                vc.loop_start_a = (vc.loop_start_a & 0x0F_0000) | lo;
            }
            4 => vc.next_a = hi | (vc.next_a & 0xFFF8) | 1,
            _ => vc.next_a = (vc.next_a & 0x0F_0000) | lo | 1,
        }
    }

    /// PCSX2's `RegWrite_Core`.
    fn write_core(&mut self, core: usize, off: u32, value: u16) {
        match off {
            // The data port: a manual transfer of one halfword.
            0x1AC => {
                let tsa = self.cores[core].tsa;
                self.cores[core].active_tsa = tsa;
                self.irq_check(tsa);
                self.set_mem(tsa, value);
                let next = tsa.wrapping_add(1) & 0xF_FFFF;
                self.cores[core].active_tsa = next;
                self.cores[core].tsa = next;
            }
            0x19A => self.write_attr(core, value),
            0x180 => {
                let c = &mut self.cores[core];
                for vc in 1..16 {
                    c.voices[vc].modulated = (value >> vc) & 1 != 0;
                }
                set_half(&mut c.pmon, false, value);
            }
            0x182 => {
                let c = &mut self.cores[core];
                for vc in 0..8 {
                    c.voices[vc + 16].modulated = (value >> vc) & 1 != 0;
                }
                set_half(&mut c.pmon, true, value);
            }
            0x184 => {
                let c = &mut self.cores[core];
                for vc in 0..16 {
                    c.voices[vc].noise = (value >> vc) & 1 != 0;
                }
                set_half(&mut c.non, false, value);
            }
            0x186 => {
                let c = &mut self.cores[core];
                for vc in 0..8 {
                    c.voices[vc + 16].noise = (value >> vc) & 1 != 0;
                }
                set_half(&mut c.non, true, value);
            }
            // The voices' gates read VMIXL/VMIXEL/VMIXR/VMIXER's bits
            // directly; PCSX2 copies them into gate masks on every change.
            0x188 | 0x18A => set_half(&mut self.cores[core].vmixl, off == 0x18A, value),
            0x18C | 0x18E => set_half(&mut self.cores[core].vmixel, off == 0x18E, value),
            0x190 | 0x192 => set_half(&mut self.cores[core].vmixr, off == 0x192, value),
            0x194 | 0x196 => set_half(&mut self.cores[core].vmixer, off == 0x196, value),
            0x198 => {
                let c = &mut self.cores[core];
                let vx = value & if core == 0 { 0xFF0 } else { 0xFFF };
                let bit = |b: u16| vx & b != 0;
                c.wet_gate.ext = [bit(0x002), bit(0x001)];
                c.dry_gate.ext = [bit(0x008), bit(0x004)];
                c.wet_gate.inp = [bit(0x020), bit(0x010)];
                c.dry_gate.inp = [bit(0x080), bit(0x040)];
                c.wet_gate.snd = [bit(0x200), bit(0x100)];
                c.dry_gate.snd = [bit(0x800), bit(0x400)];
                c.mmix = value;
            }
            // Writing ENDX clears the half written.
            0x340 => self.cores[core].endx &= 0xFF_0000,
            0x342 => self.cores[core].endx &= 0xFFFF,
            0x1B0 => self.write_admas(core, value),
            // EEA's low halfword ignores writes.
            0x33E => {}
            _ => {
                // Core 1's write table has DIFF_L_SRC and DIFF_R_SRC the
                // other way round from its read table (PCSX2's
                // `tbl_reg_writes`): kept.
                let off = match (core, off) {
                    (1, 0x324..0x328) => off + 4,
                    (1, 0x328..0x32C) => off - 4,
                    _ => off,
                };
                let mem = off | if core == 1 { 0x400 } else { 0 };
                self.store(slot(mem), value);
            }
        }
    }

    /// ATTR (PCSX2's `REG_C_ATTR` handler).
    fn write_attr(&mut self, core: usize, value: u16) {
        let c = &mut self.cores[core];
        let irqe = c.irq_enable;
        let old_dma_mode = c.dma_mode;

        c.attr_bit0 = (value & 1) as u8;
        c.dma_bits = ((value >> 1) & 7) as u8;
        c.dma_mode = ((value >> 4) & 3) as u8;
        c.irq_enable = (value >> 6) & 1 != 0;
        c.fx_enable = (value >> 7) & 1 != 0;
        c.noise_clk = ((value >> 8) & 0x3F) as u8;
        // PCSX2 leaves bit 14 (mute) and bit 15 (core enable) alone.
        c.mute = false;
        c.attr = value;

        if c.dma_mode == 0 && c.statx & 0x400 == 0 {
            c.statx &= !0x80;
        } else if old_dma_mode == 0 && c.dma_mode != 0 {
            c.statx |= 0x80;
        }

        c.active_tsa = c.tsa;

        if c.irq_enable != irqe && !c.irq_enable {
            self.spdif[spdif::INFO] &= !(4 << core);
        }
    }

    /// ADMAS (PCSX2's `REG_S_ADMAS` handler). PCSX2's hack for the PS1
    /// driver (a write of 0x7FFF switching to PS1 mode) is left out.
    fn write_admas(&mut self, core: usize, value: u16) {
        let c = &mut self.cores[core];
        c.auto_dma_ctrl = value;
        if value.trailing_zeros() >= 2 && c.adma_in_progress {
            // Kill the transfer under way and silence the input buffer.
            c.adma_in_progress = false;
            c.input_data_left = 0;
            c.input_data_transferred = 0;
            c.adma_data.clear();
            let base = 0x2000 + ((core as u32) << 10);
            for i in 0..0x200 {
                self.set_mem(base + i, 0);
                self.set_mem(base + 0x200 + i, 0);
            }
        }
    }

    /// PCSX2's `RegWrite_CoreExt`, `RegWrite_SPDIF` and the raw writes from
    /// 0x760 on.
    fn write_ext(&mut self, mem: u32, value: u16) {
        match ext_slot(mem) {
            Slot::MasterVol(c, lr) => self.cores[c].master_vol[lr].reg_set(value),
            Slot::FxVol(c, lr) => self.cores[c].fx_vol[lr] = i32::from(value as i16),
            Slot::ExtVol(c, lr) => self.cores[c].ext_vol[lr] = i32::from(value as i16),
            Slot::InpVol(c, lr) => self.cores[c].inp_vol[lr] = i32::from(value as i16),
            // MVOLX is read-only.
            Slot::MasterVolX(..) => {}
            // SPDIF writes would update PCSX2's play mode; the play mode
            // stays normal here.
            other => self.store(other, value),
        }
    }
}

#[cfg(test)]
mod slot_tests {
    use super::*;
    use crate::revb;

    #[test]
    fn revb_table_ends_where_eea_starts() {
        assert!(matches!(slot(0x338), Slot::RevbAddr(0, 21, true)));
        assert!(matches!(slot(0x33A), Slot::RevbAddr(0, 21, false)));
        assert!(matches!(
            slot(0x2E4),
            Slot::RevbAddr(0, revb::APF1_SIZE, true)
        ));
        assert!(matches!(
            slot(0x724),
            Slot::RevbAddr(1, revb::DIFF_L_SRC, true)
        ));
        assert!(matches!(slot(0x786), Slot::RevbVol(0, revb::IN_COEF_R)));
        assert!(matches!(slot(0x788), Slot::MasterVol(1, 0)));
        assert!(matches!(slot(0x7AE), Slot::RevbVol(1, revb::IN_COEF_R)));
        assert!(matches!(slot(0x7C2), Slot::Spdif(spdif::INFO)));
    }
}
