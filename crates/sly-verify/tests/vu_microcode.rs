//! Every instruction pair of the three games' VU microcode overlays decodes
//! to valid upper and lower instructions. The code is read where `dvp-ld`
//! put it (`.vutext`, located through `.DVP.ovlytab`); the overlay sections
//! themselves hold only zeros, which decode as valid pairs, so the test also
//! insists on real code: non-zero pairs and programs that end (E flag).

use ps2_isa::vu::{FLAG_E, Pair};
use sly_common::Game;
use sly_verify::Executable;

#[test]
#[ignore = "needs the user's discs: cargo xtask test-full"]
fn every_overlay_pair_is_valid() {
    let mut invalid = Vec::new();
    for game in Game::ALL {
        let exe = Executable::load(game).unwrap();
        // The .DVP.overlay sections are dvp-ld placeholders: all zero.
        for section in exe
            .sections
            .iter()
            .filter(|s| s.name.starts_with(".DVP.overlay"))
        {
            assert!(
                section.data.iter().all(|&b| b == 0),
                "{game} {}: not all zero",
                section.name
            );
        }
        let overlays = exe.vu_overlays().unwrap();
        let (mut pairs, mut zero, mut ends) = (0, 0, 0);
        for overlay in &overlays {
            assert_eq!(
                overlay.code.len() % 8,
                0,
                "{game} {}: not whole pairs",
                overlay.name
            );
            let pcs = (overlay.vu_address..).step_by(8);
            for (pc, bytes) in pcs.zip(overlay.code.as_chunks::<8>().0) {
                let value = u64::from_le_bytes(*bytes);
                let pair = Pair::decode(value);
                pairs += 1;
                zero += usize::from(value == 0);
                ends += usize::from(pair.upper() & FLAG_E != 0);
                if !pair.is_valid() {
                    invalid.push(format!(
                        "{game} {} at VU {pc:#06x}: {}",
                        overlay.name,
                        pair.display(pc)
                    ));
                }
            }
        }
        println!(
            "{game}: {} overlays, {pairs} pairs ({zero} all-zero), {ends} with the E flag",
            overlays.len()
        );
        assert!(
            zero * 10 < pairs,
            "{game}: mostly zero pairs: not real microcode"
        );
        assert!(ends > 0, "{game}: no program ends: not real microcode");
    }
    assert!(
        invalid.is_empty(),
        "{} invalid pairs:\n{}",
        invalid.len(),
        invalid.join("\n")
    );
}
