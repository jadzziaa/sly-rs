//! The phase 5 gate: Sly 1 from power-on reaches its title, drawn over the
//! first level (`jb_intro`), and starts a new game there, under the scripted
//! replay `games/sly1/replays/new-game.toml`; two runs give the recorded
//! state hashes at every checkpoint. On the way, the game's
//! anticrack checks run and pass: its code in RAM is still the executable's.

#![allow(clippy::unwrap_used, reason = "a test")]

use std::path::Path;

use ps2_ee::Stop;
use sly_common::Game;
use sly_common::replay::{self, Replay};
use sly_common::state::Sly1State;

/// `g_anticrack_frm`: set when `check_anticrack_antigrab`'s CRC of the code
/// fails (decompilation, `config/symbol_addrs.txt`).
const ANTICRACK_FLAG: u32 = 0x0026_2310;
/// The checks that run while the game plays (`check_anticrack_antigrab`,
/// `check_anticrack_collectibles`).
const CHECKS: [u32; 2] = [0x0015_EDB8, 0x001E_9B30];

fn replay() -> Replay {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../games/sly1/replays/new-game.toml");
    Replay::load(&path).unwrap()
}

/// Runs the replay; returns the hashes at its checkpoints.
fn run(replay: &Replay, check_anticrack: bool) -> Vec<String> {
    let script = replay::Script::of(replay).unwrap();
    let (mut machine, image) = sly_common::machine::boot(Game::Sly1).unwrap();
    if check_anticrack {
        for check in CHECKS {
            machine.watches.insert(check, Vec::new());
        }
    }
    let mut hashes = Vec::new();
    let mut done = 0;
    for checkpoint in &replay.checkpoints {
        let stop = replay::run(&mut machine, checkpoint.field - done, &script);
        assert_eq!(
            stop,
            Stop::Budget,
            "stopped before field {}",
            checkpoint.field
        );
        done = checkpoint.field;
        let state = Sly1State::read(&mut machine.mem);
        assert_eq!(
            (state.world.0, state.world.1),
            (checkpoint.world, checkpoint.level),
            "world and level at field {done}"
        );
        if let Some(ui) = checkpoint.interface {
            assert_eq!(state.ui, ui, "interface state at field {done}");
        }
        hashes.push(format!("{:016x}", machine.state_hash()));
    }
    if check_anticrack {
        for check in CHECKS {
            assert!(
                !machine.watches[&check].is_empty(),
                "the check at {check:#x} never ran"
            );
        }
        assert_eq!(
            machine.mem.read32(ANTICRACK_FLAG),
            0,
            "the anticrack CRC failed"
        );
        let text = image.text().unwrap();
        let code = image.bytes(text.clone()).unwrap();
        let at = text.start as usize;
        assert!(
            machine.mem.regions.ram[at..at + code.len()] == *code,
            "the code in RAM changed"
        );
    }
    hashes
}

#[test]
#[ignore = "needs the user's discs: cargo xtask test-full"]
fn sly1_new_game_reaches_the_first_level_deterministically() {
    let replay = replay();
    let recorded: Vec<String> = replay
        .checkpoints
        .iter()
        .map(|c| c.hash.clone().unwrap_or_default())
        .collect();
    // The two runs are independent machines: run them side by side.
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| run(&replay, true));
        let second = scope.spawn(|| run(&replay, false));
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_eq!(first, second, "two runs of the replay diverged");
    assert_eq!(
        first, recorded,
        "the state differs from the recorded checkpoints"
    );
}
