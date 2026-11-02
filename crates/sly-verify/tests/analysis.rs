//! Function discovery on the three games, run blind (no symbols), and for
//! Sly 1 compared with the decompilation's function starts.

#![allow(
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::similar_names,
    clippy::format_collect,
    reason = "a measurement report, read top to bottom"
)]

use std::collections::{BTreeMap, BTreeSet};

use ps2_analysis::{Anomaly, Cfg, discover, fingerprint, syscall_stubs};
use sly_common::{Game, symbols};
use sly_verify::Executable;

#[test]
#[ignore = "needs the user's discs: cargo xtask test-full"]
fn discovery_report() {
    for game in Game::ALL {
        let image = Executable::load(game).unwrap().image().unwrap();
        let text = image.text().unwrap();
        let found = discover(&image, &[]).unwrap();
        let mut origins: BTreeMap<String, usize> = BTreeMap::new();
        for f in &found.functions {
            *origins.entry(format!("{:?}", f.origin)).or_default() += 1;
        }
        let bytes =
            |ranges: &[std::ops::Range<u32>]| ranges.iter().map(|r| r.end - r.start).sum::<u32>();
        let in_functions: u32 = found.functions.iter().map(|f| f.end - f.start).sum();
        let mut anomalies: BTreeMap<&str, usize> = BTreeMap::new();
        for a in &found.anomalies {
            let kind = match a {
                Anomaly::BranchOutside { .. } => "branch outside",
                Anomaly::CutAtStart { .. } => "cut at start",
                Anomaly::InvalidInFunction { .. } => "invalid in function",
                Anomaly::UnresolvedJr { .. } => "unresolved jr",
                Anomaly::PointerInside { .. } => "pointer inside",
                Anomaly::MergedOrphan { .. } => "merged orphan",
            };
            *anomalies.entry(kind).or_default() += 1;
        }
        let unresolved: Vec<String> = found
            .anomalies
            .iter()
            .filter_map(|a| match a {
                Anomaly::UnresolvedJr { site } => Some(format!("{site:08x}")),
                _ => None,
            })
            .collect();
        println!(
            "{game}: unresolved jr sites {unresolved:?}; unbounded tables {:?}",
            found
                .jump_tables
                .iter()
                .filter(|t| !t.bounded)
                .map(|t| (format!("{:08x}", t.site), t.targets.len()))
                .collect::<Vec<_>>()
        );
        let stubs = syscall_stubs(&image, &found);
        let unnamed: Vec<String> = stubs
            .iter()
            .filter(|s| s.name.is_none())
            .map(|s| format!("{:#x}", s.number))
            .collect();
        println!("{game}: unnamed syscall numbers {unnamed:?}");
        // The function list, for comparing with other projects' lists.
        let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("analysis");
        std::fs::create_dir_all(&dir).unwrap();
        let listing: String = found
            .functions
            .iter()
            .map(|f| format!("{:08x}\t{:08x}\t{:?}\n", f.start, f.end, f.origin))
            .collect();
        std::fs::write(dir.join(format!("{game}-functions.tsv")), listing).unwrap();
        println!(
            "{game}: {} functions {origins:?}; .text {} B = functions {in_functions} + padding {} + data {} (+{} unaccounted); \
             {} jump tables ({} bounded); anomalies {anomalies:?}; {} syscall stubs ({} named)",
            found.functions.len(),
            text.end - text.start,
            bytes(&found.padding),
            bytes(&found.data),
            (text.end - text.start) - in_functions - bytes(&found.padding) - bytes(&found.data),
            found.jump_tables.len(),
            found.jump_tables.iter().filter(|t| t.bounded).count(),
            stubs.len(),
            stubs.iter().filter(|s| s.name.is_some()).count(),
        );
        // Gate: every byte of .text is a function, padding or recorded data,
        // and data is under 0.1%.
        assert_eq!(
            in_functions + bytes(&found.padding) + bytes(&found.data),
            text.end - text.start,
            "{game}"
        );
        assert!(
            bytes(&found.data) * 1000 < text.end - text.start,
            "{game}: too much data in .text"
        );
        if game == Game::Sly1 {
            let decomp: BTreeSet<u32> = symbols(game).iter().map(|s| s.address).collect();
            let ours: BTreeSet<u32> = found.functions.iter().map(|f| f.start).collect();
            let missing: Vec<u32> = decomp.difference(&ours).copied().collect();
            let extra = ours.difference(&decomp).count();
            println!(
                "sly1 vs decomp: {} decomp starts, {} found ({:.2}%), {} missing, {} extra starts of ours",
                decomp.len(),
                decomp.len() - missing.len(),
                100.0 * (decomp.len() - missing.len()) as f64 / decomp.len() as f64,
                missing.len(),
                extra
            );
            // Gate: at least 99% of the decompilation's starts, and every miss
            // explained. The known misses are switch case blocks the
            // decompilation's Ghidra pass split off as `FUN_` functions.
            let cases: BTreeSet<u32> = found
                .jump_tables
                .iter()
                .flat_map(|t| t.targets.iter().copied())
                .collect();
            let unexplained: Vec<String> = missing
                .iter()
                .filter(|a| !cases.contains(a))
                .map(|a| format!("{a:08x}"))
                .collect();
            assert!(
                missing.len() * 100 <= decomp.len(),
                "sly1: under 99% of the decompilation's starts"
            );
            assert!(
                unexplained.is_empty(),
                "sly1: unexplained misses {unexplained:?}"
            );
            let names: BTreeMap<u32, &str> = symbols(game)
                .iter()
                .map(|s| (s.address, s.readable))
                .collect();
            let mut extras: BTreeMap<String, usize> = BTreeMap::new();
            for f in found
                .functions
                .iter()
                .filter(|f| !decomp.contains(&f.start))
            {
                let before = image.word(f.start - 4).unwrap_or(1);
                let context = if before == 0 {
                    "after nop"
                } else {
                    "right after code"
                };
                *extras
                    .entry(format!(
                        "{:?}, {}-aligned, {context}",
                        f.origin,
                        if f.start % 8 == 0 { 8 } else { 4 }
                    ))
                    .or_default() += 1;
            }
            println!("  our extra starts: {extras:#?}");
            for address in missing.iter().take(40) {
                let inside = found.function_at(*address).map(|f| {
                    format!(
                        "inside ours {:08x}..{:08x} ({:?})",
                        f.start, f.end, f.origin
                    )
                });
                println!(
                    "  missing {address:08x} {} {}",
                    names[address],
                    inside.unwrap_or_else(|| "outside any function".into())
                );
            }
        }
    }
}

/// Sly 1's decompilation names carried to Sly 2 and 3 through identical
/// fingerprints (unique in both executables).
#[test]
#[ignore = "needs the user's discs: cargo xtask test-full"]
fn names_carry_over_by_fingerprint() {
    let index = |game: Game| {
        let image = Executable::load(game).unwrap().image().unwrap();
        let found = discover(&image, &[]).unwrap();
        let mut by_print: BTreeMap<u64, Vec<u32>> = BTreeMap::new();
        for f in &found.functions {
            if let Some(print) = fingerprint(&image, f) {
                by_print.entry(print).or_default().push(f.start);
            }
        }
        (found.functions.len(), by_print)
    };
    let names: BTreeMap<u32, &str> = symbols(Game::Sly1)
        .iter()
        .map(|s| (s.address, s.readable))
        .collect();
    let (_, sly1) = index(Game::Sly1);
    let named: BTreeMap<u64, u32> = sly1
        .iter()
        .filter(|(_, starts)| starts.len() == 1 && names.contains_key(&starts[0]))
        .map(|(print, starts)| (*print, starts[0]))
        .collect();
    for game in [Game::Sly2, Game::Sly3] {
        let (count, prints) = index(game);
        let matched: Vec<&str> = prints
            .iter()
            .filter(|(_, starts)| starts.len() == 1)
            .filter_map(|(print, _)| named.get(print).map(|start| names[start]))
            .collect();
        let sce = matched
            .iter()
            .filter(|n| n.starts_with("sce") || n.starts_with('_'))
            .count();
        println!(
            "{game}: {} of {count} functions get a Sly 1 name ({sce} sce*/_*): e.g. {:?}",
            matched.len(),
            &matched[..matched.len().min(12)]
        );
    }
}

/// Every discovered function has a control-flow graph whose edges stay
/// inside it.
#[test]
#[ignore = "needs the user's discs: cargo xtask test-full"]
fn control_flow_graphs_are_closed() {
    for game in Game::ALL {
        let image = Executable::load(game).unwrap().image().unwrap();
        let found = discover(&image, &[]).unwrap();
        let (mut blocks, mut delay_targets) = (0, 0);
        for f in &found.functions {
            let cfg = Cfg::build(&image, f, &found.jump_tables);
            blocks += cfg.blocks.len();
            delay_targets += cfg.targets_in_delay_slots.len();
            let starts: BTreeSet<u32> = cfg.blocks.iter().map(|b| b.start).collect();
            for block in &cfg.blocks {
                assert!(
                    block.start < block.end && block.end <= f.end,
                    "{game} {:08x}",
                    block.start
                );
                for edge in &block.edges {
                    assert!(
                        starts.contains(&edge.to),
                        "{game}: edge {:08x} -> {:08x} leaves the graph",
                        block.start,
                        edge.to
                    );
                }
            }
        }
        println!(
            "{game}: {} functions, {blocks} blocks, {delay_targets} branch targets in delay slots",
            found.functions.len()
        );
    }
}
