//! The CD/DVD RPC servers of Sony's `cdvdfsv`, answering the EE's `libcdvd`.
//! Server ids, function numbers and data layouts as in the SDK's
//! `libcdvd` (ps2sdk `ee/rpc/cdvd`).
//!
//! Reads are synchronous: the sectors are in EE memory by the time the
//! reply goes back.

use ps2_core::Memory;

use crate::{Call, Server};

/// `sceCdInit`.
pub const INIT: u32 = 0x8000_0592;
/// S-commands.
pub const SCMD: u32 = 0x8000_0593;
/// N-commands: reads and the like.
pub const NCMD: u32 = 0x8000_0595;
/// Power-off notification.
pub const POWER_OFF: u32 = 0x8000_0596;
/// `sceCdSearchFile`.
pub const SEARCH_FILE: u32 = 0x8000_0597;
/// `sceCdDiskReady`.
pub const DISK_READY: u32 = 0x8000_059A;

/// Disc type: PlayStation 2 DVD.
const PS2_DVD: u32 = 0x14;
/// `sceCdDiskReady`: complete (ready).
const READY: u32 = 2;
/// `sceCdStatus`: paused, disc spinning.
const STATUS_PAUSED: u32 = 0x0A;

/// Raw 2048-byte sectors of the disc.
pub trait Sectors {
    /// `count` sectors from `lba`, or `None` past the end of the disc.
    fn read(&self, lba: u32, count: u32) -> Option<Vec<u8>>;
}

fn word(data: &[u8], index: usize) -> u32 {
    data.get(4 * index..4 * index + 4)
        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn reply(words: &[u32], size: u32) -> Vec<u8> {
    let mut bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    bytes.resize(size as usize, 0);
    bytes
}

/// A BCD byte.
fn bcd(value: u32) -> u32 {
    (value / 10) << 4 | (value % 10)
}

/// The CD/DVD servers, sharing the disc.
pub struct Cdvd {
    disc: Box<dyn Sectors>,
    /// Sectors read so far.
    pub sectors_read: u64,
}

impl Cdvd {
    /// Servers reading from `disc`.
    #[must_use]
    pub fn new(disc: Box<dyn Sectors>) -> Self {
        Self {
            disc,
            sectors_read: 0,
        }
    }

    fn scmd(call: &Call<'_>) -> Vec<u8> {
        match call.function {
            // sceCdReadClock: result, then stat, second, minute, hour, pad,
            // day, month, year in BCD. A fixed time keeps runs repeatable.
            0x01 => {
                let clock = bcd(0) << 8 | bcd(0) << 16 | bcd(12) << 24;
                let date = bcd(24) << 8 | bcd(8) << 16 | bcd(2) << 24;
                reply(&[1, clock, date], call.reply_size)
            }
            0x03 => reply(&[PS2_DVD], call.reply_size),
            0x04 => reply(&[0], call.reply_size),
            0x0C => reply(&[STATUS_PAUSED], call.reply_size),
            _ => reply(&[1], call.reply_size),
        }
    }

    /// `sceCdRead`/`sceCdDvdRead`: {lbn, sectors, buffer, mode, alignment
    /// data, position}. The sectors go straight to the EE buffer; the
    /// alignment record says nothing is left to copy.
    fn read(&mut self, call: &Call<'_>, mem: &mut Memory) {
        let (lbn, count, buffer) = (word(call.data, 0), word(call.data, 1), word(call.data, 2));
        let (align, position) = (word(call.data, 4), word(call.data, 5));
        match self.disc.read(lbn, count) {
            Some(data) => {
                for (i, chunk) in data.chunks(4).enumerate() {
                    let mut w = [0; 4];
                    w[..chunk.len()].copy_from_slice(chunk);
                    mem.write32(buffer + 4 * i as u32, u32::from_le_bytes(w));
                }
                self.sectors_read += u64::from(count);
                tracing::debug!(target: "iop::cdvd", "read {count} sectors at LBA {lbn} into {buffer:#x}");
            }
            None => {
                tracing::warn!(target: "iop::cdvd", "read past the end of the disc: {count} sectors at LBA {lbn}");
            }
        }
        if align != 0 {
            mem.write32(align, 0);
            mem.write32(align + 4, 0);
        }
        if position != 0 {
            mem.write32(position, count * 2048);
        }
    }
}

impl Server for Cdvd {
    fn name(&self) -> &'static str {
        "cdvdfsv"
    }

    fn call(&mut self, call: &Call<'_>, mem: &mut Memory) -> Vec<u8> {
        match call.sid {
            INIT => reply(&[1], call.reply_size),
            SCMD => Self::scmd(call),
            NCMD => {
                match call.function {
                    0x01 | 0x03 => self.read(call, mem),
                    0x0E => return reply(&[READY], call.reply_size),
                    // Seek, standby, stop, pause: nothing to wait for.
                    0x05..=0x08 => {}
                    other => {
                        tracing::warn!(target: "iop::cdvd", "N-command {other:#x} not implemented");
                    }
                }
                reply(&[1], call.reply_size)
            }
            DISK_READY => reply(&[READY], call.reply_size),
            _ => {
                tracing::warn!(target: "iop::cdvd", "server {:#x} function {} not implemented", call.sid, call.function);
                reply(&[0], call.reply_size)
            }
        }
    }
}
