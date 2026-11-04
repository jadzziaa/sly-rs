//! `replay`: runs a replay file and checks its checkpoints (world, level and
//! state hash), or records the hashes with `--record`. `--frames` saves what
//! the display shows every so many fields, `--beyond` runs on past the end
//! with the pad left alone, `--gs-dump` writes the last fields' GS traffic.

use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use sly_common::Game;
use sly_common::replay::{self, Replay};
use sly_common::state::Sly1State;

use crate::Recompiled;

/// Pictures to save while a replay runs.
pub(crate) struct Frames<'a> {
    /// The directory (`fieldNNNNNN.png`).
    pub(crate) dir: Option<&'a Path>,
    /// Every this many fields.
    pub(crate) every: u64,
    /// Fields to run past the replay's end.
    pub(crate) beyond: u64,
    /// A GS dump of the last fields: the file and how many.
    pub(crate) gs_dump: Option<(&'a Path, u64)>,
    /// Print how fast every `every` fields ran, and the game's clock.
    pub(crate) timing: bool,
}

/// Runs `fields` fields of `script`, saving pictures on the way.
#[allow(clippy::cast_precision_loss, reason = "a report")]
fn advance(
    machine: &mut ps2_ee::Machine,
    fields: u64,
    script: &replay::Script,
    frames: &Frames<'_>,
) -> Result<ps2_ee::Stop> {
    if frames.dir.is_none() && !frames.timing {
        return Ok(replay::run(machine, fields, script));
    }
    let mut started = std::time::Instant::now();
    // Host time so far: the machine waiting for the GS, the GS thread
    // busy, VIF1 with VU1, its waits for the GS, VU1's programs, VU calls
    // from the EE, the IOP. Reading the GS waits for its thread.
    let times = |machine: &ps2_ee::Machine| {
        let mut hw = machine.hw.borrow_mut();
        let busy = hw.gs.with(|gs| gs.busy);
        let t = &hw.timing;
        [
            hw.gs.waited,
            busy,
            t.vif1,
            t.vif1_gs_wait,
            hw.vif1.run_time,
            t.vu_calls,
            t.iop,
        ]
    };
    let mut before = times(machine);
    for _ in 0..fields {
        let stop = replay::run(machine, 1, script);
        if stop != ps2_ee::Stop::Budget {
            return Ok(stop);
        }
        let field = machine.cpu.cycles / ps2_ee::FIELD_CYCLES;
        let every = frames.every.max(1);
        if frames.timing && field.is_multiple_of(every) {
            // The GS's thread included: `times` waits for it.
            let now = times(machine);
            let took = started.elapsed().as_secs_f64();
            started = std::time::Instant::now();
            let d: Vec<f64> = now
                .iter()
                .zip(&before)
                .map(|(n, b)| n.saturating_sub(*b).as_secs_f64() / took * 100.0)
                .collect();
            before = now;
            let (waited, busy, vif1, vif1_wait, programs, calls, iop) =
                (d[0], d[1], d[2], d[3], d[4], d[5], d[6]);
            let ee = 100.0 - vif1 - calls - iop - (waited - vif1_wait);
            let state = Sly1State::read(&mut machine.mem);
            let (real, at) = (every as f64 / 59.94, field as f64 / 59.94);
            println!(
                "fields {}-{field}: {:.1} ms a field, {:.0}% of NTSC, waiting for the GS {waited:.0}%, GS thread busy {busy:.0}%; VU1 programs {:.0}%, the rest of VIF1 {:.0}%, VU calls {calls:.0}%, IOP {iop:.0}%, the EE and the rest {ee:.0}%; clock {:.2} s at {at:.2} s of fields; world {} level {}",
                field - every,
                took * 1000.0 / every as f64,
                real * 100.0 / took,
                programs - vif1_wait,
                vif1 - programs,
                state.clock(),
                state.world.0,
                state.world.1,
            );
        }
        if let Some(dir) = frames.dir
            && field.is_multiple_of(frames.every.max(1))
        {
            let mut hw = machine.hw.borrow_mut();
            let regs = hw.gs_display();
            let frame = hw.gs.with(move |gs| gs.display(&regs));
            let png = dir.join(format!("field{field:06}.png"));
            std::fs::write(&png, frame.png())
                .with_context(|| format!("writing {}", png.display()))?;
        }
    }
    Ok(ps2_ee::Stop::Budget)
}

pub(crate) fn run(
    path: &Path,
    record: bool,
    frames: &Frames<'_>,
    recompiled: Option<&Recompiled>,
) -> Result<()> {
    let mut replay = Replay::load(path)?;
    let game =
        Game::from_key(&replay.game).with_context(|| format!("unknown game {:?}", replay.game))?;
    let script = replay::Script::of(&replay)?;
    let (mut machine, _) = crate::boot(game, recompiled)?;
    if let Some(dir) = frames.dir {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut out = io::stdout().lock();
    let started = std::time::Instant::now();
    let mut failures = 0;
    let mut done = 0;
    for checkpoint in &mut replay.checkpoints {
        let stop = advance(&mut machine, checkpoint.field - done, &script, frames)?;
        done = checkpoint.field;
        if stop != ps2_ee::Stop::Budget {
            bail!("stopped at field {done}: {stop:?}");
        }
        let state = Sly1State::read(&mut machine.mem);
        let hash = format!("{:016x}", machine.state_hash());
        let place_ok = state.world.0 == checkpoint.world
            && state.world.1 == checkpoint.level
            && checkpoint.interface.is_none_or(|ui| ui == state.ui);
        let hash_ok = record || checkpoint.hash.as_ref().is_none_or(|h| *h == hash);
        writeln!(
            out,
            "field {done}: world {} level {} interface {} (expected {} {} {:?}), hash {hash}{}",
            state.world.0,
            state.world.1,
            state.ui,
            checkpoint.world,
            checkpoint.level,
            checkpoint.interface,
            if hash_ok {
                ""
            } else {
                " (differs from the recorded one)"
            }
        )?;
        failures += usize::from(!place_ok || !hash_ok);
        if record {
            checkpoint.hash = Some(hash);
        }
    }
    let end = replay.fields.max(done) + frames.beyond;
    if end > done {
        let mut left = end - done;
        if let Some((_, dumped)) = frames.gs_dump {
            let before = left.saturating_sub(dumped);
            if advance(&mut machine, before, &script, frames)? != ps2_ee::Stop::Budget {
                bail!("stopped before the GS dump");
            }
            left -= before;
            let mut hw = machine.hw.borrow_mut();
            let block = hw.gs_privileged();
            hw.gs.start_dump(block);
        }
        let stop = advance(&mut machine, left, &script, frames)?;
        if let Some((path, _)) = frames.gs_dump {
            let bytes = machine.hw.borrow_mut().gs.finish_dump().unwrap_or_default();
            std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
        }
        let state = Sly1State::read(&mut machine.mem);
        writeln!(out, "field {end}: {state} ({stop:?})")?;
    }
    let machine_time = started.elapsed();
    {
        let mut hw = machine.hw.borrow_mut();
        let draw = hw.gs.with(|gs| (gs.draw_time, gs.busy));
        let elapsed = started.elapsed();
        crate::run::host_time(&mut out, &hw, draw, machine_time, elapsed)?;
    }
    let stats = &machine.stats;
    writeln!(
        out,
        "{} instructions recompiled, {} interpreted, {} system calls; hooks {:?}",
        stats.compiled, stats.interpreted, stats.kernel, stats.hooks
    )?;
    if record {
        replay.save(path)?;
        writeln!(out, "hashes recorded in {}", path.display())?;
    }
    if failures > 0 {
        bail!("{failures} checkpoints failed");
    }
    Ok(())
}
