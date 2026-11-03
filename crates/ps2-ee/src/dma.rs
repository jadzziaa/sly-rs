//! DMA transfers: a kicked channel's data is followed through memory
//! (normal mode, or source chain with its DMA tags), takes a modelled time,
//! then completes: the channel's `D_STAT` bit is set and its start bit
//! clears.
//!
//! - The GIF channel's packets reach the GS (PATH3) when it is kicked, so
//!   the GS sees every path's packets in the order the EE kicked them.
//! - VIF1 is fed word by word into its VIF, which may stop the transfer
//!   part-way (an interrupt bit) until the EE resumes it; the rest of the
//!   chain is read from memory only then, as the hardware does, so an
//!   interrupt handler can patch what follows.
//! - VIF0's words reach VIF0 when it completes; VU0's registers are the
//!   EE's, so the machine runs them.
//! - toSPR and fromSPR copy between memory and the scratchpad when kicked.
//! - Other channels move their data nowhere yet.
//!
//! GS events the packets ask for (SIGNAL, FINISH, LABEL) are raised when the
//! transfer completes.
//!
//! Time: two cycles per quadword plus eight per DMA tag, a fixed and
//! deterministic stand-in for the bus until it is modelled.

use ps2_core::Regions;

/// Cycles per quadword moved.
pub const CYCLES_PER_QWORD: u64 = 2;
/// Cycles per DMA tag read, and to start a transfer.
pub const CYCLES_PER_TAG: u64 = 8;
/// The most DMA tags followed in one transfer (a guard against loops).
const MAX_TAGS: u32 = 1 << 16;

/// A transfer under way: something happens on `channel` at `due`.
#[derive(Clone, Debug)]
pub struct Transfer {
    /// The channel.
    pub channel: usize,
    /// When it completes (or interrupts).
    pub due: u64,
    /// GS events its packets asked for, raised when it completes.
    pub events: ps2_gs::Events,
    /// It stops at an interrupt instead of completing (VIF1).
    pub interrupt: bool,
    /// VIF0's words, for VIF0 when the transfer completes.
    pub words: Vec<u32>,
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

/// A block of a transfer: its DMA tag when the tag is sent too (TTE), its
/// address and its quadword count. VIFs take the tag's upper 64 bits; the
/// scratchpad takes all of it.
pub type Block = (Option<u128>, u32, u32);

/// A transfer's DMA tags, followed one block at a time.
#[derive(Clone, Debug)]
pub struct Chain {
    chcr: u32,
    madr: u32,
    qwc: u32,
    /// The next tag, or `None` once an end tag (or normal mode's block)
    /// was taken.
    tag_at: Option<u32>,
    started: bool,
    stack: Vec<u32>,
    /// DMA tags read.
    pub tags: u32,
    /// The block being sent, and the quadwords of it left.
    cursor: Option<(u32, u32)>,
    /// Quadwords sent through [`Chain::next_words`].
    pub qwords: u64,
}

impl Chain {
    /// The transfer a channel's registers describe when its CHCR is written.
    #[must_use]
    pub fn new(chcr: u32, madr: u32, qwc: u32, tadr: u32) -> Self {
        Self {
            chcr,
            madr,
            qwc: qwc & 0xFFFF,
            tag_at: Some(tadr),
            started: false,
            stack: Vec::new(),
            tags: 0,
            cursor: None,
            qwords: 0,
        }
    }

    /// The next block, reading its DMA tag from memory.
    pub fn next_block(&mut self, regions: &Regions) -> Option<Block> {
        let chain = (self.chcr >> 2) & 3 == 1;
        if !self.started {
            self.started = true;
            if !chain {
                self.tag_at = None;
                return Some((None, self.madr, self.qwc));
            }
            // Source chain: an optional first block from MADR/QWC.
            if self.qwc != 0 {
                return Some((None, self.madr, self.qwc));
            }
        }
        let tag_at = self.tag_at?;
        if self.tags >= MAX_TAGS {
            self.tag_at = None;
            return None;
        }
        self.tags += 1;
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
                self.stack.push(after.wrapping_add(count * 16));
                (after, Some(address))
            }
            6 => (after, self.stack.pop()), // ret
            _ => (after, None),             // end
        };
        let tie = self.chcr & 0x80 != 0;
        self.tag_at = next.filter(|_| !(irq && tie));
        let tte = self.chcr & 0x40 != 0;
        Some((tte.then_some(full), data, count))
    }

    /// Appends the next words of the transfer to `out`: a block's tag words
    /// (with TTE) or one quadword. False when the transfer is over.
    pub fn next_words(&mut self, regions: &Regions, out: &mut Vec<u32>) -> bool {
        loop {
            if let Some((at, left)) = self.cursor
                && left > 0
            {
                let q = qword(regions, at);
                out.extend((0..4).map(|k| (q >> (32 * k)) as u32));
                self.cursor = Some((at.wrapping_add(16), left - 1));
                self.qwords += 1;
                return true;
            }
            let Some((tag, at, count)) = self.next_block(regions) else {
                return false;
            };
            self.cursor = Some((at, count));
            if let Some(tag) = tag {
                out.extend([(tag >> 64) as u32, (tag >> 96) as u32]);
                return true;
            }
        }
    }

    /// The time the work done so far took.
    #[must_use]
    pub fn cycles(&self) -> u64 {
        CYCLES_PER_TAG * (1 + u64::from(self.tags)) + CYCLES_PER_QWORD * self.qwords
    }
}

/// What a transfer moves: its blocks, and the DMA tags read.
#[derive(Clone, Debug, Default)]
pub struct Moves {
    /// Blocks, each with the tag words sent before it, if any.
    pub blocks: Vec<Block>,
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
                out.extend([(tag >> 64) as u32, (tag >> 96) as u32]);
            }
            for i in 0..n {
                let q = qword(regions, at.wrapping_add(16 * i));
                out.extend((0..4).map(|k| (q >> (32 * k)) as u32));
            }
        }
        out
    }

    /// The data as quadwords (tag words left out).
    pub fn quadwords<'a>(&'a self, regions: &'a Regions) -> impl Iterator<Item = u128> + 'a {
        self.blocks.iter().flat_map(move |&(_, at, n)| {
            (0..n).map(move |i| qword(regions, at.wrapping_add(16 * i)))
        })
    }
}

/// What a whole transfer moves, from the channel's registers when its CHCR
/// is written.
#[must_use]
pub fn moves(regions: &Regions, chcr: u32, madr: u32, qwc: u32, tadr: u32) -> Moves {
    let mut chain = Chain::new(chcr, madr, qwc, tadr);
    let mut blocks = Vec::new();
    while let Some(block) = chain.next_block(regions) {
        blocks.push(block);
    }
    Moves {
        blocks,
        tags: chain.tags,
    }
}

/// A scratchpad transfer, done when kicked: toSPR (`to_spr`) copies the
/// blocks (and with TTE their tags) to the scratchpad at `sadr`; fromSPR
/// copies the scratchpad from `sadr` to memory at MADR (normal mode).
pub fn scratchpad(regions: &mut Regions, to_spr: bool, moves: &Moves, sadr: u32) {
    let mut at = sadr & 0x3FF0;
    let mut put = |regions: &mut Regions, q: u128| {
        regions.scratchpad[at as usize..at as usize + 16].copy_from_slice(&q.to_le_bytes());
        at = (at + 16) & 0x3FF0;
    };
    if to_spr {
        for &(tag, from, n) in &moves.blocks {
            if let Some(tag) = tag {
                put(regions, tag);
            }
            for i in 0..n {
                let q = qword(regions, from.wrapping_add(16 * i));
                put(regions, q);
            }
        }
    } else {
        for &(_, to, n) in &moves.blocks {
            for i in 0..n {
                let from = (at as usize + 16 * i as usize) & 0x3FF0;
                let mut raw = [0; 16];
                raw.copy_from_slice(&regions.scratchpad[from..from + 16]);
                let dest = (to.wrapping_add(16 * i) & 0x01FF_FFF0) as usize;
                if let Some(d) = regions.ram.get_mut(dest..dest + 16) {
                    d.copy_from_slice(&raw);
                }
            }
            at = (at + 16 * n) & 0x3FF0;
        }
    }
}

/// The time a transfer takes.
#[must_use]
pub fn duration(moves: &Moves) -> u64 {
    CYCLES_PER_TAG * (1 + u64::from(moves.tags)) + CYCLES_PER_QWORD * moves.qwords()
}

/// Appends a GIF or VIF1 transfer to `$SLY_RS_DMA_DUMP` when it is set:
/// records of (channel, cycle, word count) then the words, little-endian.
/// A debugging aid for graphics work; the data comes from the game, so the
/// dump never goes into the repository.
pub fn dump(regions: &Regions, channel: usize, cycle: u64, moves: &Moves) {
    use std::io::Write as _;
    static FILE: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
        std::sync::OnceLock::new();
    if !matches!(channel, 1 | 2) {
        return;
    }
    let Some(file) = FILE.get_or_init(|| {
        let path = std::env::var_os("SLY_RS_DMA_DUMP")?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        Some(std::sync::Mutex::new(file))
    }) else {
        return;
    };
    let words = moves.words(regions);
    let mut record = Vec::with_capacity(16 + 4 * words.len());
    record.extend_from_slice(&(channel as u32).to_le_bytes());
    record.extend_from_slice(&cycle.to_le_bytes());
    record.extend_from_slice(&(words.len() as u32).to_le_bytes());
    for w in words {
        record.extend_from_slice(&w.to_le_bytes());
    }
    if let Ok(mut file) = file.lock() {
        let _ = file.write_all(&record);
    }
}
