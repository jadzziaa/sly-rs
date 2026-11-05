//! Native replacements for game functions (plan §5.3, "Hooks"), keyed to the
//! supported build of each game. Generated code leaves these functions out,
//! so a compiled call always reaches the hook.
//!
//! Sly 1 (addresses and behaviour from the decompilation, CC0:
//! `src/P2/mpeg.c`): movies are skipped. `CMpeg::Execute` still runs, so the
//! game's own bookkeeping around a movie happens (its phase, the file, the
//! "intro seen" flag); only the decoding is replaced: `CMpeg::Start` sets up
//! nothing, `sceMpegIsEnd` reports the end at once, `CMpeg::Finish` has
//! nothing to tear down.

use ps2_core::{Cpu, Memory};
use ps2_ee::Hook;

use crate::Game;

fn nothing(_: &mut Cpu, _: &mut Memory) {}

fn movie_ended(cpu: &mut Cpu, _: &mut Memory) {
    cpu.set64(2, 1);
}

/// Lets the game play its movies: the hooks that skip them are taken away,
/// and its own player decodes them through the IPU.
pub fn play_movies(machine: &mut ps2_ee::Machine, game: Game) {
    for &(address, _, _) in hooks(game) {
        machine.unhook(address);
    }
}

/// A game's hooks: address, name, replacement.
#[must_use]
pub fn hooks(game: Game) -> &'static [(u32, &'static str, Hook)] {
    match game {
        Game::Sly1 => &[
            (0x0018_F448, "CMpeg::Start", nothing),
            (0x0020_B0B0, "sceMpegIsEnd", movie_ended),
            (0x0018_F7D0, "CMpeg::Finish", nothing),
        ],
        Game::Sly2 | Game::Sly3 => &[],
    }
}
