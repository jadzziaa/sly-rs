//! ADPCM decoding, the voices and the cores' mix, as PCSX2's
//! `pcsx2/SPU2/Mixer.cpp` does them, and key on and key off
//! (`StartVoices`, `StopVoices`, `V_Voice::Start` in `spu2sys.cpp`).

use crate::envelope::PHASE_STOPPED;
use crate::interp::INTERP_TABLE;
use crate::{Spu2, VOICES};

/// Block header flags (the header's high byte). `LOOP_END` sets ENDX and
/// jumps to LSAX, stopping the voice unless LOOP is also set; `LOOP_START`
/// sets LSAX to the block unless LSAX was written since key on.
const XAFLAG_LOOP_END: u8 = 1 << 0;
const XAFLAG_LOOP: u8 = 1 << 1;
const XAFLAG_LOOP_START: u8 = 1 << 2;

/// The ADPCM prediction filters; ids 5 to 15 predict nothing.
const XA_FACTOR: [[i32; 2]; 16] = {
    let mut table = [[0; 2]; 16];
    table[1] = [60, 0];
    table[2] = [115, -52];
    table[3] = [98, -55];
    table[4] = [122, -60];
    table
};

/// A left/right pair.
pub(crate) type Stereo = [i32; 2];

/// Clamps to 16 bits (PCSX2's `clamp_mix`).
pub(crate) fn clamp_mix(x: i32) -> i32 {
    x.clamp(-0x8000, 0x7FFF)
}

fn clamp_stereo(s: Stereo) -> Stereo {
    [clamp_mix(s[0]), clamp_mix(s[1])]
}

/// PCSX2's `ApplyVolume`.
pub(crate) fn apply_volume(data: i32, volume: i32) -> i32 {
    volume.wrapping_mul(data) >> 15
}

fn apply_volume_lr(data: Stereo, volume: Stereo) -> Stereo {
    [
        apply_volume(data[0], volume[0]),
        apply_volume(data[1], volume[1]),
    ]
}

fn add(a: Stereo, b: Stereo) -> Stereo {
    [a[0].wrapping_add(b[0]), a[1].wrapping_add(b[1])]
}

/// Decodes one 16-byte ADPCM block (8 halfwords: the header, then 14 bytes
/// of two 4-bit samples each, low nibble first) into 28 samples, carrying
/// the filter's history (PCSX2's `XA_decode_block`).
pub(crate) fn decode_block(block: &[i16; 8], prev1: &mut i32, prev2: &mut i32) -> [i16; 28] {
    let header = i32::from(block[0]);
    let shift = (header & 0xF) + 16;
    let id = ((header >> 4) & 0xF) as usize;
    let [coef1, coef2] = XA_FACTOR[id];

    let mut out = [0; 28];
    for i in 0..14 {
        let half = block[1 + i / 2];
        let byte = i32::from(if i % 2 == 0 {
            half as i8
        } else {
            (half >> 8) as i8
        });

        let data = ((byte << 28) as u32 & 0xF000_0000) as i32;
        let pcm = (data >> shift) + ((coef1 * *prev1 + coef2 * *prev2 + 32) >> 6);
        let pcm = pcm.clamp(-0x8000, 0x7FFF);
        out[i * 2] = pcm as i16;

        let data = ((byte << 24) as u32 & 0xF000_0000) as i32;
        let pcm2 = (data >> shift) + ((coef1 * pcm + coef2 * *prev1 + 32) >> 6);
        let pcm2 = pcm2.clamp(-0x8000, 0x7FFF);
        out[i * 2 + 1] = pcm2 as i16;

        *prev2 = pcm;
        *prev1 = pcm2;
    }
    out
}

/// Dr. Hell's noise generator as pcsxr implements it (PCSX2's tables).
const NOISE_ADD: [u32; 64] = [
    1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0,
    0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1,
];
const NOISE_FREQ_ADD: [u32; 5] = [0, 84, 140, 180, 210];

impl Spu2 {
    /// Key on (PCSX2's `StartVoices` and `V_Voice::Start`).
    pub(crate) fn start_voices(&mut self, core: usize, value: u32) {
        let c = &mut self.cores[core];
        c.endx &= !value;
        for (i, vc) in c.voices.iter_mut().enumerate() {
            if (value >> i) & 1 == 0 {
                continue;
            }
            if vc.start_a & 7 != 0 {
                // A misaligned start, as PCSX2 "fixes" it (writes keep SSA
                // aligned, so this does not happen).
                vc.start_a = vc.start_a.wrapping_add(0xF_FFF8).wrapping_add(8);
            }
            vc.adsr.attack();
            vc.loop_mode = false;
            vc.sp = 0;
            vc.loop_flags = 0;
            vc.next_a = vc.start_a | 1;
            vc.prev1 = 0;
            vc.prev2 = 0;
            vc.has_block = false;
            vc.dec_pos_read = 0;
            vc.dec_pos_write = 0;
        }
    }

    /// Key off: the envelope goes to release (PCSX2's `StopVoices`).
    pub(crate) fn stop_voices(&mut self, core: usize, value: u32) {
        for (i, vc) in self.cores[core].voices.iter_mut().enumerate() {
            if (value >> i) & 1 != 0 {
                vc.adsr.release();
            }
        }
    }

    /// The current block's header is read every sample (PCSX2's
    /// `UpdateBlockHeader`).
    fn update_block_header(&mut self, core: usize, voice: usize) {
        let block = self.cores[core].voices[voice].next_a & 0xF_FFF8;
        for i in 0..2 {
            if self.cores[i].irq_enable && self.cores[i].irqa == block {
                self.set_irq_call(i);
            }
        }
        let flags = (self.mem(block) >> 8) as u8;
        let vc = &mut self.cores[core].voices[voice];
        vc.loop_flags = flags;
        if flags & XAFLAG_LOOP_START != 0 && !vc.loop_mode {
            vc.loop_start_a = block;
        }
    }

    /// Both cores' IRQs watch the address a voice reads (PCSX2's
    /// `IncrementNextA`).
    fn increment_next_a(&mut self, core: usize, voice: usize) {
        let next_a = self.cores[core].voices[voice].next_a;
        self.irq_check(next_a);
        self.cores[core].voices[voice].next_a = next_a.wrapping_add(1) & 0xF_FFFF;
    }

    /// Four more samples into the voice's FIFO, decoding the block first
    /// when the voice has just reached it (PCSX2's `GetNextDataBuffered`;
    /// its decode cache only saves the decoding).
    fn get_next_data_buffered(&mut self, core: usize, voice: usize) {
        let next_a = self.cores[core].voices[voice].next_a;
        if !self.cores[core].voices[voice].has_block {
            let base = next_a & 0xF_FFF8;
            let raw: [i16; 8] = std::array::from_fn(|i| self.mem(base + i as u32));
            let vc = &mut self.cores[core].voices[voice];
            vc.block = decode_block(&raw, &mut vc.prev1, &mut vc.prev2);
            vc.has_block = true;
        }
        let vc = &mut self.cores[core].voices[voice];
        // The header takes the block's first halfword.
        let sample_idx = (((next_a % 8) as usize).saturating_sub(1)) * 4;
        for i in 0..4 {
            let at = (vc.dec_pos_write.wrapping_add(i as u32) % 32) as usize;
            vc.decode_fifo[at] = i32::from(vc.block[sample_idx + i]);
        }
    }

    /// PCSX2's `DecodeSamples`: runs for stopped voices too, which keep
    /// walking (and looping through) sound memory and raising IRQs.
    fn decode_samples(&mut self, core: usize, voice: usize) {
        self.update_block_header(core, voice);

        let vc = &self.cores[core].voices[voice];
        // When a voice is started at 0 pitch, NAX quickly advances to SSA + 5,
        // so the buffer holds around 12 samples (PCSX2).
        if vc.dec_pos_write.wrapping_sub(vc.dec_pos_read) as i32 > 12 {
            return;
        }

        if vc.adsr.phase > PHASE_STOPPED {
            self.get_next_data_buffered(core, voice);
        }
        let vc = &mut self.cores[core].voices[voice];
        vc.dec_pos_write = vc.dec_pos_write.wrapping_add(4);

        self.increment_next_a(core, voice);
        if self.cores[core].voices[voice].next_a.trailing_zeros() >= 3 {
            let c = &mut self.cores[core];
            let vc = &mut c.voices[voice];
            if vc.loop_flags & XAFLAG_LOOP_END != 0 {
                c.endx |= 1 << voice;
                vc.next_a = vc.loop_start_a;
                if vc.loop_flags & XAFLAG_LOOP == 0 {
                    vc.adsr.value = 0;
                    vc.adsr.phase = PHASE_STOPPED;
                }
            }
            self.increment_next_a(core, voice);
            self.cores[core].voices[voice].has_block = false;
        }
    }

    /// Advances the sample pointer, modulated by the previous voice's
    /// output if PMON says so (PCSX2's `UpdatePitch`).
    fn update_pitch(&mut self, core: usize, voice: usize) {
        let c = &mut self.cores[core];
        let vc = &c.voices[voice];
        let mut pitch = if !vc.modulated || voice == 0 {
            i32::from(vc.pitch)
        } else {
            let prev = c.voices[voice - 1].out_x;
            (i32::from(vc.pitch).wrapping_mul(32768_i32.wrapping_add(prev)) >> 15).clamp(0, 0x3FFF)
        };
        pitch = pitch.min(0x3FFF);
        let vc = &mut c.voices[voice];
        vc.sp = vc.sp.wrapping_add(pitch);
    }

    /// The voice's sample, read through the interpolation table (PCSX2's
    /// `GetVoiceValues`).
    fn voice_value(&self, core: usize, voice: usize) -> i32 {
        let vc = &self.cores[core].voices[voice];
        let phase = ((vc.sp & 0x0FF0) >> 4) as usize;
        let mut out = 0_i32;
        for (k, &weight) in INTERP_TABLE[phase].iter().enumerate() {
            let s = vc.decode_fifo[(vc.dec_pos_read.wrapping_add(k as u32) % 32) as usize];
            out = out.wrapping_add(i32::from(weight).wrapping_mul(s) >> 15);
        }
        out
    }

    /// One voice's sample (PCSX2's `MixVoice`).
    fn mix_voice(&mut self, core: usize, voice: usize) -> Stereo {
        {
            let vc = &mut self.cores[core].voices[voice];
            vc.volume[0].update();
            vc.volume[1].update();
        }

        self.decode_samples(core, voice);

        let mut voice_out = [0, 0];
        let mut value = 0;

        if self.cores[core].voices[voice].adsr.phase > PHASE_STOPPED {
            value = if self.cores[core].voices[voice].noise {
                i32::from(self.cores[core].noise_out as i16)
            } else {
                self.voice_value(core, voice)
            };

            // The envelope applies to wave and noise alike.
            let vc = &mut self.cores[core].voices[voice];
            if !vc.adsr.calculate() {
                vc.adsr.value = 0;
                vc.adsr.phase = PHASE_STOPPED;
            }
            value = apply_volume(value, vc.adsr.value);
            vc.out_x = value;

            voice_out = [
                apply_volume(value, vc.volume[0].value),
                apply_volume(value, vc.volume[1].value),
            ];
        }

        // The SPU2 runs every voice, audible or not.
        self.update_pitch(core, voice);

        let vc = &mut self.cores[core].voices[voice];
        let consumed = vc.sp >> 12;
        vc.sp &= 0xFFF;
        vc.dec_pos_read = vc.dec_pos_read.wrapping_add(consumed as u32);

        // Voices 1 and 3 write their output (after the envelope) to sound
        // memory.
        let out_pos = u32::from(self.out_pos);
        if voice == 1 {
            self.write_fast(if core == 0 { 0x400 } else { 0xC00 } + out_pos, value);
        } else if voice == 3 {
            self.write_fast(if core == 0 { 0x600 } else { 0xE00 } + out_pos, value);
        }

        voice_out
    }

    /// Every voice of a core, into its dry and wet sums (PCSX2's
    /// `MixCoreVoices`), voice 0 first.
    fn mix_core_voices(&mut self, core: usize) -> [Stereo; 2] {
        let mut dry: Stereo = [0, 0];
        let mut wet: Stereo = [0, 0];
        for voice in 0..VOICES {
            let v = self.mix_voice(core, voice);
            let c = &self.cores[core];
            let gate = |reg: u32, x: i32| if (reg >> voice) & 1 != 0 { x } else { 0 };
            dry[0] = dry[0].wrapping_add(gate(c.vmixl, v[0]));
            dry[1] = dry[1].wrapping_add(gate(c.vmixr, v[1]));
            wet[0] = wet[0].wrapping_add(gate(c.vmixel, v[0]));
            wet[1] = wet[1].wrapping_add(gate(c.vmixer, v[1]));
        }
        [dry, wet]
    }

    /// PCSX2's `UpdateNoise`.
    fn update_noise(&mut self, core: usize) {
        let c = &mut self.cores[core];
        let clk = u32::from(c.noise_clk);
        let level = (0x8000_u32 >> (clk >> 2)) << 16;

        c.noise_cnt = c.noise_cnt.wrapping_add(0x10000);
        c.noise_cnt = c.noise_cnt.wrapping_add(NOISE_FREQ_ADD[(clk & 3) as usize]);
        if (c.noise_cnt & 0xFFFF) >= NOISE_FREQ_ADD[4] {
            c.noise_cnt = c.noise_cnt.wrapping_add(0x10000);
            c.noise_cnt = c.noise_cnt.wrapping_sub(NOISE_FREQ_ADD[(clk & 3) as usize]);
        }

        if c.noise_cnt >= level {
            while c.noise_cnt >= level {
                c.noise_cnt -= level;
            }
            c.noise_out = (c.noise_out << 1) | NOISE_ADD[((c.noise_out >> 10) & 63) as usize];
        }
    }

    /// One core's output: input, voices, external input (core 0's output
    /// for core 1), dry and through the reverb (PCSX2's `MixCore`).
    fn mix_core(&mut self, core: usize, voices: [Stereo; 2], input: Stereo, ext: Stereo) -> Stereo {
        {
            let c = &mut self.cores[core];
            c.master_vol[0].update();
            c.master_vol[1].update();
        }
        self.update_noise(core);

        let dry = clamp_stereo(voices[0]);
        let wet = clamp_stereo(voices[1]);

        // The mixed voices go to sound memory.
        let out_pos = u32::from(self.out_pos);
        let base = if core == 0 { 0x1000 } else { 0x1800 };
        self.write_fast(base + out_pos, dry[0]);
        self.write_fast(base + 0x200 + out_pos, dry[1]);
        self.write_fast(base + 0x400 + out_pos, wet[0]);
        self.write_fast(base + 0x600 + out_pos, wet[1]);

        let c = &self.cores[core];
        let g = |on: bool, x: i32| if on { x } else { 0 };
        let mut td = [0, 0];
        let mut tw = [0, 0];
        for lr in 0..2 {
            td[lr] = g(c.dry_gate.inp[lr], input[lr])
                .wrapping_add(g(c.dry_gate.snd[lr], dry[lr]))
                .wrapping_add(g(c.dry_gate.ext[lr], ext[lr]));
            tw[lr] = g(c.wet_gate.inp[lr], input[lr])
                .wrapping_add(g(c.wet_gate.snd[lr], wet[lr]))
                .wrapping_add(g(c.wet_gate.ext[lr], ext[lr]));
        }

        // The reverb runs whether or not ATTR enables effects; the enable
        // only lets it write its buffer and raise IRQs.
        let rv = self.do_reverb(core, tw);

        // Master volume applies later, to dry and wet together.
        add(td, apply_volume_lr(rv, self.cores[core].fx_vol))
    }

    /// One output sample (PCSX2's `spu2Mix`), in the normal play mode.
    pub(crate) fn mix(&mut self) -> [i16; 2] {
        let in0 = self.read_input(0);
        let in0 = apply_volume_lr(in0, self.cores[0].inp_vol);
        let in1 = self.read_input(1);
        let in1 = apply_volume_lr(in1, self.cores[1].inp_vol);

        let voices0 = self.mix_core_voices(0);
        let voices1 = self.mix_core_voices(1);

        let mut ext = self.mix_core(0, voices0, in0, [0, 0]);
        if self.cores[0].mute {
            ext = [0, 0];
        } else {
            let mv = [
                self.cores[0].master_vol[0].value,
                self.cores[0].master_vol[1].value,
            ];
            ext = apply_volume_lr(clamp_stereo(ext), mv);
        }

        // Core 0's output goes to sound memory before core 1 mixes it in.
        let out_pos = u32::from(self.out_pos);
        self.write_fast(0x800 + out_pos, ext[0]);
        self.write_fast(0xA00 + out_pos, ext[1]);

        let ext = apply_volume_lr(ext, self.cores[1].ext_vol);
        let out = self.mix_core(1, voices1, in1, ext);
        let mv = [
            self.cores[1].master_vol[0].value,
            self.cores[1].master_vol[1].value,
        ];
        let out = apply_volume_lr(clamp_stereo(out), mv);

        self.out_pos += 1;
        if self.out_pos >= 0x200 {
            self.out_pos = 0;
        }

        [clamp_mix(out[0]) as i16, clamp_mix(out[1]) as i16]
    }
}
