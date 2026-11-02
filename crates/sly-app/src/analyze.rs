//! `sly-rs analyze`: what phase 2's analysis finds in each executable.

use std::collections::BTreeMap;
use std::io::{self, Write};

use anyhow::{Context, Result};
use ps2_analysis::{Anomaly, discover, syscall_stubs, vu_overlays};
use sly_common::{Game, symbols};

use crate::disasm::load;

pub(crate) fn run(games: &[String], list_functions: bool) -> Result<()> {
    let games: Vec<Game> = if games.is_empty() {
        Game::ALL.to_vec()
    } else {
        games
            .iter()
            .map(|g| Game::from_key(g).with_context(|| format!("unknown game {g:?}")))
            .collect::<Result<_>>()?
    };
    let mut out = io::stdout().lock();
    for game in games {
        let image = load(game)?;
        let text = image.text()?;
        let found = discover(&image, &[])?;
        let size =
            |ranges: &[std::ops::Range<u32>]| ranges.iter().map(|r| r.end - r.start).sum::<u32>();
        let mut origins: BTreeMap<String, usize> = BTreeMap::new();
        for f in &found.functions {
            *origins
                .entry(format!("{:?}", f.origin).to_lowercase())
                .or_default() += 1;
        }
        let mut anomalies: BTreeMap<&str, usize> = BTreeMap::new();
        for a in &found.anomalies {
            *anomalies.entry(label(a)).or_default() += 1;
        }
        let stubs = syscall_stubs(&image, &found);
        writeln!(out, "{game}")?;
        writeln!(
            out,
            "  .text      {:#010x}..{:#010x}, {} bytes",
            text.start,
            text.end,
            text.end - text.start
        )?;
        writeln!(
            out,
            "  functions  {} ({}), {} bytes; padding {} bytes; data {} bytes",
            found.functions.len(),
            origins
                .iter()
                .map(|(k, v)| format!("{v} {k}"))
                .collect::<Vec<_>>()
                .join(", "),
            found.functions.iter().map(|f| f.end - f.start).sum::<u32>(),
            size(&found.padding),
            size(&found.data)
        )?;
        writeln!(
            out,
            "  switches   {} jump tables ({} bounded by a range check)",
            found.jump_tables.len(),
            found.jump_tables.iter().filter(|t| t.bounded).count()
        )?;
        writeln!(
            out,
            "  anomalies  {}",
            if anomalies.is_empty() {
                "none".into()
            } else {
                format!("{anomalies:?}")
            }
        )?;
        writeln!(
            out,
            "  kernel     {} syscall stubs ({} named)",
            stubs.len(),
            stubs.iter().filter(|s| s.name.is_some()).count()
        )?;
        let overlays = vu_overlays(&image)?;
        writeln!(
            out,
            "  VU         {} microcode overlays, {} instruction pairs",
            overlays.len(),
            overlays.iter().map(|o| o.code.len() / 8).sum::<usize>()
        )?;
        if list_functions {
            let names: BTreeMap<u32, &str> = symbols(game)
                .iter()
                .map(|s| (s.address, s.readable))
                .collect();
            for f in &found.functions {
                let name = names.get(&f.start).copied().unwrap_or("");
                writeln!(
                    out,
                    "    {:08x} {:08x} {:<8} {name}",
                    f.start,
                    f.end,
                    format!("{:?}", f.origin).to_lowercase()
                )?;
            }
        }
        writeln!(out)?;
    }
    Ok(())
}

fn label(anomaly: &Anomaly) -> &'static str {
    match anomaly {
        Anomaly::BranchOutside { .. } => "branch outside its function",
        Anomaly::CutAtStart { .. } => "function cut at a call target",
        Anomaly::InvalidInFunction { .. } => "invalid word in a function",
        Anomaly::UnresolvedJr { .. } => "jr without a recovered table",
        Anomaly::PointerInside { .. } => "code pointer inside a function",
        Anomaly::MergedOrphan { .. } => "orphan blocks merged",
    }
}
