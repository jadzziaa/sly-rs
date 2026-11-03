//! DMA transfers as sinks: a kicked channel's transfer is followed through
//! memory (normal mode, or source chain with its DMA tags), takes a
//! modelled time, then completes: the channel's `D_STAT` bit is set and its
//! start bit clears. The data goes nowhere yet (phase 6 renders it), except
//! the GS events a GIF packet asks for: writes to SIGNAL, FINISH and LABEL
//! set the GS's CSR when the transfer completes, and raise the GS interrupt
//! unless the IMR masks it. GIF packets come straight through the GIF
//! channel (PATH3) or inside VIF1's DIRECT commands (PATH2).
//!
//! Time: two cycles per quadword plus eight per DMA tag, a fixed and
//! deterministic stand-in for the bus until the GS is modelled.

use ps2_core::Regions;

/// Cycles per quadword moved.
const CYCLES_PER_QWORD: u64 = 2;
/// Cycles per DMA tag read, and to start a transfer.
const CYCLES_PER_TAG: u64 = 8;
/// The most DMA tags followed in one transfer (a guard against loops).
const MAX_TAGS: u32 = 1 << 16;

/// GS events a transfer's packets cause.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GsEvents {
    /// SIGNAL was written: its value.
    pub signal: Option<u64>,
    /// FINISH was written.
    pub finish: bool,
    /// LABEL was written: its value.
    pub label: Option<u64>,
}

/// A transfer under way.
#[derive(Clone, Copy, Debug)]
pub struct Transfer {
    /// The channel.
    pub channel: usize,
    /// When it completes.
    pub due: u64,
    /// What it does to the GS.
    pub gs: GsEvents,
}

/// A quadword of EE memory (bit 31 of the address selects the
/// scratchpad).
fn qword(regions: &Regions, address: u32) -> u128 {
    let (region, offset) = if address & 0x8000_0000 != 0 {
        (&regions.scratchpad, (address & 0x3FF0) as usize)
    } else {
        (&regions.ram, (address & 0x01FF_FFF0) as usize)
    };
    region.get(offset..offset + 16).map_or(0, |b| {
        let mut raw = [0; 16];
        raw.copy_from_slice(b);
        u128::from_le_bytes(raw)
    })
}

/// What a transfer moves: blocks of memory (address and quadword count),
/// and with TTE the upper half of each DMA tag, sent before its block (VIF
/// channels read VIF codes there).
#[derive(Clone, Debug, Default)]
pub struct Moves {
    /// Blocks, each with the tag words sent before it, if any.
    pub blocks: Vec<(Option<u64>, u32, u32)>,
    /// DMA tags read.
    pub tags: u32,
}

impl Moves {
    /// The quadwords moved.
    #[must_use]
    pub fn qwords(&self) -> u64 {
        self.blocks.iter().map(|b| u64::from(b.2)).sum()
    }

    /// The data as 32-bit words, tag words included.
    #[must_use]
    pub fn words(&self, regions: &Regions) -> Vec<u32> {
        let mut out = Vec::new();
        for &(tag, at, n) in &self.blocks {
            if let Some(tag) = tag {
                out.extend([tag as u32, (tag >> 32) as u32]);
            }
            for i in 0..n {
                let q = qword(regions, at.wrapping_add(16 * i));
                out.extend((0..4).map(|k| (q >> (32 * k)) as u32));
            }
        }
        out
    }
}

/// What a transfer moves, from the channel's registers when its CHCR is
/// written.
#[must_use]
pub fn moves(regions: &Regions, chcr: u32, madr: u32, qwc: u32, tadr: u32) -> Moves {
    let mode = (chcr >> 2) & 3;
    if mode != 1 {
        return Moves {
            blocks: vec![(None, madr, qwc & 0xFFFF)],
            tags: 0,
        };
    }
    let tte = chcr & 0x40 != 0;
    // Source chain: an optional first block from MADR/QWC, then tags.
    let mut out = Vec::new();
    if qwc & 0xFFFF != 0 {
        out.push((None, madr, qwc & 0xFFFF));
    }
    let mut tag_at = tadr;
    let mut stack: Vec<u32> = Vec::new();
    let tie = chcr & 0x80 != 0;
    for tags in 1..=MAX_TAGS {
        let full = qword(regions, tag_at);
        let tag = full as u64;
        let count = (tag & 0xFFFF) as u32;
        let id = (tag >> 28) & 7;
        let irq = tag & 0x8000_0000 != 0;
        let address = (tag >> 32) as u32;
        let after = tag_at.wrapping_add(16);
        let (data, next) = match id {
            0 => (address, None),                               // refe
            1 => (after, Some(after.wrapping_add(count * 16))), // cnt
            2 => (after, Some(address)),                        // next
            3 | 4 => (address, Some(after)),                    // ref, refs
            5 => {
                // call: data follows; return after it.
                stack.push(after.wrapping_add(count * 16));
                (after, Some(address))
            }
            6 => (after, stack.pop()), // ret
            _ => (after, None),        // end
        };
        out.push((tte.then_some((full >> 64) as u64), data, count));
        match next {
            Some(n) if !(irq && tie) => tag_at = n,
            _ => return Moves { blocks: out, tags },
        }
    }
    Moves {
        blocks: out,
        tags: MAX_TAGS,
    }
}

/// The GS events in GIF packets (PACKED A+D writes to SIGNAL 0x60, FINISH
/// 0x61 and LABEL 0x62), from their quadwords.
pub fn gif_events(data: impl Iterator<Item = u128>, events: &mut GsEvents) {
    let mut data = data;
    while let Some(tag) = data.next() {
        let nloop = (tag & 0x7FFF) as u32;
        let flg = (tag >> 58) & 3;
        let nreg = match ((tag >> 60) & 0xF) as u32 {
            0 => 16,
            n => n,
        };
        let regs = (tag >> 64) as u64;
        match flg {
            0 => {
                for _ in 0..nloop {
                    for r in 0..nreg {
                        let Some(q) = data.next() else {
                            return;
                        };
                        if (regs >> (4 * r)) & 0xF == 0xE {
                            let (value, register) = (q as u64, (q >> 64) as u8);
                            match register {
                                0x60 => events.signal = Some(value),
                                0x61 => events.finish = true,
                                0x62 => events.label = Some(value),
                                _ => {}
                            }
                        }
                    }
                }
            }
            1 => {
                let words = u64::from(nloop) * u64::from(nreg);
                for _ in 0..words.div_ceil(2) {
                    data.next();
                }
            }
            _ => {
                for _ in 0..nloop {
                    data.next();
                }
            }
        }
    }
}

/// The GS events a transfer on `channel` causes: GIF packets on the GIF
/// channel, or inside VIF1's DIRECT commands.
#[must_use]
pub fn gs_events(regions: &Regions, channel: usize, moves: &Moves) -> GsEvents {
    let mut events = GsEvents::default();
    match channel {
        2 => {
            let words = moves.words(regions);
            gif_events(quadwords(&words), &mut events);
        }
        1 => {
            for packet in vif_direct(&moves.words(regions)) {
                gif_events(quadwords(&packet), &mut events);
            }
        }
        _ => {}
    }
    events
}

fn quadwords(words: &[u32]) -> impl Iterator<Item = u128> + '_ {
    words.as_chunks::<4>().0.iter().map(|c| {
        u128::from(c[0]) | u128::from(c[1]) << 32 | u128::from(c[2]) << 64 | u128::from(c[3]) << 96
    })
}

/// The DIRECT and DIRECTHL payloads (GIF packets) in a VIF1 stream.
fn vif_direct(words: &[u32]) -> Vec<Vec<u32>> {
    let mut out = Vec::new();
    let (mut cl, mut wl) = (1u32, 1u32);
    let mut i = 0;
    while i < words.len() {
        let code = words[i];
        i += 1;
        let command = (code >> 24) & 0x7F;
        let imm = code & 0xFFFF;
        let num = match (code >> 16) & 0xFF {
            0 => 256,
            n => n,
        };
        match command {
            0x01 => {
                cl = imm & 0xFF;
                wl = imm >> 8;
            }
            0x00 | 0x02..=0x07 | 0x10 | 0x11 | 0x13 | 0x14 | 0x15 | 0x17 => {}
            0x20 => i += 1,
            0x30 | 0x31 => i += 4,
            0x4A => i += 2 * num as usize,
            0x50 | 0x51 => {
                let n = if imm == 0 { 65_536 } else { imm } as usize * 4;
                out.push(
                    words
                        .get(i..(i + n).min(words.len()))
                        .unwrap_or_default()
                        .to_vec(),
                );
                i += n;
            }
            0x60..=0x7F => {
                let (vn, vl) = (command & 3, (command >> 2) & 3);
                let elements = if wl <= cl || wl == 0 {
                    num
                } else {
                    cl * (num / wl) + (num % wl).min(cl)
                };
                let bits = (32 >> vl) * (vn + 1) * elements;
                i += bits.div_ceil(32) as usize;
            }
            _ => break,
        }
    }
    out
}

/// The time a transfer takes.
#[must_use]
pub fn duration(moves: &Moves) -> u64 {
    CYCLES_PER_TAG * (1 + u64::from(moves.tags)) + CYCLES_PER_QWORD * moves.qwords()
}
