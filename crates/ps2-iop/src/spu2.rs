//! The IOP's hardware, as far as the modules it runs need it: the SPU2's
//! two cores (registers and 2 MiB of sound memory, silent) and the IOP DMA
//! channels that feed them (4 for core 0, 7 for core 1).
//!
//! A DMA transfer starts when its channel's CHCR gets the start bit. It
//! takes one IOP cycle per word; then the data moves between IOP RAM and
//! sound memory, the start bit clears and the channel's interrupt line is
//! raised (0x24 or 0x28), as `libsd` expects. Writes to a core's data port
//! go to sound memory at the core's transfer address. A core's STATX has
//! bit 7 set while it has no transfer under way (`libsd` waits for it after
//! each one). Every other register reads back what was last written.

use std::collections::BTreeMap;

use ps2_core::iop::IopMmio;

/// Sound memory size: 2 MiB.
pub const SPU_RAM_SIZE: usize = 2 << 20;

/// The DMA channels feeding the SPU2: (channel, base register, core,
/// interrupt line).
const CHANNELS: [(u8, u32, usize, u32); 2] = [(4, 0x1F80_10C0, 0, 0x24), (7, 0x1F80_1500, 1, 0x28)];

/// A core's registers start here.
fn core_base(core: usize) -> u32 {
    0x1F90_0000 + 0x400 * core as u32
}

/// A DMA transfer under way.
#[derive(Clone, Copy, Debug)]
pub struct Transfer {
    /// Its channel's interrupt line.
    pub line: u32,
    /// Its CHCR register.
    pub chcr: u32,
    /// IOP RAM address.
    pub madr: u32,
    /// Bytes.
    pub bytes: u32,
    /// From IOP RAM to sound memory (or back).
    pub to_spu: bool,
    /// Sound memory address.
    pub spu: u32,
    /// IOP cycle at which it completes.
    pub due: u64,
}

/// The IOP's hardware registers.
pub struct IopHardware {
    values: BTreeMap<u32, u32>,
    /// SPU2 sound memory.
    pub spu_ram: Vec<u8>,
    /// The IOP clock, set by the processor before each instruction.
    pub now: u64,
    /// Transfers under way.
    pub transfers: Vec<Transfer>,
}

impl Default for IopHardware {
    fn default() -> Self {
        Self {
            values: BTreeMap::new(),
            spu_ram: vec![0; SPU_RAM_SIZE],
            now: 0,
            transfers: Vec::new(),
        }
    }
}

impl IopHardware {
    /// A core's transfer address, in bytes (the register counts
    /// halfwords).
    fn tsa(&self, core: usize) -> u32 {
        let base = core_base(core);
        let hi = self.values.get(&(base + 0x1A8)).copied().unwrap_or(0) & 0xF;
        let lo = self.values.get(&(base + 0x1AA)).copied().unwrap_or(0) & 0xFFFF;
        ((hi << 16) | lo) * 2
    }

    fn set_tsa(&mut self, core: usize, bytes: u32) {
        let base = core_base(core);
        let halfwords = (bytes / 2) & 0xF_FFFF;
        self.values.insert(base + 0x1A8, halfwords >> 16);
        self.values.insert(base + 0x1AA, halfwords & 0xFFFF);
    }

    /// When the next transfer completes.
    #[must_use]
    pub fn next_due(&self) -> Option<u64> {
        self.transfers.iter().map(|t| t.due).min()
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
        let spu = self.tsa(core);
        tracing::debug!(target: "iop::spu2", "cycle {}: DMA {}: {} bytes {} sound memory {spu:#x}, IOP RAM {madr:#x}", self.now, CHANNELS[channel].0, words * 4, if chcr & 1 != 0 { "to" } else { "from" });
        self.transfers.push(Transfer {
            line,
            chcr: base + 8,
            madr,
            bytes: words * 4,
            to_spu: chcr & 1 != 0,
            spu,
            due: self.now + u64::from(words).max(1),
        });
    }
}

impl IopMmio for IopHardware {
    fn read(&mut self, address: u32, bytes: u32) -> u32 {
        let mut value = self.values.get(&address).copied().unwrap_or(0);
        if let Some(core) = [0x1F90_0344, 0x1F90_0744]
            .iter()
            .position(|&a| a == address)
        {
            // STATX bit 7: the core's DMA has finished.
            let line = CHANNELS[core].3;
            let busy = self.transfers.iter().any(|t| t.line == line);
            value = (value & !0x80) | if busy { 0 } else { 0x80 };
        }
        tracing::trace!(target: "iop::hw", "read{} {address:#010x} = {value:#x}", bytes * 8);
        value
    }

    fn write(&mut self, address: u32, bytes: u32, value: u32) {
        tracing::trace!(target: "iop::hw", "write{} {address:#010x} = {value:#x}", bytes * 8);
        // A 32-bit write to a pair of 16-bit SPU2 registers.
        if bytes == 4 && (0x1F90_0000..0x1F90_0800).contains(&address) {
            self.write(address, 2, value & 0xFFFF);
            self.write(address + 2, 2, value >> 16);
            return;
        }
        match address {
            // The cores' data ports: manual writes to sound memory.
            0x1F90_01AC | 0x1F90_05AC => {
                let core = usize::from(address >= 0x1F90_0400);
                let at = self.tsa(core) as usize;
                if let Some(slot) = self.spu_ram.get_mut(at..at + 2) {
                    slot.copy_from_slice(&(value as u16).to_le_bytes());
                }
                self.set_tsa(core, (at as u32 + 2) % SPU_RAM_SIZE as u32);
                return;
            }
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
