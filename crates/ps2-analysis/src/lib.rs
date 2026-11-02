//! Analysis of PS2 executables, ahead of recompilation:
//!
//! - [`Image`]: the executable loaded at its addresses.
//! - [`vu_overlays`]: the VU microcode `dvp-ld` placed in `.vutext`.
//! - [`discover`]: functions, padding and data in `.text`, from the code
//!   alone (entry point, `jal` targets, code pointers, a linear sweep), with
//!   jump tables recovered first so `switch` bodies stay whole.
//! - [`Cfg`]: basic blocks and edges of a function, delay slots included.
//! - [`syscall_stubs`]: the EE kernel call wrappers, by syscall number.
//! - [`fingerprint`]: a function's code hashed without its addresses, to
//!   recognise the same function (SDK libraries, shared engine code) in
//!   another executable.

mod cfg;
mod fingerprint;
mod functions;
mod image;
mod overlays;
mod switch;
mod syscalls;

pub use cfg::{Block, Cfg, Edge, EdgeKind};
pub use fingerprint::{MIN_INSTRUCTIONS, fingerprint};
pub use functions::{Anomaly, Discovery, Function, Origin, discover};
pub use image::{Image, Section, SectionKind};
pub use overlays::{Overlay, vu_overlays};
pub use switch::JumpTable;
pub use syscalls::{SyscallStub, syscall_name, syscall_stubs};

/// Errors from loading or analysing an executable.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AnalysisError {
    /// The ELF file could not be parsed.
    #[error("ELF: {0}")]
    Elf(#[from] object::Error),
    /// The executable's layout is not what the analysis expects.
    #[error("layout: {0}")]
    Layout(&'static str),
}

#[cfg(test)]
mod tests;
