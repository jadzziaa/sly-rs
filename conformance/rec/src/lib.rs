//! The conformance program (`conformance/conformance.elf`), recompiled by
//! `ps2-codegen` when this crate builds. Its tests run it on the machine
//! with the generated code and compare the output with the interpreter's.

#![allow(
    missing_docs,
    unused,
    clippy::all,
    clippy::pedantic,
    unreachable_pub,
    reason = "generated code"
)]

include!(concat!(env!("OUT_DIR"), "/rec.rs"));
