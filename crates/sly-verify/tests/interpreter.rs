//! The phase 3 gate: Sly 1 boots in the interpreter from its entry point
//! through C++ start-up, the IOP reboot, SIF and RPC set-up and IOP module
//! loading, to its first wait on a subsystem not built yet (the 989snd
//! sound driver, recompiled in phase 5), and two runs end in the same
//! state.

#![allow(clippy::unwrap_used, reason = "a test helper")]

use ps2_ee::{FIELD_CYCLES, Stop};
use sly_common::Game;

/// Five seconds of game time: well past the point where start-up settles.
const FIELDS: u64 = 300;

fn run() -> (u64, ps2_ee::Machine) {
    let (mut machine, _) = sly_common::machine::boot(Game::Sly1).unwrap();
    assert_eq!(machine.run(FIELDS * FIELD_CYCLES), Stop::Budget);
    (machine.state_hash(), machine)
}

#[test]
#[ignore = "needs the user's discs: cargo xtask test-full"]
fn sly1_boots_to_the_sound_driver_deterministically() {
    let (first, machine) = run();
    let (second, _) = run();
    assert_eq!(first, second, "two runs from the same start diverged");

    let calls = &machine.iop.calls;
    // The command layer, CD/DVD, the module loader and the pad were used...
    for (sid, function) in [
        (0x8000_0592, 0),
        (0x8000_0593, 3),
        (0x8000_0006, 0xFF),
        (0x8000_0006, 0),
        (0x8000_0100, 1),
    ] {
        assert!(
            calls.contains_key(&(sid, function)),
            "no call to {sid:#x} function {function:#x}: {calls:x?}"
        );
    }
    // ...and start-up waits on the sound driver, not yet built.
    assert!(
        calls.contains_key(&(0x0012_3456, 0)),
        "989snd was not reached: {calls:x?}"
    );
    // The kernel created the game's threads and semaphores.
    assert!(machine.kernel.threads.iter().flatten().count() >= 3);
    assert!(machine.kernel.semas.iter().flatten().count() >= 9);
}
