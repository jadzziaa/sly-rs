//! The recompiled conformance program against the interpreter: the same
//! output, and the same machine state at every checkpoint.

#![allow(clippy::unwrap_used, reason = "a test")]

use std::path::Path;

use ps2_analysis::Image;
use ps2_core::rec::{Exit, Table};
use ps2_ee::{FIELD_CYCLES, Machine, Stop};

fn boot(compiled: bool) -> Machine {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../conformance.elf");
    let image = Image::from_elf(&std::fs::read(path).unwrap()).unwrap();
    let mut machine = Machine::boot(&image, "host:conformance.elf").unwrap();
    if compiled {
        machine.attach(Table::new(conformance_rec::BLOCKS));
    }
    machine
}

fn done(machine: &Machine) -> bool {
    machine.hw.borrow().console.ends_with(b"done\n")
}

#[test]
fn recompiled_output_matches_expected() {
    let mut machine = boot(true);
    for _ in 0..60 {
        assert!(matches!(
            machine.run(FIELD_CYCLES),
            Stop::Budget | Stop::Exit(_)
        ));
        if done(&machine) {
            break;
        }
    }
    let expected =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../expected.txt"))
            .unwrap();
    let ours = String::from_utf8(machine.hw.borrow().console.clone()).unwrap();
    let first = ours.lines().zip(expected.lines()).find(|(a, b)| a != b);
    assert!(
        first.is_none(),
        "first difference (ours, expected): {first:?}"
    );
    assert_eq!(ours.lines().count(), expected.lines().count());

    // The phase 4 gate's measure: at most 0.1% of instructions interpreted.
    let stats = &machine.stats;
    eprintln!(
        "compiled {} interpreted {} system calls {}, exits {:?}",
        stats.compiled, stats.interpreted, stats.kernel, stats.exits
    );
    assert!(
        stats.interpreted * 1000 <= stats.compiled + stats.interpreted,
        "{} of {} instructions interpreted",
        stats.interpreted,
        stats.compiled + stats.interpreted
    );
    // flow.c recurses deeper than generated calls nest: the machine must
    // have taken over and started again from an empty host stack.
    assert!(stats.exits.contains_key(&Exit::Depth), "{:?}", stats.exits);
}

#[test]
fn recompiled_runs_in_lockstep_with_the_interpreter() {
    const STEP: u64 = 10_000;
    let mut compiled = boot(true);
    let mut interpreted = boot(false);
    while !done(&interpreted) {
        let a = compiled.run(STEP);
        let b = interpreted.run(STEP);
        assert_eq!(a, b);
        assert_eq!(
            compiled.cpu, interpreted.cpu,
            "registers differ by cycle {}",
            interpreted.cpu.cycles
        );
        assert!(
            compiled.mem.regions.ram == interpreted.mem.regions.ram,
            "RAM differs by cycle {}",
            interpreted.cpu.cycles
        );
        assert!(
            compiled.cpu.cycles < 60 * FIELD_CYCLES,
            "the program did not finish"
        );
    }
}
