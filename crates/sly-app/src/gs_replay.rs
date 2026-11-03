//! `gs-replay`: plays a GS dump through sly-rs's GS alone, timing it.

use std::io::{self, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use ps2_gs::Privileged;
use ps2_gs::dump::{self, Packet};

pub(crate) fn run(
    path: &Path,
    repeat: u32,
    threads: usize,
    screenshot: Option<&Path>,
    frames: Option<&Path>,
) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if let Some(dir) = frames {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut out = io::stdout().lock();
    let mut best = Duration::MAX;
    for _ in 0..repeat.max(1) {
        let (mut gs, mut privileged, packets) =
            dump::read(&bytes).map_err(|e| anyhow!("{}: {e}", path.display()))?;
        gs.set_threads(threads);
        let started = Instant::now();
        let mut fields = 0;
        for packet in packets {
            match packet {
                Packet::Transfer(path, data) => gs.gif(path, data),
                Packet::Registers(block) => privileged = block,
                Packet::VSync(_) => {
                    fields += 1;
                    if let Some(dir) = frames {
                        // Numbered as PCSX2 numbers a dump's frames.
                        gs.flush();
                        let frame = gs.display(&Privileged::from_block(&privileged[..]));
                        let png = dir.join(format!("frame{:05}.png", fields - 1));
                        std::fs::write(&png, frame.png())
                            .with_context(|| format!("writing {}", png.display()))?;
                    }
                }
            }
        }
        gs.flush();
        let elapsed = started.elapsed();
        best = best.min(elapsed);
        writeln!(
            out,
            "{fields} fields, {} primitives in {:.3} s",
            gs.drawn,
            elapsed.as_secs_f64()
        )?;
        let [pool, serial, uploads] = gs.drawer_times();
        writeln!(
            out,
            "  {} primitives drawn alone; {} batches, {} on the pool; the drawer: {:.3} s on the pool, \
             {:.3} s one primitive after another, {:.3} s uploading; the GS thread queueing and drawing {:.3} s, waiting for the drawer {:.3} s",
            gs.alone,
            gs.batches,
            gs.parallel_batches,
            pool.as_secs_f64(),
            serial.as_secs_f64(),
            uploads.as_secs_f64(),
            gs.draw_time.as_secs_f64(),
            gs.drawer_waits().as_secs_f64()
        )?;
        let hash = gs
            .mem
            .to_bytes()
            .iter()
            .fold(0xCBF2_9CE4_8422_2325u64, |h, &b| {
                (h ^ u64::from(b)).wrapping_mul(0x100_0000_01B3)
            });
        writeln!(out, "GS memory hash {hash:016x}")?;
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
