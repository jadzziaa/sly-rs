//! `gs-replay`: plays a GS dump through sly-rs's GS alone, timing it.

use std::io::{self, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use ps2_gs::Privileged;
use ps2_gs::dump::{self, Packet};

pub(crate) fn run(path: &Path, repeat: u32, screenshot: Option<&Path>) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut out = io::stdout().lock();
    let mut best = Duration::MAX;
    for _ in 0..repeat.max(1) {
        let (mut gs, mut privileged, packets) =
            dump::read(&bytes).map_err(|e| anyhow!("{}: {e}", path.display()))?;
        let started = Instant::now();
        let mut fields = 0;
        for packet in packets {
            match packet {
                Packet::Transfer(path, data) => gs.gif(path, data),
                Packet::Registers(block) => privileged = block,
                Packet::VSync(_) => fields += 1,
            }
        }
        let elapsed = started.elapsed();
        best = best.min(elapsed);
        writeln!(
            out,
            "{fields} fields, {} primitives in {:.3} s",
            gs.drawn,
            elapsed.as_secs_f64()
        )?;
        if let Some(png) = screenshot {
            let frame = gs.display(&Privileged::from_block(&privileged[..]));
            std::fs::write(png, frame.png())
                .with_context(|| format!("writing {}", png.display()))?;
        }
    }
    if repeat > 1 {
        writeln!(out, "best {:.3} s", best.as_secs_f64())?;
    }
    Ok(())
}
