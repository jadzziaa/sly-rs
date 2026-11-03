//! `sly-rs elf`: run a PS2 executable that is not a game (the conformance
//! programs) and print its debug console, or the IOP's with `--iop`.

use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result};
use ps2_analysis::Image;
use ps2_ee::{FIELD_CYCLES, Machine, Stop};

pub(crate) fn run(path: &Path, fields: u64, iop: bool) -> Result<()> {
    let elf = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let image = Image::from_elf(&elf)?;
    let argv0 = format!("host:{}", path.display());
    let mut machine = Machine::boot(&image, &argv0)
        .map_err(|at| anyhow::anyhow!("section at {at:#x} does not fit"))?;
    // host: is the program's folder, as PCSX2's HostFs has it.
    let dir = path.parent().map_or_else(|| ".".into(), Path::to_path_buf);
    machine.iop.attach_files(Box::new(ps2_iop::HostFiles(dir)));
    let stop = machine.run(fields * FIELD_CYCLES);
    let console = if iop {
        machine.iop.core.console.clone()
    } else {
        machine.hw.borrow().console.clone()
    };
    io::stdout().lock().write_all(&console)?;
    if !matches!(stop, Stop::Budget | Stop::Exit(_)) {
        anyhow::bail!("stopped: {stop:?} at {:#010x}", machine.cpu.pc);
    }
    Ok(())
}
