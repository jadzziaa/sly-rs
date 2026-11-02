//! The Emotion Engine's state and the semantics of its instructions, shared
//! by the interpreter (`ps2-ee`) and recompiled code.
//!
//! - [`Cpu`]: registers of the R5900 core, its FPU and VU0.
//! - [`Memory`]: the EE's address space; hardware registers go to an
//!   [`Mmio`] handler the machine provides.
//! - [`execute`]: one instruction's effect, and what the machine must do
//!   next ([`Outcome`]).
//! - [`rec`]: what recompiled code runs on.
//! - [`float`]: PS2 single-precision arithmetic (no Inf/NaN, no denormals,
//!   round toward zero, no guard bits).

// Instruction semantics narrow, widen and reinterpret register values on
// every line; the casts are the hardware's behaviour, not accidents.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::many_single_char_names,
    clippy::enum_glob_use,
    clippy::semicolon_if_nothing_returned,
    reason = "instruction semantics work on register bit patterns"
)]

pub mod cpu;
mod exec;
pub mod float;
pub mod mem;
mod mmi;
pub mod rec;
pub mod vu0;

pub use cpu::{Cpu, Vu0};
pub use exec::{Outcome, execute};
pub use mem::{Memory, Mmio, NoMmio, Regions};
pub use ps2_isa::ee::Insn;

#[cfg(test)]
mod tests;
