//! EE hardware registers: the interrupt and DMA controllers' status and
//! mask registers, timers, the SIF/SBUS registers, the GS's CSR, the VIFs'
//! registers and FIFOs, and the GIF's FIFO. Everything else is a plain store
//! that keeps the last value written. Every register's first access is
//! logged by name. DMA transfers take modelled time ([`crate::dma`]); VIF1
//! and VU1 run on a thread of their own with the GS behind them
//! ([`crate::vu1`]), fed from here, with a shadow of VIF1 here.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use ps2_core::{Mmio, Regions};

/// INTC causes.
pub mod intc {
    /// The GS (its CSR events, unless the IMR masks them).
    pub const GS: u32 = 0;
    /// VIF0 (a VIF code's interrupt bit).
    pub const VIF0: u32 = 4;
    /// VIF1 (a VIF code's interrupt bit).
    pub const VIF1: u32 = 5;
    /// V-blank start.
    pub const VBLANK_START: u32 = 2;
    /// V-blank end.
    pub const VBLANK_END: u32 = 3;
}

/// Words of a VIF1 transfer the shadow reads before handing them to VU1's
/// thread.
const VIF1_PIECE: usize = 4096;

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
    /// Bytes written to the EE's debug console port (`0x1000F180`).
    pub console: Vec<u8>,
    /// DMA transfers under way.
    pub transfers: Vec<crate::dma::Transfer>,
    /// The GS's SIGNAL and LABEL registers, as packets last set them.
    pub gs_signal_label: (u64, u64),
    /// VIF1 and VU1 on a thread of their own, with the GS behind them
    /// (the GS outside the state hash: it only follows what it is sent).
    pub gs: crate::vu1::Remote,
    /// VIF0.
    pub vif0: ps2_vu::Vif,
    /// VIF0 transfers completed, for the machine to run through VIF0.
    pub vif0_ready: Vec<Vec<u32>>,
    /// VIF1's shadow: the real VIF1 runs on VU1's thread.
    pub vif1: ps2_vu::Vif,
    /// Memory the shadow unpacks into when it must (ROW): nobody reads it.
    shadow_memory: (Box<[u8]>, Box<[u8]>, u64),
    /// VU1's registers, as last copied back from its thread ([`Hardware::sync_vu1`]).
    pub vu1: ps2_vu::Regs,
    /// VIF1's transfer, while it is under way.
    vif1_chain: Option<crate::dma::Chain>,
    /// VIF1 work handed to VU1's thread whose PATH1 events are still to be
    /// raised: the job and when its transfer ended.
    vif1_jobs: Vec<(u64, u64)>,
    /// Host time spent in the VIFs (with the VU programs they start and
    /// what those draw), and in VU programs the EE starts (for reports).
    pub timing: Timing,
}

/// Host time spent by subsystem, for reports.
#[derive(Clone, Copy, Debug, Default)]
pub struct Timing {
    /// VIF0, with VU0 programs.
    pub vif0: std::time::Duration,
    /// VIF1's shadow (the real VIF1 runs on VU1's thread).
    pub vif1: std::time::Duration,
    /// VU programs the EE starts (`vcallms`, CMSAR1).
    pub vu_calls: std::time::Duration,
    /// The IOP catching up.
    pub iop: std::time::Duration,
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
            // ID 0x55 and revision 0x1B in bits 31..16; FIFO empty (bit 14).
            gs_csr: 0x551B_4000,
            gs_imr: 0x7F00,
            sbus: [0, 0, 0, sif::SIFINIT | sif::CMDINIT | sif::BOOTEND],
            timers: [(0, 0, 0, 0); 4],
            regs: BTreeMap::new(),
            seen: BTreeSet::new(),
            dma_kicks: BTreeMap::new(),
            console: Vec::new(),
            transfers: Vec::new(),
            gs_signal_label: (0, 0),
            gs: crate::vu1::Remote::default(),
            vif0: ps2_vu::Vif::new(false),
            vif0_ready: Vec::new(),
            vif1: ps2_vu::Vif::shadow(true),
            shadow_memory: (
                vec![0; 16 << 10].into_boxed_slice(),
                vec![0; 16 << 10].into_boxed_slice(),
                0,
            ),
            vu1: ps2_vu::reset_regs(),
            vif1_chain: None,
            vif1_jobs: Vec::new(),
            timing: Timing::default(),
        }
    }

    /// A GS event (CSR bit `bit`: 0 SIGNAL, 1 FINISH, 3 V-sync): sets it,
    /// and raises the GS interrupt unless the IMR masks it.
    pub fn gs_event(&mut self, bit: u32) {
        self.gs_csr |= 1 << bit;
        if self.gs_imr & (1 << (bit + 8)) == 0 {
            self.raise_intc(intc::GS);
        }
    }

    /// The GS privileged registers as the block at `0x12000000` holds them
    /// (for GS dumps).
    #[must_use]
    pub fn gs_privileged(&self) -> Box<[u8; ps2_gs::dump::PRIV_SIZE]> {
        let mut block = Box::new([0u8; ps2_gs::dump::PRIV_SIZE]);
        for (&address, &value) in self.regs.range(0x1200_0000..0x1200_2000) {
            let at = (address - 0x1200_0000) as usize & !7;
            block[at..at + 8].copy_from_slice(&(value as u64).to_le_bytes());
        }
        block[0x1000..0x1008].copy_from_slice(&self.gs_csr.to_le_bytes());
        block[0x1010..0x1018].copy_from_slice(&self.gs_imr.to_le_bytes());
        block
    }

    /// The GS privileged registers the display read-out uses.
    #[must_use]
    pub fn gs_display(&self) -> ps2_gs::Privileged {
        let reg = |address: u32| self.regs.get(&address).copied().unwrap_or(0) as u64;
        ps2_gs::Privileged {
            pmode: reg(0x1200_0000),
            smode2: reg(0x1200_0020),
            dispfb: [reg(0x1200_0070), reg(0x1200_0090)],
            display: [reg(0x1200_0080), reg(0x1200_00A0)],
            bgcolor: reg(0x1200_00E0),
        }
    }

    /// When the next DMA transfer completes.
    #[must_use]
    pub fn next_dma(&self) -> Option<u64> {
        self.transfers.iter().map(|t| t.due).min()
    }

    /// Completes the transfers due by `now`.
    pub fn finish_dma(&mut self, now: u64) {
        let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.transfers)
            .into_iter()
            .partition(|t| t.due <= now);
        self.transfers = rest;
        for t in due {
            if t.interrupt {
                self.raise_intc(intc::VIF1);
            } else {
                let chcr = DMA_CHANNELS[t.channel].0;
                if let Some(value) = self.regs.get_mut(&chcr) {
                    *value &= !0x100;
                }
                self.complete_dma(t.channel);
            }
            if t.channel == 0 {
                self.vif0_ready.push(t.words);
            }
            self.raise_gs_events(t.events);
        }
    }

    /// Raises what SIGNAL, FINISH and LABEL writes ask for.
    fn raise_gs_events(&mut self, events: ps2_gs::Events) {
        if let Some(signal) = events.signal {
            self.gs_signal_label.0 = signal;
            self.gs_event(0);
        }
        if let Some(label) = events.label {
            self.gs_signal_label.1 = label;
        }
        if events.finish {
            self.gs_event(1);
        }
    }

    /// Starts a transfer on `channel` (its CHCR was written with STR set).
    fn kick_dma(&mut self, regions: &mut Regions, channel: usize, chcr: u32) {
        let (base, name) = DMA_CHANNELS[channel];
        *self.dma_kicks.entry(name).or_default() += 1;
        let reg =
            |hw: &Self, offset: u32| hw.regs.get(&(base + offset)).copied().unwrap_or(0) as u32;
        let (madr, qwc, tadr) = (reg(self, 0x10), reg(self, 0x20), reg(self, 0x30));
        let moves = crate::dma::moves(regions, chcr, madr, qwc, tadr);
        crate::dma::dump(regions, channel, self.clock.get(), &moves);
        let count = self.dma_kicks[name];
        if count.is_power_of_two() {
            tracing::debug!(target: "ee::dma", "{name} kick #{count} (CHCR {chcr:#x}): {} quadwords, {} tags", moves.qwords(), moves.tags);
        }
        if matches!(channel, 8 | 9) {
            crate::dma::scratchpad(regions, channel == 9, &moves, reg(self, 0x80));
        }
        if channel == 1 {
            if self.vif1_chain.is_some() {
                tracing::warn!(target: "ee::dma", "VIF1 kicked while its last transfer is under way");
            }
            self.vif1_chain = Some(crate::dma::Chain::new(chcr, madr, qwc, tadr));
            self.pump_vif1(regions);
            return;
        }
        if channel == 2 {
            ps2_gs::thread::Sink::gif(&mut self.gs, 3, moves.quadwords(regions).collect());
        }
        let words = if channel == 0 {
            moves.words(regions)
        } else {
            Vec::new()
        };
        let events = self.gs.take_events();
        self.transfers.push(crate::dma::Transfer {
            channel,
            due: self.clock.get() + crate::dma::duration(&moves),
            events,
            interrupt: false,
            words,
        });
    }

    /// Words the EE writes into VIF1's FIFO register: they run at once,
    /// unless a transfer is under way (the FIFO is the DMA's then).
    fn write_vif1_fifo(&mut self, value: u128) {
        if self.vif1_chain.is_some() {
            // The FIFO is the DMA's while a transfer is under way.
            tracing::warn!(target: "ee::dma", "VIF1 FIFO written during a transfer: dropped");
            return;
        }
        let words: [u32; 4] = std::array::from_fn(|k| (value >> (32 * k)) as u32);
        self.vif1.push(&words);
        let stop = self.run_shadow();
        let job = self.gs.vif(words.to_vec());
        self.vif1_jobs.push((job, self.clock.get()));
        let events = self.gs.take_events();
        self.raise_gs_events(events);
        if stop == ps2_vu::vif::Stop::Interrupt {
            self.raise_intc(intc::VIF1);
        }
    }

    /// Runs VIF1's shadow on the words it holds.
    fn run_shadow(&mut self) -> ps2_vu::vif::Stop {
        let (code, data, code_writes) = &mut self.shadow_memory;
        let mut mem = ps2_vu::Memories {
            code,
            data,
            code_writes,
        };
        self.vif1.run(&mut self.vu1, &mut mem, &mut self.gs.path2())
    }

    /// At V-blank start (`now`): raises the events PATH1's packets asked
    /// for in VIF1 work whose transfer has ended, once VU1's thread has
    /// run it.
    pub fn vblank_start(&mut self, now: u64) {
        let Some(last) = self
            .vif1_jobs
            .iter()
            .filter(|&&(_, ended)| ended <= now)
            .map(|&(job, _)| job)
            .max()
        else {
            return;
        };
        self.vif1_jobs.retain(|&(job, _)| job > last);
        let events = self.gs.path1_events(last);
        self.raise_gs_events(events);
    }

    /// Copies VU1's memories and registers back from its thread, once it
    /// has run everything sent before (for state hashes and comparisons).
    pub fn sync_vu1(&mut self, regions: &mut Regions) {
        let (code, data, regs) = self
            .gs
            .look(|vu1| (vu1.code.clone(), vu1.data.clone(), vu1.regs.clone()));
        regions.vu1_code.copy_from_slice(&code);
        regions.vu1_data.copy_from_slice(&data);
        self.vu1 = regs;
    }

    /// Feeds VIF1 from its transfer until the VIF stalls or the transfer
    /// ends, and schedules the interrupt or the completion.
    fn pump_vif1(&mut self, regions: &mut Regions) {
        let Some(mut chain) = self.vif1_chain.take() else {
            return;
        };
        let before = chain.cycles();
        let started = std::time::Instant::now();
        let mut words = Vec::with_capacity(4);
        // Everything the shadow reads, for the real VIF1: handed over in
        // pieces, so that VU1's thread starts on them at once.
        let mut sent = Vec::new();
        let mut last = None;
        let interrupt = loop {
            match self.run_shadow() {
                ps2_vu::vif::Stop::NeedData => {
                    // The words the waiting command needs, at once: it
                    // cannot run on fewer, so the transfer stands where
                    // it would after reading them one quadword at a time.
                    words.clear();
                    let missing = self.vif1.missing();
                    let mut ended = false;
                    while words.len() < missing {
                        if !chain.next_words(regions, &mut words) {
                            ended = true;
                            break;
                        }
                    }
                    self.vif1.push(&words);
                    sent.extend_from_slice(&words);
                    if sent.len() >= VIF1_PIECE {
                        last = Some(self.gs.vif(std::mem::take(&mut sent)));
                    }
                    if ended {
                        break false;
                    }
                }
                ps2_vu::vif::Stop::Interrupt => break true,
                ps2_vu::vif::Stop::Stalled => {
                    self.vif1_chain = Some(chain);
                    if !sent.is_empty() {
                        last = Some(self.gs.vif(sent));
                    }
                    if let Some(job) = last {
                        self.vif1_jobs.push((job, self.clock.get()));
                    }
                    self.timing.vif1 += started.elapsed();
                    return;
                }
            }
        };
        let due = self.clock.get() + chain.cycles() - before;
        let job = self.gs.vif(sent);
        self.vif1_jobs.push((job, due));
        self.timing.vif1 += started.elapsed();
        self.transfers.push(crate::dma::Transfer {
            channel: 1,
            due,
            events: self.gs.take_events(),
            interrupt,
            words: Vec::new(),
        });
        if interrupt {
            self.vif1_chain = Some(chain);
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

/// The VU1 memory at `address` (`0x1100_8000` on) and the offset in it.
fn vu1_memory(vu1: &mut crate::vu1::Vu1, address: u32) -> (&mut [u8], usize) {
    if address < 0x1100_C000 {
        (&mut vu1.code, (address & 0x3FFF) as usize)
    } else {
        (&mut vu1.data, (address & 0x3FFF) as usize)
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
    fn set_clock(&mut self, cycles: u64) {
        self.0.borrow().clock.set(cycles);
    }

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
            0x1000_3C00..0x1000_3D80 => u128::from(hw.vif1.read(address - 0x1000_3C00)),
            0x1100_8000..0x1101_0000 => hw.gs.look(move |vu1| {
                let (memory, at) = vu1_memory(vu1, address);
                (0..bytes as usize).fold(0u128, |v, k| v | u128::from(memory[at + k]) << (8 * k))
            }),
            0x1000_3800..0x1000_3980 => u128::from(hw.vif0.read(address - 0x1000_3800)),
            // CHCR reads busy (STR) until the transfer completes, like every
            // other register: the value last written, as completion left it.
            _ => hw.regs.get(&address).copied().unwrap_or(0),
        }
    }

    fn write(&mut self, regions: &mut Regions, address: u32, bytes: u32, value: u128) -> bool {
        let mut hw = self.0.borrow_mut();
        if address != 0x1000_F180 {
            hw.note(address, Some(value), bytes);
        }
        let value32 = value as u32;
        match address {
            0x1000_F000 => hw.intc_stat &= !value32,
            0x1000_F010 => hw.intc_mask ^= value32,
            0x1000_E010 => {
                let clear = value32 & 0xFFFF;
                let toggle = value32 & 0xFFFF_0000;
                hw.d_stat = (hw.d_stat & !clear) ^ toggle;
            }
            0x1000_F180 => hw.console.push(value as u8),
            0x1000_F200 => hw.sbus[0] = value32,
            0x1000_F220 => hw.sbus[2] |= value32,
            0x1000_F230 => hw.sbus[3] &= !value32,
            0x1200_1000 => {
                // Writing 1 to an interrupt bit acknowledges it.
                hw.gs_csr &= !(value as u64 & 0x1F);
            }
            0x1200_1010 => hw.gs_imr = value as u64,
            0x1000_5000 => {
                hw.write_vif1_fifo(value);
                return true;
            }
            0x1000_4000 => {
                // VIF0's FIFO: the words run with the next VIF0 work.
                let words: [u32; 4] = std::array::from_fn(|k| (value >> (32 * k)) as u32);
                hw.vif0_ready.push(words.to_vec());
                return true;
            }
            0x1000_3800..0x1000_3980 => hw.vif0.write(address - 0x1000_3800, value32),
            0x1000_6000 => {
                // The GIF's FIFO: PATH3 packets written by the EE. Their
                // events are raised at once.
                ps2_gs::thread::Sink::gif(&mut hw.gs, 3, vec![value]);
                let events = hw.gs.take_events();
                hw.raise_gs_events(events);
                return true;
            }
            0x1100_8000..0x1101_0000 => hw.gs.call(move |vu1| {
                if address < 0x1100_C000 {
                    vu1.code_writes += 1;
                }
                let (memory, at) = vu1_memory(vu1, address);
                for k in 0..bytes as usize {
                    memory[at + k] = (value >> (8 * k)) as u8;
                }
            }),
            0x1000_3C00..0x1000_3D80 => {
                hw.vif1.write(address - 0x1000_3C00, value32);
                hw.gs.vif_write(address - 0x1000_3C00, value32);
                if address == 0x1000_3C10 && value32 & 8 != 0 {
                    // STC: the stalled transfer goes on.
                    hw.pump_vif1(regions);
                    return true;
                }
            }
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
                hw.regs.insert(address, value);
                if let Some(channel) = DMA_CHANNELS.iter().position(|(base, _)| *base == address)
                    && value32 & 0x100 != 0
                {
                    hw.kick_dma(regions, channel, value32);
                    // The machine's next event changed: recompiled code
                    // must hand back so it is taken at the exact cycle.
                    return true;
                }
            }
        }
        hw.intc_pending() != 0 || hw.dmac_pending() != 0
    }
}
