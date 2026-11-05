//! Sony's `mcserv` RPC server (`0x80000400`), the memory card service the
//! EE's `libmc` calls, in the numbering the disc's MCSERV 2.0f answers
//! (init `0xFE`, `GetInfo` 0x01, ...). Protocol and structures follow
//! ps2sdk's `libmc.c`, `mcserv.c` and `libmc-common.h`, which document it
//! (read for facts, not copied).
//!
//! A card may be inserted in port 0, slot 0; its file system is
//! [`ps2_memcard::Files`], answering as the card manager (`mcman`) does.
//! The other slots are empty, as on a console with one card. Data goes to
//! and from EE memory directly: a read's whole buffer is written at once,
//! and its alignment record says nothing is left for `libmc` to copy.

use std::cell::RefCell;
use std::rc::Rc;

use ps2_core::Memory;
use ps2_memcard::Files;

use crate::{Call, Server};

/// The server.
pub const MCSERV: u32 = 0x8000_0400;

/// The versions the init call reports: the disc's MCSERV and MCMAN (`libmc`
/// requires at least 0x20A and 0x20E).
const MCSERV_VERSION: u32 = 0x20F;
const MCMAN_VERSION: u32 = 0x220;

/// `McDetectCard2` with nothing in the slot: below -9.
const NO_CARD: i32 = -10;
/// `sceMcTypePS2`.
const PS2_CARD: u32 = 2;
/// Bytes in the record `GetInfo` and `Read` fill in (`mcEndParam2_t`).
const END_PARAM_SIZE: usize = 192;
/// Where `mcEndParam2_t` says whether the card is formatted.
const END_PARAM_FORMATTED: usize = 144;
/// Bytes in a `sceMcTblGetDir` record.
const TABLE_SIZE: u32 = 64;
/// Bytes `ChDir` writes back for the current directory.
const CURRENT_DIR_SIZE: usize = 1024;

/// The card slot, shared with the machine that inserts the card.
pub type Slot = Rc<RefCell<Option<Files>>>;

fn word(data: &[u8], index: usize) -> u32 {
    data.get(4 * index..4 * index + 4)
        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A path in a name parameter (`mcNameParam_t`: port, slot, flags, maximum
/// entries, table, then the name at byte 20).
fn name(data: &[u8]) -> &[u8] {
    data.get(20..)
        .and_then(|name| name.split(|&b| b == 0).next())
        .unwrap_or_default()
}

/// Writes bytes into EE memory.
fn store(mem: &mut Memory, address: u32, bytes: &[u8]) {
    let Ok(len) = u32::try_from(bytes.len()) else {
        return;
    };
    if let Some(slice) = mem.regions.slice_mut(address, len) {
        slice.copy_from_slice(bytes);
    } else {
        tracing::warn!(target: "iop::mc", "{len} bytes to {address:#x}: not EE memory");
    }
}

/// Bytes of EE memory.
fn fetch(mem: &mut Memory, address: u32, len: u32) -> Vec<u8> {
    mem.regions
        .slice_mut(address, len)
        .map(|slice| slice.to_vec())
        .unwrap_or_default()
}

/// The memory card server.
pub struct Mcserv {
    card: Slot,
    /// What the last directory listing found, and how much of it was
    /// sent (`GetDir` continues a listing when its flags are not 0).
    listing: (Vec<[u8; 64]>, usize),
}

impl Mcserv {
    /// The server for the card in `card`.
    #[must_use]
    pub fn new(card: Slot) -> Self {
        Self {
            card,
            listing: (Vec::new(), 0),
        }
    }

    /// `GetInfo` (`McGetInfo2`): the result, and { type, free clusters,
    /// formatted } where the descriptor's `param` points, each only when
    /// asked for (`origin`, `offset`, `size`).
    fn info(files: Option<&mut Files>, data: &[u8], mem: &mut Memory) -> i32 {
        let mut end = [0u8; END_PARAM_SIZE];
        let result = match files {
            None => NO_CARD,
            Some(files) => {
                let mut result = files.detect();
                if word(data, 5) != 0 {
                    end[0..4].copy_from_slice(&PS2_CARD.to_le_bytes());
                }
                if word(data, 4) != 0 {
                    match files.card().free_clusters() {
                        0 => result = ps2_memcard::result::FULL_DEVICE,
                        free => end[4..8].copy_from_slice(&free.to_le_bytes()),
                    }
                }
                if word(data, 3) != 0 {
                    end[END_PARAM_FORMATTED] = 1;
                }
                result
            }
        };
        let param = word(data, 7);
        if param != 0 {
            store(mem, param, &end);
        }
        result
    }

    /// Runs a call on the card in port 0, slot 0.
    fn card_call(&mut self, files: &mut Files, call: &Call<'_>, mem: &mut Memory) -> i32 {
        let data = call.data;
        let fd = word(data, 0).cast_signed();
        let detected = files.detect();
        if detected != 0 {
            return detected;
        }
        match call.function {
            // Open (`sceMcOpen`, `sceMcMkDir`): name parameter, flags.
            0x02 => files.open(name(data), word(data, 2)),
            0x03 => files.close(fd),
            // Seek: descriptor, offset, origin.
            0x04 => files.seek(fd, word(data, 4).cast_signed(), word(data, 5)),
            // Read: descriptor, size, buffer, the alignment record.
            0x05 => {
                let (size, buffer, param) = (word(data, 3), word(data, 6), word(data, 7));
                let result = match files.read_file(fd, size) {
                    Ok(bytes) => {
                        store(mem, buffer, &bytes);
                        i32::try_from(bytes.len()).unwrap_or(i32::MAX)
                    }
                    Err(code) => code,
                };
                if param != 0 {
                    store(mem, param, &[0; END_PARAM_SIZE]);
                }
                result
            }
            // Write: the first `origin` bytes inline (`data`, byte 32),
            // `size` more at `buffer`.
            0x06 => {
                let (size, head, buffer) = (word(data, 3), word(data, 5), word(data, 6));
                let mut bytes = data
                    .get(32..32 + (head as usize).min(16))
                    .unwrap_or_default()
                    .to_vec();
                bytes.extend(fetch(mem, buffer, size));
                files.write_file(fd, &bytes)
            }
            0x0A => files.flush(fd),
            // ChDir: the new current directory's path goes where the
            // parameter's table pointer says.
            0x0C => {
                let (result, path) = match files.change_dir(name(data)) {
                    Ok(path) => (0, path),
                    Err(code) => (code, Vec::new()),
                };
                let to = word(data, 4);
                if to != 0 && result == 0 {
                    let mut out = path;
                    out.resize(CURRENT_DIR_SIZE, 0);
                    store(mem, to, &out);
                }
                result
            }
            // GetDir: flags 0 start a listing, others continue it; up to
            // `maxent` records at the table. Sly 1 looks for its save
            // directory with `maxent` -1 and no table, and takes a result
            // of 0 for none: a negative `maxent` counts every match.
            0x0D => {
                if word(data, 2) as u16 == 0 {
                    self.listing = match files.list(name(data), usize::MAX) {
                        Ok(rows) => (rows, 0),
                        Err(code) => {
                            self.listing = (Vec::new(), 0);
                            return code;
                        }
                    };
                }
                let max = usize::try_from(word(data, 3).cast_signed()).unwrap_or(usize::MAX);
                let (rows, sent) = &mut self.listing;
                let take = rows.len().saturating_sub(*sent).min(max);
                let mut table = word(data, 4);
                for row in &rows[*sent..*sent + take] {
                    if table != 0 {
                        store(mem, table, row);
                        table += TABLE_SIZE;
                    }
                }
                *sent += take;
                i32::try_from(take).unwrap_or(i32::MAX)
            }
            // SetFileInfo: the record to take from is at the table pointer.
            0x0E => {
                let table: [u8; 64] = fetch(mem, word(data, 4), TABLE_SIZE)
                    .try_into()
                    .unwrap_or([0; 64]);
                files.set_info(name(data), &table, word(data, 2))
            }
            // Delete: flags 0 delete, otherwise undelete.
            0x0F => files.delete(name(data), word(data, 2) != 0),
            0x10 => files.format(),
            0x12 => files.free_entries(name(data)),
            other => {
                tracing::warn!(target: "iop::mc", "mcserv function {other:#x}: not supported");
                ps2_memcard::result::DENIED_PERMIT
            }
        }
    }
}

impl Server for Mcserv {
    fn name(&self) -> &'static str {
        "mcserv"
    }

    fn call(&mut self, call: &Call<'_>, mem: &mut Memory) -> Vec<u8> {
        // Name parameters start with the port and slot; descriptors with a
        // handle, then the port and slot.
        let names = matches!(call.function, 0x02 | 0x0C..=0x0F | 0x12);
        let words: Vec<u32> = if call.function == 0xFE {
            // Init: result, then the server's and the card manager's version.
            vec![0, MCSERV_VERSION, MCMAN_VERSION]
        } else {
            let card = Rc::clone(&self.card);
            let mut card = card.borrow_mut();
            let (port, slot) = if names {
                (word(call.data, 0), word(call.data, 1))
            } else if matches!(call.function, 0x01 | 0x10 | 0x11) {
                (word(call.data, 1), word(call.data, 2))
            } else {
                (0, 0)
            };
            let files = card.as_mut().filter(|_| port == 0 && slot == 0);
            let result = match (call.function, files) {
                (0x01, files) => Self::info(files, call.data, mem),
                (_, None) => NO_CARD,
                (_, Some(files)) => self.card_call(files, call, mem),
            };
            vec![result.cast_unsigned()]
        };
        tracing::debug!(
            target: "iop::mc",
            "mcserv function {:#x}{} {:x?}: {:?}",
            call.function,
            if names {
                format!(" {:?}", String::from_utf8_lossy(name(call.data)))
            } else {
                String::new()
            },
            (0..8).map(|k| word(call.data, k)).collect::<Vec<_>>(),
            words.first().map(|w| w.cast_signed()),
        );
        let mut reply: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        reply.resize(call.reply_size as usize, 0);
        reply
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// A name parameter: port 0, slot 0, flags, maximum entries, table,
    /// then the name.
    fn named(flags: u32, maxent: u32, table: u32, name: &str) -> Vec<u8> {
        let mut data = words(&[0, 0, flags, maxent, table]);
        data.extend_from_slice(name.as_bytes());
        data.resize(20 + 1024, 0);
        data
    }

    fn call(server: &mut Mcserv, mem: &mut Memory, function: u32, data: &[u8]) -> i32 {
        let reply = server.call(
            &Call {
                sid: MCSERV,
                function,
                data,
                reply_size: 4,
            },
            mem,
        );
        i32::from_le_bytes([reply[0], reply[1], reply[2], reply[3]])
    }

    #[test]
    fn a_card_answers_as_libmc_expects() {
        let card: Slot = Rc::default();
        let mut server = Mcserv::new(Rc::clone(&card));
        let mut mem = Memory::new(Box::new(ps2_core::NoMmio));
        // GetInfo: no card, then the first detection of one, then the same
        // card; type, free clusters and the formatted flag where `param`
        // points.
        let info = words(&[0, 0, 0, 1, 1, 1, 0, 0x1000]);
        assert_eq!(call(&mut server, &mut mem, 0x01, &info), NO_CARD);
        *card.borrow_mut() = Some(Files::new(
            ps2_memcard::Card::formatted(),
            crate::cdvd::CLOCK,
        ));
        assert_eq!(call(&mut server, &mut mem, 0x01, &info), -1);
        assert_eq!(call(&mut server, &mut mem, 0x01, &info), 0);
        assert_eq!(mem.read32(0x1000), PS2_CARD);
        assert_eq!(mem.read32(0x1004), 8134);
        assert_eq!(mem.read32(0x1000 + 144), 1);

        assert_eq!(
            call(&mut server, &mut mem, 0x02, &named(0x40, 0, 0, "/DIR")),
            0
        );
        let fd = call(
            &mut server,
            &mut mem,
            0x02,
            &named(0x203, 0, 0, "/DIR/file"),
        );
        assert_eq!(fd, 0);
        // Write: three bytes inline, five more from EE memory.
        mem.regions
            .slice_mut(0x2000, 5)
            .unwrap_or_default()
            .copy_from_slice(b"defgh");
        let mut write = words(&[0, 0, 0, 5, 0, 3, 0x2000, 0]);
        write.extend_from_slice(b"abc");
        write.resize(48, 0);
        assert_eq!(call(&mut server, &mut mem, 0x06, &write), 8);
        assert_eq!(
            call(&mut server, &mut mem, 0x04, &words(&[0, 0, 0, 0, 2, 0])),
            2
        );
        // Read: straight into the buffer, nothing left for libmc to copy.
        mem.write32(0x3000, 0xFFFF_FFFF);
        let read = words(&[0, 0, 0, 100, 0, 0, 0x4000, 0x3000]);
        assert_eq!(call(&mut server, &mut mem, 0x05, &read), 6);
        assert_eq!(
            mem.regions.slice_mut(0x4000, 6).map(|s| s.to_vec()),
            Some(b"cdefgh".to_vec())
        );
        assert_eq!(mem.read32(0x3000), 0);
        assert_eq!(call(&mut server, &mut mem, 0x03, &words(&[0])), 0);

        // GetDir: with no table and a negative maximum, a count; with one,
        // the records.
        assert_eq!(
            call(&mut server, &mut mem, 0x0D, &named(0, u32::MAX, 0, "/DIR")),
            1
        );
        assert_eq!(
            call(&mut server, &mut mem, 0x0D, &named(0, 10, 0x5000, "/DIR/*")),
            3
        );
        let names: Vec<u8> = mem
            .regions
            .slice_mut(0x5000 + 128 + 32, 4)
            .unwrap_or_default()
            .to_vec();
        assert_eq!(names, b"file");
        assert_eq!(mem.read32(0x5000 + 128 + 16), 8);
        // A slot without a card.
        let mut other = named(0, 0, 0, "/DIR");
        other[0] = 1;
        assert_eq!(call(&mut server, &mut mem, 0x02, &other), NO_CARD);
    }
}
