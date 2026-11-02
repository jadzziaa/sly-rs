//! `sly-rs disasm`: a listing of a game's executable with the functions
//! discovery finds and the known symbols.

use std::collections::BTreeMap;
use std::io::{self, Write};

use anyhow::{Context, Result};
use ps2_analysis::{Image, discover, syscall_stubs};
use ps2_disc::Disc;
use ps2_isa::ee::Insn;
use sly_common::{Build, Config, Game, identify, symbols};

/// `game`'s boot executable from its configured disc, loaded.
pub(crate) fn load(game: Game) -> Result<Image> {
    let config = Config::load()?;
    let path = config
        .disc(game)
        .with_context(|| format!("no disc configured for {game}"))?;
    let disc = Disc::open(path)?;
    let id = identify(&disc)?;
    anyhow::ensure!(
        id.build == Some(Build::of(game)),
        "not the supported {game} build"
    );
    Ok(Image::from_elf(&id.elf)?)
}

pub(crate) fn run(game: &str, address: &str, count: u32) -> Result<()> {
    let game = Game::from_key(game).with_context(|| format!("unknown game {game:?}"))?;
    let start = u32::from_str_radix(address.trim_start_matches("0x"), 16).context("address")?;
    let image = load(game)?;
    let found = discover(&image, &[])?;
    let mut names: BTreeMap<u32, String> = symbols(game)
        .into_iter()
        .map(|s| (s.address, s.readable.to_owned()))
        .collect();
    for stub in syscall_stubs(&image, &found) {
        names
            .entry(stub.address)
            .or_insert_with(|| format!("syscall {}", stub.name.unwrap_or("?")));
    }
    let starts: BTreeMap<u32, String> = found
        .functions
        .iter()
        .map(|f| (f.start, format!("{:?}, ends {:08x}", f.origin, f.end)))
        .collect();
    let mut out = io::stdout().lock();
    for pc in (start..).step_by(4).take(count as usize) {
        if let Some(how) = starts.get(&pc) {
            writeln!(
                out,
                "\n{pc:08x} <function: {how}> {}",
                names.get(&pc).map_or("", String::as_str)
            )?;
        } else if let Some(name) = names.get(&pc) {
            writeln!(out, "{pc:08x} <symbol {name} (not a start of ours)>")?;
        }
        let Some(word) = image.word(pc) else { break };
        writeln!(
            out,
            "  {pc:08x}: {word:08x}  {}",
            Insn::decode(word).display(pc)
        )?;
    }
    Ok(())
}
