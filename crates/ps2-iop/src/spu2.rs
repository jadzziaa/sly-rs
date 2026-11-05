//! The IOP's hardware, as far as the modules it runs need it: the SPU2
//! ([`ps2_spu2`], PCSX2's, ported) and the IOP DMA channels that feed it (4
//! for core 0, 7 for core 1).
//!
//! The SPU2 makes a sample every 768 IOP cycles. It is caught up to the
//! IOP's clock before every access to its registers, as the IOP's
//! processor runs, and to the machine's time at each V-blank, so whatever
//! the IOP reads is what the chip holds at that moment. While a core's
//! interrupt is enabled, the next sample is an IOP event, so an interrupt
//! is raised at the sample that causes it.
//!
//! A DMA transfer starts when its channel's CHCR gets the start bit and
//! takes one IOP cycle per word; then its data moves between IOP RAM and
//! the SPU2, the start bit clears and the channel's interrupt line is
//! raised (0x24 or 0x28), as `libsd` expects. A transfer to a core in
//! auto-DMA (its `ADMAS` bit set, as 989snd sets it for movie sound) feeds
//! the core's input buffer instead, a kilobyte (a half-buffer of each
//! channel) at a time, read from IOP memory when the core asks for it, as
//! PCSX2 reads it: the movie player refills that memory just behind the
//! sound being played. It completes when the core asks after the last
//! piece; until then it is checked at every sample. Other registers read
//! back what was last written.

use std::collections::BTreeMap;

use ps2_core::iop::IopMmio;
use ps2_spu2::Spu2;

/// The DMA channels feeding the SPU2: (channel, base register, core,
/// interrupt line).
const CHANNELS: [(u8, u32, usize, u32); 2] = [(4, 0x1F80_10C0, 0, 0x24), (7, 0x1F80_1500, 1, 0x28)];

/// The SPU2's registers.
const SPU2: std::ops::Range<u32> = 0x1F90_0000..0x1F90_0800;

/// The cores' ATTR registers: bit 6 enables the core's interrupt.
const ATTR: [u32; 2] = [0x1F90_019A, 0x1F90_059A];

/// Bytes an auto-DMA transfer hands its core at a time: a half-buffer of
/// each channel (256 halfwords left, then 256 right).
const ADMA_PIECE: u32 = 1024;

/// The cores' auto-DMA status registers.
const ADMAS: [u32; 2] = [0x1F90_01B0, 0x1F90_05B0];

/// The IOP interrupt line of the SPU2.
pub const SPU2_LINE: u32 = 9;

/// A DMA transfer under way.
#[derive(Clone, Copy, Debug)]
pub struct Transfer {
    /// Its channel's interrupt line.
    pub line: u32,
    /// The SPU2 core it feeds.
    pub core: usize,
    /// Its CHCR register.
    pub chcr: u32,
    /// IOP RAM address.
    pub madr: u32,
    /// Bytes.
    pub bytes: u32,
    /// From IOP RAM to the SPU2 (or back).
    pub to_spu: bool,
    /// Into the core's auto-DMA input, and how many bytes were handed over.
    pub adma: bool,
    handed: u32,
    /// IOP cycle at which it completes.
    pub due: u64,
}

/// The IOP's hardware registers.
pub struct IopHardware {
    values: BTreeMap<u32, u32>,
    /// The SPU2.
    pub spu: Spu2,
    /// The IOP cycle of the SPU2's next sample.
    next_sample: u64,
    /// Whether each core's interrupt is enabled (ATTR bit 6).
    irq_enabled: [bool; 2],
    /// An SPU2 interrupt waits to be raised.
    irq_pending: bool,
    /// The samples made, when they are wanted.
    samples: Option<Vec<[i16; 2]>>,
    /// Each core's `ADMAS`, as last written.
    admas: [u16; 2],
    /// The IOP clock, set by the processor before each instruction.
    pub now: u64,
    /// Transfers under way.
    pub transfers: Vec<Transfer>,
}

impl Default for IopHardware {
    fn default() -> Self {
        Self {
            values: BTreeMap::new(),
            spu: Spu2::new(),
            next_sample: ps2_spu2::CYCLES_PER_SAMPLE,
            irq_enabled: [false; 2],
            irq_pending: false,
            samples: None,
            admas: [0; 2],
            now: 0,
            transfers: Vec::new(),
        }
    }
}

impl IopHardware {
    /// Runs the SPU2 up to IOP cycle `now`.
    pub fn catch_up(&mut self, now: u64) {
        while self.next_sample <= now {
            let sample = self.spu.step();
            if let Some(samples) = &mut self.samples {
                samples.push(sample);
            }
            if self.spu.take_irq().contains(&true) {
                self.irq_pending = true;
            }
            self.next_sample += ps2_spu2::CYCLES_PER_SAMPLE;
        }
    }

    /// Whether the SPU2 raised its interrupt (taken).
    pub fn take_irq(&mut self) -> bool {
        std::mem::take(&mut self.irq_pending)
    }

    /// Keeps the samples the SPU2 makes from now on (or stops keeping
    /// them).
    pub fn keep_samples(&mut self, keep: bool) {
        self.samples = keep.then(Vec::new);
    }

    /// The samples made since the last call, if they are kept.
    pub fn take_samples(&mut self) -> Vec<[i16; 2]> {
        self.samples
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// When the next transfer completes, or, while an SPU2 interrupt is
    /// enabled, the next sample is made.
    #[must_use]
    pub fn next_due(&self) -> Option<u64> {
        let sample = self.irq_enabled.contains(&true).then_some(self.next_sample);
        self.transfers.iter().map(|t| t.due).chain(sample).min()
    }

    /// Completes the transfers due by now: their data moves (an auto-DMA
    /// transfer's once, when it is first due; it then waits for the core
    /// to ask for more). Returns the interrupt lines to raise.
    pub fn finish_due(&mut self, ram: &mut [u8]) -> Vec<u32> {
        self.catch_up(self.now);
        let mut lines = Vec::new();
        let mut waiting = Vec::new();
        for mut t in std::mem::take(&mut self.transfers) {
            if t.due > self.now {
                waiting.push(t);
                continue;
            }
            if t.adma {
                if !self.spu.adma_wants_data(t.core) || t.handed < t.bytes {
                    if self.spu.adma_wants_data(t.core) {
                        let n = (t.bytes - t.handed).min(ADMA_PIECE);
                        tracing::trace!(target: "iop::spu2", "cycle {}: auto-DMA piece from {:#x}", self.now, t.madr + t.handed);
                        self.move_data(&t, t.handed, n, ram);
                        t.handed += n;
                        // MADR moves on with each piece, as on the IOP.
                        self.values.insert(t.chcr - 8, t.madr + t.handed);
                    }
                    t.due = self.next_sample;
                    waiting.push(t);
                    continue;
                }
            } else {
                self.move_data(&t, 0, t.bytes, ram);
            }
            let chcr = self.values.get(&t.chcr).copied().unwrap_or(0);
            self.values.insert(t.chcr, chcr & !0x0100_0000);
            if !t.adma {
                self.spu.set_dma_busy(t.core, false);
            }
            if self.spu.take_irq().contains(&true) {
                self.irq_pending = true;
            }
            lines.push(t.line);
        }
        self.transfers = waiting;
        lines
    }

    /// Moves `bytes` of a transfer's data, from `offset` on, between IOP RAM
    /// and the SPU2.
    fn move_data(&mut self, t: &Transfer, offset: u32, bytes: u32, ram: &mut [u8]) {
        let madr = (t.madr + offset) as usize & 0x1F_FFFF;
        let n = (bytes as usize).min(ram.len().saturating_sub(madr)) & !1;
        if t.to_spu {
            let halfwords: Vec<u16> = ram[madr..madr + n]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&b| u16::from_le_bytes(b))
                .collect();
            self.spu.dma_write(t.core, &halfwords);
        } else {
            let halfwords = self.spu.dma_read(t.core, n / 2);
            for (k, h) in halfwords.iter().enumerate() {
                ram[madr + 2 * k..madr + 2 * k + 2].copy_from_slice(&h.to_le_bytes());
            }
        }
    }

    /// A DMA channel's CHCR was written.
    fn start(&mut self, channel: usize, chcr: u32) {
        let (_, base, core, line) = CHANNELS[channel];
        if chcr & 0x0100_0000 == 0 {
            // Clearing the start bit stops the channel: a transfer under
            // way ends there, with no interrupt (989snd stops movie sound's
            // auto-DMA this way between movies).
            let before = self.transfers.len();
            self.transfers.retain(|t| t.chcr != base + 8);
            if self.transfers.len() != before {
                tracing::debug!(target: "iop::spu2", "cycle {}: DMA {} stopped", self.now, CHANNELS[channel].0);
                self.spu.set_dma_busy(core, false);
            }
            return;
        }
        let madr = self.values.get(&base).copied().unwrap_or(0) & 0x00FF_FFFF;
        let bcr = self.values.get(&(base + 4)).copied().unwrap_or(0);
        let (size, count) = (bcr & 0xFFFF, (bcr >> 16).max(1));
        let words = size * count;
        tracing::debug!(target: "iop::spu2", "cycle {}: DMA {}: {} bytes {} the SPU2, IOP RAM {madr:#x}", self.now, CHANNELS[channel].0, words * 4, if chcr & 1 != 0 { "to" } else { "from" });
        self.catch_up(self.now);
        let to_spu = chcr & 1 != 0;
        let adma = to_spu && self.admas[core] & (1 << core) != 0;
        if !adma {
            self.spu.set_dma_busy(core, true);
        }
        self.transfers.push(Transfer {
            line,
            core,
            chcr: base + 8,
            madr,
            bytes: words * 4,
            to_spu,
            adma,
            handed: 0,
            due: self.now + u64::from(words).max(1),
        });
    }
}

impl IopMmio for IopHardware {
    fn read(&mut self, address: u32, bytes: u32) -> u32 {
        let value = if SPU2.contains(&address) {
            self.catch_up(self.now);
            let low = u32::from(self.spu.read16(address));
            if bytes == 4 {
                low | u32::from(self.spu.read16(address + 2)) << 16
            } else {
                low
            }
        } else {
            self.values.get(&address).copied().unwrap_or(0)
        };
        tracing::trace!(target: "iop::hw", "cycle {}: read{} {address:#010x} = {value:#x}", self.now, bytes * 8);
        value
    }

    fn write(&mut self, address: u32, bytes: u32, value: u32) {
        tracing::trace!(target: "iop::hw", "cycle {}: write{} {address:#010x} = {value:#x}", self.now, bytes * 8);
        if SPU2.contains(&address) {
            self.catch_up(self.now);
            // A 32-bit write is a pair of 16-bit registers.
            let halves: &[(u32, u32)] = if bytes == 4 {
                &[(address, value & 0xFFFF), (address + 2, value >> 16)]
            } else {
                &[(address, value & 0xFFFF)]
            };
            for &(at, half) in halves {
                self.spu.write16(at, half as u16);
                if let Some(core) = ATTR.iter().position(|&a| a == at) {
                    self.irq_enabled[core] = half & 0x40 != 0;
                }
                if let Some(core) = ADMAS.iter().position(|&a| a == at) {
                    self.admas[core] = half as u16;
                }
            }
            if self.spu.take_irq().contains(&true) {
                self.irq_pending = true;
            }
            return;
        }
        match address {
            // BCR written as two halves.
            0x1F80_10C6 | 0x1F80_1506 => {
                let base = address - 2;
                let low = self.values.get(&base).copied().unwrap_or(0) & 0xFFFF;
                self.values.insert(base, low | (value << 16));
                return;
            }
            0x1F80_10C4 | 0x1F80_1504 if bytes == 2 => {
                let high = self.values.get(&address).copied().unwrap_or(0) & 0xFFFF_0000;
                self.values.insert(address, high | (value & 0xFFFF));
                return;
            }
            _ => {}
        }
        self.values.insert(address, value);
        if let Some(channel) = CHANNELS.iter().position(|c| c.1 + 8 == address) {
            self.start(channel, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Core 0's auto-DMA input fed from IOP RAM at 0x1000, three pieces.
    fn movie_sound() -> IopHardware {
        let mut hw = IopHardware::default();
        hw.write(ADMAS[0], 2, 1);
        hw.write(0x1F80_10C0, 4, 0x1000);
        hw.write(0x1F80_10C4, 2, 0x10);
        hw.write(0x1F80_10C6, 2, 3 * ADMA_PIECE / 64);
        hw.write(0x1F80_10C8, 4, 0x0100_0201);
        hw
    }

    /// MADR moves on a piece at a time as the core takes them, which is
    /// how a movie player sees how far the sound has played.
    #[test]
    fn auto_dma_madr_follows_the_pieces() {
        let mut hw = movie_sound();
        let mut ram = vec![0u8; 0x20_0000];
        let mut seen = Vec::new();
        while hw.transfers.len() == 1 && hw.now < 10_000_000 {
            hw.now = hw.next_due().unwrap_or(hw.now + 1);
            hw.finish_due(&mut ram);
            let madr = hw.read(0x1F80_10C0, 4);
            if seen.last() != Some(&madr) {
                seen.push(madr);
            }
        }
        assert_eq!(seen, [0x1400, 0x1800, 0x1C00]);
    }

    /// Clearing CHCR's start bit stops the channel: the transfer is gone
    /// and no interrupt follows.
    #[test]
    fn clearing_the_start_bit_stops_the_channel() {
        let mut hw = movie_sound();
        assert_eq!(hw.transfers.len(), 1);
        hw.write(0x1F80_10C8, 4, 0x201);
        assert!(hw.transfers.is_empty());
        let mut ram = vec![0u8; 0x20_0000];
        hw.now = 1_000_000;
        assert_eq!(hw.finish_due(&mut ram), Vec::<u32>::new());
    }
}
