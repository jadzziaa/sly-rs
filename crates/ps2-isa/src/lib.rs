//! PlayStation 2 instruction sets: decoding and disassembly.
//!
//! - [`ee`]: the Emotion Engine's R5900 core, including MMI, the FPU and VU0
//!   macro mode.
//! - [`vu`]: VU micro mode, the 64-bit instruction pairs VU0 and VU1 run.
//!
//! Decoding is `const`, allocation-free and total: every word decodes, to an
//! operation or to `Invalid`. The crate has no dependencies and builds
//! without `std`, so generated code and tools can share it.

#![cfg_attr(not(test), no_std)]
// Decoding extracts bit fields and reinterprets them as narrower or signed
// values; that is the job, not an accident.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "instruction decoding extracts and reinterprets bit fields"
)]

pub mod ee;
mod regs;
pub mod vu;

pub use regs::{GPR_NAMES, VU_COMPONENTS};
