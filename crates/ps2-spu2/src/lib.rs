//! The PlayStation 2's sound processor, the SPU2: two cores of 24 voices
//! each, playing ADPCM samples from 2 MiB of sound memory through
//! envelopes and volume slides, with a reverb per core and an input buffer
//! fed by `AutoDMA`, mixed to one 48 kHz stereo output.
//!
//! This is a port of PCSX2's SPU2 (`pcsx2/SPU2`, GPL-3.0-or-later like
//! sly-rs), PS2 mode only. It keeps PCSX2's integer arithmetic and the
//! order of its updates within a sample, so the output matches PCSX2's
//! sample for sample. The modules follow these files:
//!
//! - `regs`: register reads and writes (`spu2sys.cpp`, `RegTable.cpp`,
//!   `spu2.cpp`'s `SPU2read` and `SPU2write`).
//! - `envelope`: envelopes and volume slides (`ADSR.cpp`).
//! - `mixer`: ADPCM decoding, voices, the cores' mix (`Mixer.cpp`), key on
//!   and key off (`spu2sys.cpp`).
//! - `reverb`: the reverb (`Reverb.cpp`) and its downsampling and
//!   upsampling filters (`ReverbResample.cpp`).
//! - `dma`: DMA and `AutoDMA` transfers (`Dma.cpp`) and the cores' input
//!   buffers (`ReadInput.cpp`).
//! - `interp`: the interpolation table (`interpolate_table.h`).
//!
//! Left out: PS1 mode, SPDIF output (its registers read back what was
//! written, and the play mode stays "normal"), PCSX2's debug, wave dump and
//! savestate code, its decode cache (blocks are decoded as voices reach
//! them, which is what the cache's invalidation amounts to) and the DC
//! filter it applies to the host's output.
//!
//! Time: PCSX2 mixes one sample per 768 IOP cycles as its clock catches up
//! (`TimeUpdate`). Here the caller calls [`Spu2::step`] once per sample
//! instead; register writes land between samples, as they do in PCSX2.
//! DMA transfers move their data when the caller hands it over (see
//! [`Spu2::dma_write`]).

// The SPU2 works in 16-bit samples and 20-bit addresses held in 32-bit
// integers, as PCSX2 does; the casts are its truncations.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "16-bit samples and registers in 32-bit integers, as in PCSX2"
)]

use std::fmt;

mod dma;
mod envelope;
mod interp;
mod mixer;
mod regs;
mod reverb;

#[cfg(test)]
mod tests;

use envelope::{Adsr, VolumeSlide};

/// Bytes of sound memory (2 MiB).
pub const RAM_SIZE: usize = 2 << 20;
/// IOP cycles per output sample (36.864 MHz / 48 kHz).
pub const CYCLES_PER_SAMPLE: u64 = 768;
/// Output samples per second.
pub const SAMPLE_RATE: u32 = 48_000;
/// Voices per core.
pub const VOICES: usize = 24;

/// Sound memory addresses (in halfwords) wrap at 1 Mi.
const ADDR_MASK: u32 = 0xF_FFFF;

/// One voice (PCSX2's `V_Voice`).
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools, reason = "PCSX2's flags")]
struct Voice {
    /// Left and right.
    volume: [VolumeSlide; 2],
    adsr: Adsr,
    pitch: u16,
    loop_start_a: u32,
    start_a: u32,
    next_a: u32,
    prev1: i32,
    prev2: i32,
    /// Pitch modulated by the previous voice (PMON).
    modulated: bool,
    /// Plays the noise generator (NON).
    noise: bool,
    /// LSAX was written since key on: loop-start flags leave it alone.
    loop_mode: bool,
    /// The current block's flags (its header's high byte).
    loop_flags: u8,
    /// Sample pointer: 12 bits of fraction.
    sp: i32,
    /// The last output, for the next voice's pitch modulation.
    out_x: i32,
    /// The current block, decoded (PCSX2's `SBuffer`, pointing into its
    /// decode cache), and whether there is one.
    block: [i16; 28],
    has_block: bool,
    decode_fifo: [i32; 32],
    dec_pos_write: u32,
    dec_pos_read: u32,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            volume: [VolumeSlide::default(); 2],
            adsr: Adsr::default(),
            pitch: 0x3FFF,
            loop_start_a: 0x2800,
            start_a: 0x2800,
            next_a: 0x2801,
            prev1: 0,
            prev2: 0,
            modulated: false,
            noise: false,
            loop_mode: false,
            loop_flags: 0,
            sp: 0,
            out_x: 0,
            block: [0; 28],
            has_block: false,
            decode_fifo: [0; 32],
            dec_pos_write: 0,
            dec_pos_read: 0,
        }
    }
}

/// Which sources reach a core's dry or wet output (PCSX2's `V_CoreGates`).
#[derive(Clone, Copy, Debug)]
struct Gates {
    inp: [bool; 2],
    snd: [bool; 2],
    ext: [bool; 2],
}

/// The reverb's buffer offsets, in register order from 0x2E4 (each a
/// high and a low halfword).
mod revb {
    pub(crate) const APF1_SIZE: usize = 0;
    pub(crate) const APF2_SIZE: usize = 1;
    pub(crate) const SAME_L_DST: usize = 2;
    pub(crate) const SAME_R_DST: usize = 3;
    pub(crate) const COMB1_L_SRC: usize = 4;
    pub(crate) const COMB1_R_SRC: usize = 5;
    pub(crate) const COMB2_L_SRC: usize = 6;
    pub(crate) const COMB2_R_SRC: usize = 7;
    pub(crate) const SAME_L_SRC: usize = 8;
    pub(crate) const SAME_R_SRC: usize = 9;
    pub(crate) const DIFF_L_DST: usize = 10;
    pub(crate) const DIFF_R_DST: usize = 11;
    pub(crate) const COMB3_L_SRC: usize = 12;
    pub(crate) const COMB3_R_SRC: usize = 13;
    pub(crate) const COMB4_L_SRC: usize = 14;
    pub(crate) const COMB4_R_SRC: usize = 15;
    pub(crate) const DIFF_L_SRC: usize = 16;
    pub(crate) const DIFF_R_SRC: usize = 17;
    pub(crate) const APF1_L_DST: usize = 18;
    pub(crate) const APF1_R_DST: usize = 19;
    pub(crate) const APF2_L_DST: usize = 20;
    pub(crate) const APF2_R_DST: usize = 21;
    pub(crate) const ADDRS: usize = 22;

    // The reverb's volumes, in register order from 0x774 (core 0).
    pub(crate) const IIR_VOL: usize = 0;
    pub(crate) const COMB1_VOL: usize = 1;
    pub(crate) const COMB2_VOL: usize = 2;
    pub(crate) const COMB3_VOL: usize = 3;
    pub(crate) const COMB4_VOL: usize = 4;
    pub(crate) const WALL_VOL: usize = 5;
    pub(crate) const APF1_VOL: usize = 6;
    pub(crate) const APF2_VOL: usize = 7;
    pub(crate) const IN_COEF_L: usize = 8;
    pub(crate) const IN_COEF_R: usize = 9;
    pub(crate) const VOLS: usize = 10;
}

/// One core (PCSX2's `V_Core`).
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools, reason = "PCSX2's flags")]
struct Core {
    index: usize,
    /// [`VOICES`] of them.
    voices: Vec<Voice>,
    dry_gate: Gates,
    wet_gate: Gates,
    master_vol: [VolumeSlide; 2],
    ext_vol: [i32; 2],
    inp_vol: [i32; 2],
    fx_vol: [i32; 2],
    irqa: u32,
    tsa: u32,
    /// The address a transfer under way has reached.
    active_tsa: u32,
    irq_enable: bool,
    fx_enable: bool,
    mute: bool,
    adma_in_progress: bool,
    dma_bits: u8,
    noise_clk: u8,
    noise_cnt: u32,
    noise_out: u32,
    auto_dma_ctrl: u16,
    /// `AutoDMA` input not yet in the input buffer, in halfwords.
    input_data_left: u32,
    /// PCSX2's simulated MADR progress; the transfer completes when it and
    /// `input_data_left` are both 0.
    input_data_transferred: u32,
    /// Where the next `AutoDMA` block goes (0 or 0x100 in each half buffer,
    /// plus 0x80 once the left half is in), or 0xFFFF for "not asked for".
    input_pos_write: u32,
    /// How much of `adma_data` has gone into the input buffer.
    input_data_progress: u32,
    /// The `AutoDMA` transfer under way: the IOP memory PCSX2 reads lazily
    /// through `DMAPtr`.
    adma_data: Vec<u16>,
    revb_addr: [u32; revb::ADDRS],
    revb_vol: [i16; revb::VOLS],
    revb_down_buf: [[i16; 128]; 2],
    revb_up_buf: [[i16; 128]; 2],
    revb_sample_buf_pos: u32,
    effects_start_a: u32,
    effects_end_a: u32,
    pmon: u32,
    non: u32,
    vmixl: u32,
    vmixr: u32,
    vmixel: u32,
    vmixer: u32,
    endx: u32,
    mmix: u16,
    statx: u16,
    attr: u16,
    attr_bit0: u8,
    dma_mode: u8,
    key_on: u32,
    key_off: u32,
    /// A DMA transfer passed IRQA; raised when it completes (PCSX2's
    /// `has_to_call_irq_dma`).
    irq_dma_pending: bool,
}

impl Core {
    /// Power-on state (PCSX2's `V_Core::Init`).
    fn new(index: usize) -> Self {
        let c = index != 0;
        Self {
            index,
            voices: (0..VOICES).map(|_| Voice::default()).collect(),
            dry_gate: Gates {
                inp: [true; 2],
                snd: [true; 2],
                ext: [false; 2],
            },
            // Core 1's wet gate keeps its external input on until MMIX is
            // written, although MMIX reads 0xFFC (as in PCSX2).
            wet_gate: Gates {
                inp: [true; 2],
                snd: [true; 2],
                ext: [c; 2],
            },
            master_vol: [VolumeSlide::default(); 2],
            ext_vol: [0x7FFF; 2],
            inp_vol: [0x7FFF; 2],
            fx_vol: [0; 2],
            // "Somewhere in the input or output areas" (PCSX2).
            irqa: 0x800,
            tsa: 0,
            active_tsa: 0,
            irq_enable: false,
            fx_enable: false,
            mute: false,
            adma_in_progress: false,
            dma_bits: 0,
            noise_clk: 0,
            noise_cnt: 0,
            noise_out: 0,
            auto_dma_ctrl: 0,
            input_data_left: 0,
            input_data_transferred: 0,
            input_pos_write: 0x100,
            input_data_progress: 0,
            adma_data: Vec::new(),
            revb_addr: [0; revb::ADDRS],
            revb_vol: [0; revb::VOLS],
            revb_down_buf: [[0; 128]; 2],
            revb_up_buf: [[0; 128]; 2],
            revb_sample_buf_pos: 0,
            effects_start_a: if c { 0xF_FFF8 } else { 0xE_FFF8 },
            effects_end_a: if c { 0xF_FFFF } else { 0xE_FFFF },
            pmon: 0,
            non: 0,
            vmixl: 0xFF_FFFF,
            vmixr: 0xFF_FFFF,
            vmixel: 0xFF_FFFF,
            vmixer: 0xFF_FFFF,
            endx: 0xFF_FFFF,
            mmix: if c { 0xFFC } else { 0xFF0 },
            statx: 0x80,
            attr: 0,
            attr_bit0: 0,
            dma_mode: 0,
            key_on: 0,
            key_off: 0,
            irq_dma_pending: false,
        }
    }
}

/// The SPDIF registers at 0x7C0 (PCSX2's `V_SPDIF`), in order.
mod spdif {
    pub(crate) const INFO: usize = 1;
    pub(crate) const REGS: usize = 7;
}

/// The SPU2: both cores, sound memory and the registers.
#[derive(Clone)]
pub struct Spu2 {
    cores: [Core; 2],
    ram: Vec<u8>,
    /// Out, Info (the cores' IRQ flags, bits 2 and 3), Unknown1, Mode,
    /// Media, Unknown2, Protection.
    spdif: [u16; spdif::REGS],
    /// Register space with nothing behind it reads back what was written
    /// (PCSX2's `spu2regs`, indexed by byte address & 0x1FFF).
    raw: Vec<u16>,
    /// Samples mixed (PCSX2's `Cycles`); the reverb runs from it.
    cycles: u32,
    /// The sample's position in the 0x200-sample output and input buffers.
    out_pos: u16,
    /// Interrupts raised since the last [`Spu2::take_irq`].
    irq: [bool; 2],
}

impl Default for Spu2 {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Spu2 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Spu2")
            .field("cycles", &self.cycles)
            .field("out_pos", &self.out_pos)
            .field("irq", &self.irq)
            .finish_non_exhaustive()
    }
}

impl Spu2 {
    /// The power-on state, as PCSX2 resets it.
    #[must_use]
    pub fn new() -> Self {
        let mut ram = vec![0; RAM_SIZE];
        // From the BIOS: a looping block of silence voices start on, and
        // one the BIOS leaves behind (Mega Man X7 relies on it).
        for start in [0x2800_usize, 0xE870] {
            ram[start * 2..start * 2 + 0x10].fill(7);
        }
        Self {
            cores: [Core::new(0), Core::new(1)],
            ram,
            spdif: [0; spdif::REGS],
            raw: vec![0; 0x1000],
            cycles: 0,
            out_pos: 0,
            irq: [false; 2],
        }
    }

    /// Sound memory: little-endian halfwords.
    #[must_use]
    pub fn ram(&self) -> &[u8] {
        &self.ram
    }

    /// Sound memory, to change. Voices see changes from the next block
    /// they decode.
    pub fn ram_mut(&mut self) -> &mut [u8] {
        &mut self.ram
    }

    /// A 16-bit register read; `address` is the IOP physical address
    /// `0x1F90_0000..0x1F90_0800`.
    pub fn read16(&mut self, address: u32) -> u16 {
        self.read_reg(address)
    }

    /// A 16-bit register write (same addresses). Writes to a core's data
    /// port (0x1AC) go to sound memory at TSA and advance it, as manual
    /// transfers do.
    pub fn write16(&mut self, address: u32, value: u16) {
        self.write_reg(address, value);
    }

    /// Runs one 48 kHz sample: every voice, both cores, reverb; returns the
    /// output (left, right).
    ///
    /// Key-offs and key-ons written since the last sample take effect
    /// first, core 0's then core 1's, as in PCSX2's `TimeUpdate`.
    pub fn step(&mut self) -> [i16; 2] {
        self.cycles = self.cycles.wrapping_add(1);
        for c in 0..2 {
            let key_off = std::mem::take(&mut self.cores[c].key_off);
            if key_off != 0 {
                self.stop_voices(c, key_off);
            }
            let key_on = std::mem::take(&mut self.cores[c].key_on);
            if key_on != 0 {
                self.start_voices(c, key_on);
            }
        }
        self.mix()
    }

    /// Whether core 0/1 raised its interrupt since the last call (taken:
    /// cleared on return). The SPDIF IRQ info register (0x7C2) keeps its
    /// bits until the game clears them or turns the interrupt off.
    pub fn take_irq(&mut self) -> [bool; 2] {
        std::mem::take(&mut self.irq)
    }

    /// A DMA write of halfwords into `core`'s transfer path: plain
    /// transfers to sound memory at TSA, or, when the core's `AutoDMA` is
    /// enabled (ADMAS), into its input buffer, as PCSX2's `DoDMAwrite`
    /// does.
    ///
    /// A plain transfer's data lands at once (PCSX2 copies it in pieces as
    /// its DMA counter runs); an interrupt for passing IRQA is raised when
    /// the transfer completes ([`Spu2::set_dma_busy`] with `false`). An
    /// `AutoDMA` transfer's data is kept and moves into the input buffer half
    /// a buffer at a time as the output reaches each half, as in PCSX2;
    /// [`Spu2::adma_wants_data`] says when it has all gone in.
    pub fn dma_write(&mut self, core: usize, data: &[u16]) {
        self.do_dma_write(core & 1, data);
    }

    /// A DMA read of `halfwords` from sound memory at the core's TSA.
    pub fn dma_read(&mut self, core: usize, halfwords: usize) -> Vec<u16> {
        self.do_dma_read(core & 1, halfwords)
    }

    /// Whether the core's `AutoDMA` wants more input (so its IOP DMA channel
    /// can be fed), as PCSX2 decides it: `AutoDMA` is on for the core and
    /// everything handed to it has gone into the input buffer. PCSX2
    /// completes the channel's transfer (raising its DMA interrupt) at that
    /// point, a few samples after the last block went in.
    #[must_use]
    pub fn adma_wants_data(&self, core: usize) -> bool {
        let core = &self.cores[core & 1];
        let bit = (core.index + 1) as u16;
        core.auto_dma_ctrl & bit == bit
            && core.input_data_left == 0
            && core.input_data_transferred == 0
    }

    /// The IOP's DMA channel for `core` started (`true`) or finished
    /// (`false`) a transfer. STATX (0x344, 0x744) then reads as PCSX2 keeps
    /// it: a transfer clears bit 7 and sets bit 10; its end sets bit 7
    /// again if ATTR selects a DMA mode (bits 4 and 5), clears bit 10 and
    /// raises any interrupt a transfer owed for passing IRQA (PCSX2's
    /// `SPU2interruptDMA4/7`, then its DMA counter running out). Call it
    /// with `false` before raising the channel's interrupt.
    pub fn set_dma_busy(&mut self, core: usize, busy: bool) {
        let c = core & 1;
        let core = &mut self.cores[c];
        if busy {
            core.statx = (core.statx & !0x80) | 0x400;
        } else {
            if core.dma_mode != 0 {
                core.statx |= 0x80;
            }
            core.statx &= !0x400;
            core.tsa = core.active_tsa;
            self.raise_dma_irqs();
        }
    }

    /// Raises a core's interrupt (PCSX2's `SetIrqCall`): once until the
    /// game clears its bit in the SPDIF IRQ info register, and only while
    /// the core's interrupt is enabled (ATTR bit 6).
    fn set_irq_call(&mut self, core: usize) {
        let bit = 4 << core;
        if self.spdif[spdif::INFO] & bit == 0 && self.cores[core].irq_enable {
            self.spdif[spdif::INFO] |= bit;
            self.irq[core] = true;
        }
    }

    /// Both cores watch every address the SPU2 touches.
    fn irq_check(&mut self, address: u32) {
        for i in 0..2 {
            if self.cores[i].irq_enable && self.cores[i].irqa == address {
                self.set_irq_call(i);
            }
        }
    }

    /// A halfword of sound memory (`addr` in halfwords).
    fn mem(&self, addr: u32) -> i16 {
        let at = ((addr & ADDR_MASK) as usize) * 2;
        i16::from_le_bytes([self.ram[at], self.ram[at + 1]])
    }

    /// Writes a halfword of sound memory (PCSX2's `spu2M_Write`).
    fn set_mem(&mut self, addr: u32, value: u16) {
        let at = ((addr & ADDR_MASK) as usize) * 2;
        self.ram[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    /// Writes one of the output areas below 0x2800, with the IRQ check
    /// (PCSX2's `spu2M_WriteFast`).
    fn write_fast(&mut self, addr: u32, value: i32) {
        self.irq_check(addr);
        self.set_mem(addr, value as u16);
    }
}
