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
//!
//! Patches change instructions for an option, in the interpreter only:
//! their functions stay out of generated code, so both builds run them the
//! same way, and memory keeps the original bytes (Sly 1 checksums its own
//! code). Widescreen in Sly 1 widens the camera's aspect ratio, which only
//! `InitCm` sets (`CM::rAspect`, 1.298701, read by `RecalcCmFrustrum` and
//! the look-at code), by 4/3: 1.731601. The community's patch (`pcsx2_patches`,
//! SCUS-97198) changes the same instruction's upper half only, to 1.728389.

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

/// A patch: the function it is in, then each instruction's address, the
/// word the game has there and the word run instead.
pub struct Patch {
    /// The function's address (left out of generated code).
    pub function: u32,
    /// Address, original word, replacement.
    pub words: &'static [(u32, u32, u32)],
}

/// A game's widescreen patch (16:9), if it has one.
#[must_use]
pub fn widescreen(game: Game) -> Option<&'static Patch> {
    match game {
        // InitCm: `lui $at, 0x3FA6; ori $at, $at, 0x3BD8` (1.298701), then
        // stored at CM + 0x1E0.
        Game::Sly1 => Some(&Patch {
            function: 0x0014_3648,
            words: &[
                (0x0014_36B0, 0x3C01_3FA6, 0x3C01_3FDD),
                (0x0014_36B4, 0x3421_3BD8, 0x3421_A520),
            ],
        }),
        Game::Sly2 | Game::Sly3 => None,
    }
}

/// Functions generated code leaves out: the hooked ones, and those a patch
/// changes.
#[must_use]
pub fn left_out(game: Game) -> Vec<u32> {
    hooks(game)
        .iter()
        .map(|h| h.0)
        .chain(widescreen(game).map(|p| p.function))
        .collect()
}

/// Applies a patch, if the game has the words it expects there (as it
/// does on the supported build); returns whether it did.
pub fn apply(machine: &mut ps2_ee::Machine, patch: &Patch) -> bool {
    if patch
        .words
        .iter()
        .any(|&(address, original, _)| machine.mem.read32(address) != original)
    {
        return false;
    }
    for &(address, _, word) in patch.words {
        machine.patch(address, word);
    }
    true
}

/// Makes the game draw widescreen (16:9), if it can; returns whether it
/// does.
pub fn use_widescreen(machine: &mut ps2_ee::Machine, game: Game) -> bool {
    widescreen(game).is_some_and(|patch| apply(machine, patch))
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
