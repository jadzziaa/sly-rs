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
//! raised (0x24 or 0x28), as `libsd` expects. Other registers read back
//! what was last written.

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

    /// Takes the transfers due by `now` (the processor moves their data).
    pub fn take_due(&mut self, now: u64) -> Vec<Transfer> {
        let (due, rest) = self.transfers.iter().partition(|t| t.due <= now);
        self.transfers = rest;
        for t in &due {
            let chcr = self.values.get(&t.chcr).copied().unwrap_or(0);
            self.values.insert(t.chcr, chcr & !0x0100_0000);
        }
        due
    }

    /// Moves a finished transfer's data between IOP RAM and the SPU2.
    pub fn complete(&mut self, t: &Transfer, ram: &mut [u8]) {
        self.catch_up(self.now);
        let madr = t.madr as usize & 0x1F_FFFF;
        let n = (t.bytes as usize).min(ram.len().saturating_sub(madr)) & !1;
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
        self.spu.set_dma_busy(t.core, false);
        if self.spu.take_irq().contains(&true) {
            self.irq_pending = true;
        }
    }

    /// A DMA channel's CHCR was written.
    fn start(&mut self, channel: usize, chcr: u32) {
        let (_, base, core, line) = CHANNELS[channel];
        if chcr & 0x0100_0000 == 0 {
            return;
        }
        let madr = self.values.get(&base).copied().unwrap_or(0) & 0x00FF_FFFF;
        let bcr = self.values.get(&(base + 4)).copied().unwrap_or(0);
        let (size, count) = (bcr & 0xFFFF, (bcr >> 16).max(1));
        let words = size * count;
        tracing::debug!(target: "iop::spu2", "cycle {}: DMA {}: {} bytes {} the SPU2, IOP RAM {madr:#x}", self.now, CHANNELS[channel].0, words * 4, if chcr & 1 != 0 { "to" } else { "from" });
        self.catch_up(self.now);
        self.spu.set_dma_busy(core, true);
        self.transfers.push(Transfer {
            line,
            core,
            chcr: base + 8,
            madr,
            bytes: words * 4,
            to_spu: chcr & 1 != 0,
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
        tracing::trace!(target: "iop::hw", "read{} {address:#010x} = {value:#x}", bytes * 8);
        value
    }

    fn write(&mut self, address: u32, bytes: u32, value: u32) {
        tracing::trace!(target: "iop::hw", "write{} {address:#010x} = {value:#x}", bytes * 8);
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
