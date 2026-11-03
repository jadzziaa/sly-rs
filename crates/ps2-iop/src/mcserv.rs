//! Sony's `mcserv` RPC server (`0x80000400`), the memory card service the
//! EE's `libmc` calls, in the numbering the disc's MCSERV 2.0f answers
//! (init `0xFE`, `GetInfo` 0x01, ...). Protocol and structures follow
//! ps2sdk's `libmc.c`, `mcserv.c` and `libmc-common.h`, which document it
//! (read for facts, not copied).
//!
//! No memory card is inserted yet: detection fails in both slots, as it
//! does on a console without a card. A card image comes later.

use ps2_core::Memory;

use crate::{Call, Server};

/// The server.
pub const MCSERV: u32 = 0x8000_0400;

/// The versions the init call reports: the disc's MCSERV and MCMAN (`libmc`
/// requires at least 0x20A and 0x20E).
const MCSERV_VERSION: u32 = 0x20F;
const MCMAN_VERSION: u32 = 0x220;

/// `McDetectCard2` with nothing in the slot: below -9.
const NO_CARD: i32 = -10;

fn word(data: &[u8], index: usize) -> u32 {
    data.get(4 * index..4 * index + 4)
        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// The memory card server.
#[derive(Default)]
pub struct Mcserv;

impl Server for Mcserv {
    fn name(&self) -> &'static str {
        "mcserv"
    }

    fn call(&mut self, call: &Call<'_>, mem: &mut Memory) -> Vec<u8> {
        let words: Vec<u32> = match call.function {
            // Init: result, then the server's and the card manager's version.
            0xFE => vec![0, MCSERV_VERSION, MCMAN_VERSION],
            // GetInfo: the descriptor (fd, port, slot, size, offset, origin,
            // buffer, param) says where in EE memory { type, free,
            // formatted } go.
            0x01 => {
                let param = word(call.data, 7);
                if param != 0 {
                    for k in 0..3 {
                        mem.write32(param + 4 * k, 0);
                    }
                }
                vec![NO_CARD.cast_unsigned()]
            }
            other => {
                tracing::debug!(target: "iop::mc", "mcserv function {other:#x}: no card");
                vec![NO_CARD.cast_unsigned()]
            }
        };
        tracing::debug!(target: "iop::mc", "mcserv function {:#x} (port {}, slot {}): {words:x?}", call.function, word(call.data, 1), word(call.data, 2));
        let mut reply: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        reply.resize(call.reply_size as usize, 0);
        reply
    }
}
