//! The conformance program (`conformance/`, our own PS2 test executable)
//! run in the interpreter: its output must equal `expected.txt`, and every
//! line for an integer, MMI or load/store operation must equal the same line
//! from PCSX2's recompiler or its interpreter. Float and VU lines are not
//! held to PCSX2, whose float results come from x86 SSE rather than the
//! PS2's rules (`docs/research/conformance.md`).

#![allow(clippy::unwrap_used, reason = "a test helper")]

use std::path::Path;

use ps2_analysis::Image;
use ps2_ee::{FIELD_CYCLES, Machine, Stop};

fn file(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn ours() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/conformance.elf");
    let image = Image::from_elf(&std::fs::read(path).unwrap()).unwrap();
    let mut machine = Machine::boot(&image, "host:conformance.elf").unwrap();
    // The program idles after "done": run a field at a time until then.
    for _ in 0..60 {
        let stop = machine.run(FIELD_CYCLES);
        assert!(
            matches!(stop, Stop::Budget | Stop::Exit(_)),
            "stopped: {stop:?}"
        );
        if machine.hw.borrow().console.ends_with(b"done\n") {
            return String::from_utf8(machine.hw.borrow().console.clone()).unwrap();
        }
    }
    panic!("the program did not print \"done\" within 60 fields");
}

/// Float and VU0 operations: `add.s`, `c.eq.s`, `vadd` and the like.
fn is_float(op: &str) -> bool {
    op.rsplit_once('.').is_some_and(|(_, format)| format == "s") || op.starts_with('v')
}

#[test]
fn conformance_program_matches_expected_and_pcsx2() {
    let ours = ours();
    let expected = file("expected.txt");
    let mismatches: Vec<_> = ours
        .lines()
        .zip(expected.lines())
        .filter(|(a, b)| a != b)
        .take(10)
        .collect();
    assert!(
        mismatches.is_empty(),
        "differs from expected.txt (ours, expected): {mismatches:#?}"
    );
    assert_eq!(
        ours.lines().count(),
        expected.lines().count(),
        "line counts differ"
    );

    let recompiler = file("pcsx2-recompiler.txt");
    let interpreter = file("pcsx2-interpreter.txt");
    assert_eq!(recompiler.lines().count(), expected.lines().count());
    assert_eq!(interpreter.lines().count(), expected.lines().count());
    let unmatched: Vec<_> = expected
        .lines()
        .zip(recompiler.lines().zip(interpreter.lines()))
        .filter(|(line, _)| !is_float(line.split(' ').next().unwrap_or("")))
        .filter(|(line, (r, i))| line != r && line != i)
        .take(10)
        .collect();
    assert!(
        unmatched.is_empty(),
        "integer lines PCSX2 does not give in either mode: {unmatched:#?}"
    );
}
