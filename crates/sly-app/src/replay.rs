//! `replay`: runs a replay file and checks its checkpoints (world, level and
//! state hash), or records the hashes with `--record`. `--frames` saves what
//! the display shows every so many fields, `--beyond` runs on past the end
//! with the pad left alone.

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
}

/// Runs `fields` fields of `script`, saving pictures on the way.
fn advance(
    machine: &mut ps2_ee::Machine,
    fields: u64,
    script: &replay::Script,
    frames: &Frames<'_>,
) -> Result<ps2_ee::Stop> {
    let Some(dir) = frames.dir else {
        return Ok(replay::run(machine, fields, script));
    };
    for _ in 0..fields {
        let stop = replay::run(machine, 1, script);
        if stop != ps2_ee::Stop::Budget {
            return Ok(stop);
        }
        let field = machine.cpu.cycles / ps2_ee::FIELD_CYCLES;
        if field.is_multiple_of(frames.every.max(1)) {
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
        let stop = advance(&mut machine, end - done, &script, frames)?;
        let state = Sly1State::read(&mut machine.mem);
        writeln!(out, "field {end}: {state} ({stop:?})")?;
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
