//! The IOP conformance module (`conformance/iop`): the EE program loads it
//! from `host:` through the loader's RPC, it runs on the IOP's processor and
//! emulated kernel, and what it prints must equal what it prints on PCSX2,
//! whose IOP runs Sony's own kernel (`pcsx2.txt`).

#![allow(clippy::unwrap_used, reason = "a test")]

use std::path::Path;

use ps2_analysis::Image;
use ps2_ee::{FIELD_CYCLES, Machine, Stop};
use ps2_iop::HostFiles;

#[test]
fn iop_conformance_matches_pcsx2() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/iop");
    let image = Image::from_elf(&std::fs::read(dir.join("iopload.elf")).unwrap()).unwrap();
    let mut machine = Machine::boot(&image, "host:iopload.elf").unwrap();
    machine.iop.attach_files(Box::new(HostFiles(dir.clone())));
    for _ in 0..120 {
        let stop = machine.run(FIELD_CYCLES);
        assert!(matches!(stop, Stop::Budget), "stopped: {stop:?}");
        if machine.iop.core.console.ends_with(b"iop done\n") {
            break;
        }
    }
    let ours = String::from_utf8_lossy(&machine.iop.core.console).into_owned();
    let expected = std::fs::read_to_string(dir.join("pcsx2.txt")).unwrap();
    assert_eq!(
        ours,
        expected,
        "EE console: {}",
        String::from_utf8_lossy(&machine.hw.borrow().console)
    );
}
