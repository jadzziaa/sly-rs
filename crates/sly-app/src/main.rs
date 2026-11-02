//! `sly-rs`: the command line, with the interpreter only. Recompiled games
//! are binaries of their own (`cargo xtask recomp`).

use std::process::ExitCode;

fn main() -> ExitCode {
    sly_app::main(None)
}
