//! Sony's `loadfile` RPC server (id `0x80000006`): IOP module loading for
//! the EE's `sceSifLoadModule`. Modules are recorded, not run: their
//! services are emulated where the game needs them.
//!
//! Requests (as in the SDK's `_lf_module_load_arg`): result word, module
//! result word, then the module's path. Replies: the module id, then its
//! start result (0, resident).

use ps2_core::Memory;

use crate::{Call, Server};

/// The server id.
pub const LOADFILE: u32 = 0x8000_0006;

/// The loader.
pub struct Loadfile {
    version: [u8; 4],
    /// Modules loaded, by path, in order.
    pub modules: Vec<String>,
}

impl Loadfile {
    /// A loader that reports `version` (four ASCII digits, as the IOP image
    /// the game boots has it: `2430`, `3000`, …).
    #[must_use]
    pub fn new(version: [u8; 4]) -> Self {
        Self {
            version,
            modules: Vec::new(),
        }
    }
}

impl Server for Loadfile {
    fn name(&self) -> &'static str {
        "loadfile"
    }

    fn call(&mut self, call: &Call<'_>, _: &mut Memory) -> Vec<u8> {
        let mut reply = match call.function {
            // Version query.
            0xFF => self.version.to_vec(),
            // Load a module from a file (0) or from EE memory (6).
            0 | 6 => {
                let path: Vec<u8> = call
                    .data
                    .iter()
                    .skip(8)
                    .take_while(|&&b| b != 0)
                    .copied()
                    .collect();
                let path = String::from_utf8_lossy(&path).into_owned();
                tracing::debug!(target: "iop::loadfile", "load module {path:?}");
                self.modules.push(path);
                let id = u32::try_from(self.modules.len()).unwrap_or(u32::MAX);
                [id.to_le_bytes(), 0u32.to_le_bytes()].concat()
            }
            other => {
                tracing::warn!(target: "iop::loadfile", "function {other:#x} not implemented");
                Vec::new()
            }
        };
        reply.resize(call.reply_size as usize, 0);
        reply
    }
}
