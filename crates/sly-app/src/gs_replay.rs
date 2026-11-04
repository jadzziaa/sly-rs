//! `gs-replay`: plays a GS dump through sly-rs's GS alone, timing it.

use std::io::{self, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use ps2_gs::Privileged;
use ps2_gs::dump::{self, Packet};

#[allow(clippy::cast_precision_loss, reason = "a report")]
pub(crate) fn run(
    path: &Path,
    repeat: u32,
    threads: usize,
    screenshot: Option<&Path>,
    frames: Option<&Path>,
    states: bool,
    gpu: bool,
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
        // The GS hands its work on only when drawing on several threads.
        gs.set_threads(if gpu { threads.max(2) } else { threads });
        if states {
            gs.states = Some(std::collections::HashMap::new());
        }
        let mut check = if gpu {
            let shared = ps2_gs_wgpu::Shared(std::sync::Arc::new(std::sync::Mutex::new(
                ps2_gs_wgpu::GpuGs::new().map_err(|e| anyhow!(e))?,
            )));
            gs.set_tap(Some(Box::new(shared.clone())));
            Some(GpuCheck {
                shared,
                first: None,
            })
        } else {
            None
        };
        let started = Instant::now();
        let mut fields = 0;
        for packet in packets {
            match packet {
                Packet::Transfer(path, data) => gs.gif(path, data),
                Packet::Registers(block) => privileged = block,
                Packet::VSync(_) => {
                    fields += 1;
                    if let Some(check) = &mut check {
                        check.field(&mut gs, fields);
                    }
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
            "{fields} fields, {} primitives in {:.3} s; transfers host to local {}, local to host {}, local to local {}",
            gs.drawn,
            elapsed.as_secs_f64(),
            gs.transfers[0],
            gs.transfers[1],
            gs.transfers[2],
        )?;
        if let Some(states) = gs.states.take() {
            report_states(&mut out, states)?;
        }
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
        if let Some(check) = &mut check {
            check.report(&mut out, &mut gs)?;
        }
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

/// `--gpu`: the GPU drawing alongside, compared after every field.
struct GpuCheck {
    shared: ps2_gs_wgpu::Shared,
    /// The first field after which the two memories differed, and in how
    /// many words.
    first: Option<(u32, usize)>,
}

impl GpuCheck {
    /// Words of the two memories that differ, and the first.
    fn compare(&self, gs: &mut ps2_gs::Gs) -> (usize, Option<usize>, Vec<u32>) {
        gs.flush();
        let cpu = gs.mem.to_words();
        let gpu = self
            .shared
            .0
            .lock()
            .map(|g| g.read_memory())
            .unwrap_or_default();
        let differ: Vec<usize> = (0..cpu.len())
            .filter(|&i| gpu.get(i) != Some(&cpu[i]))
            .collect();
        (differ.len(), differ.first().copied(), gpu)
    }

    fn field(&mut self, gs: &mut ps2_gs::Gs, field: u32) {
        if self.first.is_none() {
            let (n, _, _) = self.compare(gs);
            if n > 0 {
                self.first = Some((field, n));
            }
        }
    }

    fn report(&mut self, out: &mut impl io::Write, gs: &mut ps2_gs::Gs) -> Result<()> {
        let (n, first, words) = self.compare(gs);
        let hash = words
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .fold(0xCBF2_9CE4_8422_2325u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x100_0000_01B3)
            });
        let (name, batches) = self
            .shared
            .0
            .lock()
            .map(|g| (g.name.clone(), g.batches))
            .unwrap_or_default();
        writeln!(
            out,
            "GPU ({name}): {batches} batches, {} copies resynced; GPU memory hash {hash:016x}",
            gs.tap_resyncs
        )?;
        match (self.first, first) {
            (None, None) => writeln!(out, "  the same as the software GS's after every field")?,
            (first_field, word) => writeln!(
                out,
                "  differs from the software GS's: first after field {:?} ({:?} words), at the end {n} words, the first word {:?} (block {:?})",
                first_field.map(|f| f.0),
                first_field.map(|f| f.1),
                word,
                word.map(|w| w / 64),
            )?,
        }
        Ok(())
    }
}

/// `--states`: facts first (`#` keys), then primitives and pixels by
/// pipeline state, most pixels first.
#[allow(clippy::cast_precision_loss, reason = "a report")]
fn report_states(
    out: &mut impl io::Write,
    states: std::collections::HashMap<String, (u64, u64)>,
) -> Result<()> {
    let mut facts: Vec<_> = states
        .iter()
        .filter(|(k, _)| k.starts_with('#'))
        .map(|(k, v)| (k.clone(), v.0))
        .collect();
    facts.sort();
    for (fact, n) in facts {
        writeln!(out, "  {fact}: {n}")?;
    }
    let states: std::collections::HashMap<_, _> = states
        .into_iter()
        .filter(|(k, _)| !k.starts_with('#'))
        .collect();
    let total: u64 = states.values().map(|s| s.1).sum();
    let mut states: Vec<_> = states.into_iter().collect();
    states.sort_by_key(|(_, (_, pixels))| std::cmp::Reverse(*pixels));
    let mut running = 0;
    for (state, (prims, pixels)) in states.iter().take(30) {
        running += pixels;
        writeln!(
            out,
            "  {:5.1}% ({:5.1}% so far) {prims:8} prims  {state}",
            *pixels as f64 * 100.0 / total.max(1) as f64,
            running as f64 * 100.0 / total.max(1) as f64,
        )?;
    }
    writeln!(out, "  {} states, {total} pixels", states.len())?;
    Ok(())
}
