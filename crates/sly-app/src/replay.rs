//! `replay`: runs a replay file and checks its checkpoints (world, level and
//! state hash), or records the hashes with `--record`.

use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use sly_common::Game;
use sly_common::replay::{self, Replay};
use sly_common::state::Sly1State;

use crate::Recompiled;

pub(crate) fn run(path: &Path, record: bool, recompiled: Option<&Recompiled>) -> Result<()> {
    let mut replay = Replay::load(path)?;
    let game =
        Game::from_key(&replay.game).with_context(|| format!("unknown game {:?}", replay.game))?;
    let presses = replay::parse_presses(&replay.presses)?;
    let (mut machine, _) = crate::boot(game, recompiled)?;
    let mut out = io::stdout().lock();
    let mut failures = 0;
    let mut done = 0;
    for checkpoint in &mut replay.checkpoints {
        let stop = replay::run(&mut machine, checkpoint.field - done, &presses);
        done = checkpoint.field;
        if stop != ps2_ee::Stop::Budget {
            bail!("stopped at field {done}: {stop:?}");
        }
        let state = Sly1State::read(&mut machine.mem);
        let hash = format!("{:016x}", machine.state_hash());
        let place_ok = state.world.0 == checkpoint.world && state.world.1 == checkpoint.level;
        let hash_ok = record || checkpoint.hash.as_ref().is_none_or(|h| *h == hash);
        writeln!(
            out,
            "field {done}: world {} level {} (expected {} {}), hash {hash}{}",
            state.world.0,
            state.world.1,
            checkpoint.world,
            checkpoint.level,
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
