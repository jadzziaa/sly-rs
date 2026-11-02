//! EE hardware registers, as far as phase 3 needs them: the interrupt and
//! DMA controllers' status and mask registers, timers, the SIF/SBUS
//! registers and the GS's CSR. Everything else is a plain store that keeps
//! the last value written. Every register's first access is logged by
//! name; DMA transfers complete at once with nothing moved (graphics and
//! sound come in later phases), each logged.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use ps2_core::{Mmio, Regions};

/// INTC causes.
pub mod intc {
    /// V-blank start.
    pub const VBLANK_START: u32 = 2;
    /// V-blank end.
    pub const VBLANK_END: u32 = 3;
}

/// DMA channels by number, with their register bases.
pub const DMA_CHANNELS: [(u32, &str); 10] = [
    (0x1000_8000, "VIF0"),
    (0x1000_9000, "VIF1"),
    (0x1000_A000, "GIF"),
    (0x1000_B000, "fromIPU"),
    (0x1000_B400, "toIPU"),
    (0x1000_C000, "SIF0"),
    (0x1000_C400, "SIF1"),
    (0x1000_C800, "SIF2"),
    (0x1000_D000, "fromSPR"),
    (0x1000_D400, "toSPR"),
];

/// SIF registers as the EE kernel's `sceSifGetReg`/`sceSifSetReg` number
/// them.
pub mod sif {
    /// SMFLG bit: the IOP's SIF is initialised.
    pub const SIFINIT: u32 = 0x1_0000;
    /// SMFLG bit: the IOP's command layer is initialised.
    pub const CMDINIT: u32 = 0x2_0000;
    /// SMFLG bit: the IOP has finished booting.
    pub const BOOTEND: u32 = 0x4_0000;
}

/// The hardware registers' state.
pub struct Hardware {
    /// The machine's clock, shared with the interpreter.
    pub clock: Rc<Cell<u64>>,
    /// `INTC_STAT`: pending interrupt causes.
    pub intc_stat: u32,
    /// `INTC_MASK`: enabled causes.
    pub intc_mask: u32,
    /// `D_STAT`: channel completion bits (15..0) and their masks (31..16).
    pub d_stat: u32,
    /// GS CSR.
    pub gs_csr: u64,
    /// GS IMR.
    pub gs_imr: u64,
    /// SBUS mailboxes and flags: MSCOM, SMCOM, MSFLG, SMFLG.
    pub sbus: [u32; 4],
    /// Timers: (mode, base cycle at the last write, count at that time, comp).
    pub timers: [(u32, u64, u32, u32); 4],
    /// Every other register: the last value written.
    pub regs: BTreeMap<u32, u128>,
    /// Registers seen so far (for first-access logging).
    seen: BTreeSet<u32>,
    /// DMA kicks: channel name and count.
    pub dma_kicks: BTreeMap<&'static str, u64>,
}

impl Hardware {
    /// Hardware at power-on, with the IOP reported booted (its side is
    /// high-level emulated).
    #[must_use]
    pub fn new(clock: Rc<Cell<u64>>) -> Self {
        Self {
            clock,
            intc_stat: 0,
            intc_mask: 0,
            d_stat: 0,
            gs_csr: 0x551B_0000_0000_0000,
            gs_imr: 0x7F00,
            sbus: [0, 0, 0, sif::SIFINIT | sif::CMDINIT | sif::BOOTEND],
            timers: [(0, 0, 0, 0); 4],
            regs: BTreeMap::new(),
            seen: BTreeSet::new(),
            dma_kicks: BTreeMap::new(),
        }
    }

    /// Raises an INTC cause.
    pub fn raise_intc(&mut self, cause: u32) {
        self.intc_stat |= 1 << cause;
    }

    /// Marks a DMA channel complete.
    pub fn complete_dma(&mut self, channel: usize) {
        self.d_stat |= 1 << channel;
    }

    /// Whether an enabled INTC cause is pending.
    #[must_use]
    pub fn intc_pending(&self) -> u32 {
        self.intc_stat & self.intc_mask
    }

    /// Whether an enabled DMAC channel completion is pending.
    #[must_use]
    pub fn dmac_pending(&self) -> u32 {
        self.d_stat & (self.d_stat >> 16) & 0x3FF
    }

    fn timer_count(&self, index: usize) -> u32 {
        let (mode, base, count, _) = self.timers[index];
        if mode & 0x80 == 0 {
            return count;
        }
        // Clock source: bus clock (EE/2), /16, /256, or H-blank.
        let divider = match mode & 3 {
            0 => 2,
            1 => 32,
            2 => 512,
            _ => crate::HSYNC_CYCLES,
        };
        count.wrapping_add(((self.clock.get() - base) / divider) as u32) & 0xFFFF
    }

    fn note(&mut self, address: u32, write: Option<u128>, bytes: u32) {
        if self.seen.insert(address) {
            let name = name(address);
            match write {
                Some(value) => {
                    tracing::debug!(target: "ee::mmio", "first write {address:#010x} {name} <- {value:#x} ({bytes} bytes)")
                }
                None => {
                    tracing::debug!(target: "ee::mmio", "first read  {address:#010x} {name} ({bytes} bytes)")
                }
            }
        }
    }
}

/// A register's name, for the log.
#[must_use]
pub fn name(address: u32) -> String {
    let fixed = match address {
        0x1000_F000 => "INTC_STAT",
        0x1000_F010 => "INTC_MASK",
        0x1000_E000 => "D_CTRL",
        0x1000_E010 => "D_STAT",
        0x1000_E020 => "D_PCR",
        0x1000_E030 => "D_SQWC",
        0x1000_E040 => "D_RBSR",
        0x1000_E050 => "D_RBOR",
        0x1000_F200 => "SBUS_MSCOM",
        0x1000_F210 => "SBUS_SMCOM",
        0x1000_F220 => "SBUS_MSFLG",
        0x1000_F230 => "SBUS_SMFLG",
        0x1000_F240 => "SBUS_CTRL",
        0x1000_F520 => "D_ENABLER",
        0x1000_F590 => "D_ENABLEW",
        0x1200_1000 => "GS_CSR",
        0x1200_1010 => "GS_IMR",
        0x1000_7000 => "IPU_out_FIFO",
        0x1000_7010 => "IPU_in_FIFO",
        0x1000_4000 => "VIF0_FIFO",
        0x1000_5000 => "VIF1_FIFO",
        0x1000_6000 => "GIF_FIFO",
        _ => "",
    };
    if !fixed.is_empty() {
        return fixed.to_owned();
    }
    if (0x1000_0000..0x1000_2000).contains(&address) {
        let reg = ["COUNT", "MODE", "COMP", "HOLD"]
            .get(((address & 0xFF) >> 4) as usize)
            .copied()
            .unwrap_or("?");
        return format!("T{}_{reg}", (address >> 11) & 3);
    }
    if let Some((base, channel)) = DMA_CHANNELS
        .iter()
        .find(|(base, _)| (base..&(base + 0x100)).contains(&&address))
    {
        let reg = match address - base {
            0x00 => "CHCR",
            0x10 => "MADR",
            0x20 => "QWC",
            0x30 => "TADR",
            0x40 => "ASR0",
            0x50 => "ASR1",
            0x80 => "SADR",
            _ => "?",
        };
        return format!("D_{channel}_{reg}");
    }
    if (0x1200_0000..0x1200_2000).contains(&address) {
        return "GS_priv".to_owned();
    }
    "unknown".to_owned()
}

/// The [`Mmio`] handle the EE's memory routes hardware accesses to.
pub struct HardwareHandle(pub Rc<std::cell::RefCell<Hardware>>);

impl Mmio for HardwareHandle {
    fn read(&mut self, _: &mut Regions, address: u32, bytes: u32) -> u128 {
        let mut hw = self.0.borrow_mut();
        hw.note(address, None, bytes);
        match address {
            0x1000_F000 => u128::from(hw.intc_stat),
            0x1000_F010 => u128::from(hw.intc_mask),
            0x1000_E010 => u128::from(hw.d_stat),
            0x1000_F200 => u128::from(hw.sbus[0]),
            0x1000_F210 => u128::from(hw.sbus[1]),
            0x1000_F220 => u128::from(hw.sbus[2]),
            0x1000_F230 => u128::from(hw.sbus[3]),
            0x1200_1000 => u128::from(hw.gs_csr),
            0x1200_1010 => u128::from(hw.gs_imr),
            0x1000_0000 | 0x1000_0800 | 0x1000_1000 | 0x1000_1800 => {
                u128::from(hw.timer_count(((address >> 11) & 3) as usize))
            }
            _ => {
                if let Some((base, _)) = DMA_CHANNELS.iter().find(|(base, _)| *base == address) {
                    // CHCR: never busy, transfers complete at once.
                    return hw.regs.get(base).copied().unwrap_or(0) & !0x100;
                }
                hw.regs.get(&address).copied().unwrap_or(0)
            }
        }
    }

    fn write(&mut self, _: &mut Regions, address: u32, bytes: u32, value: u128) {
        let mut hw = self.0.borrow_mut();
        hw.note(address, Some(value), bytes);
        let value32 = value as u32;
        match address {
            0x1000_F000 => hw.intc_stat &= !value32,
            0x1000_F010 => hw.intc_mask ^= value32,
            0x1000_E010 => {
                let clear = value32 & 0xFFFF;
                let toggle = value32 & 0xFFFF_0000;
                hw.d_stat = (hw.d_stat & !clear) ^ toggle;
            }
            0x1000_F200 => hw.sbus[0] = value32,
            0x1000_F220 => hw.sbus[2] |= value32,
            0x1000_F230 => hw.sbus[3] &= !value32,
            0x1200_1000 => {
                // Writing 1 to an interrupt bit acknowledges it.
                hw.gs_csr &= !(value as u64 & 0x1F);
            }
            0x1200_1010 => hw.gs_imr = value as u64,
            0x1000_0000 | 0x1000_0800 | 0x1000_1000 | 0x1000_1800 => {
                let index = ((address >> 11) & 3) as usize;
                let now = hw.clock.get();
                hw.timers[index].1 = now;
                hw.timers[index].2 = value32 & 0xFFFF;
            }
            0x1000_0010 | 0x1000_0810 | 0x1000_1010 | 0x1000_1810 => {
                let index = ((address >> 11) & 3) as usize;
                let count = hw.timer_count(index);
                let now = hw.clock.get();
                hw.timers[index] = (value32, now, count, hw.timers[index].3);
            }
            _ => {
                if let Some(channel) = DMA_CHANNELS.iter().position(|(base, _)| *base == address)
                    && value32 & 0x100 != 0
                {
                    let name = DMA_CHANNELS[channel].1;
                    *hw.dma_kicks.entry(name).or_default() += 1;
                    let count = hw.dma_kicks[name];
                    if count.is_power_of_two() {
                        tracing::debug!(target: "ee::dma", "{name} kick #{count} (CHCR {value32:#x}): completed without a transfer");
                    }
                    hw.complete_dma(channel);
                }
                hw.regs.insert(address, value);
            }
        }
    }
}
