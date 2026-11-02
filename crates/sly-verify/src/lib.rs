//! Checks of sly-rs's layers against independent references, on the user's
//! own discs. The checks are this crate's tests, ignored by default because
//! they need the discs; `cargo xtask test-full` runs them.
//!
//! The library half loads what the checks share: each configured game's
//! boot executable and its sections.

use std::path::PathBuf;

use anyhow::{Context, Result};
use object::{Object, ObjectSection};
use ps2_disc::Disc;
use sly_common::{Build, Config, Game, identify};

/// A section of a boot executable.
pub struct Section {
    /// Section name.
    pub name: String,
    /// Load address.
    pub address: u32,
    /// Contents.
    pub data: Vec<u8>,
}

/// A VU microcode overlay: code `dvp-ld` placed in `.vutext` for upload to
/// VU micro memory. The `.DVP.overlay.*` sections themselves hold only
/// zeros; `.DVP.ovlytab` gives each overlay's load address in `.vutext`.
pub struct Overlay {
    /// The overlay section's name (`.DVP.overlay..<vu address>.<id>.<seq>.<n>`).
    pub name: String,
    /// Byte address in VU micro memory.
    pub vu_address: u32,
    /// Address of the code in EE memory (inside `.vutext`).
    pub ee_address: u32,
    /// The microcode: little-endian 64-bit instruction pairs.
    pub code: Vec<u8>,
}

/// A game's boot executable, read from the configured disc.
pub struct Executable {
    /// Which game.
    pub game: Game,
    /// Its sections with contents, in file order.
    pub sections: Vec<Section>,
}

impl Executable {
    /// Reads `game`'s boot executable from its configured disc and checks it
    /// is the supported build.
    ///
    /// # Errors
    ///
    /// If no disc is configured for `game`, it cannot be read, or it is not
    /// the supported build.
    pub fn load(game: Game) -> Result<Self> {
        let config = Config::load()?;
        let path: PathBuf = config
            .disc(game)
            .with_context(|| {
                format!("no disc configured for {game} (sly-rs.toml or SLY_RS_DISC_*)")
            })?
            .to_path_buf();
        let disc = Disc::open(&path)?;
        let id = identify(&disc)?;
        anyhow::ensure!(
            id.build == Some(Build::of(game)),
            "{}: not the supported {game} build",
            path.display()
        );
        let file = object::File::parse(id.elf.as_slice()).context("boot executable")?;
        let sections = file
            .sections()
            .filter_map(|section| {
                let data = section.data().ok()?.to_vec();
                Some(Section {
                    name: section.name().ok()?.to_owned(),
                    address: u32::try_from(section.address()).ok()?,
                    data,
                })
            })
            .collect();
        Ok(Self { game, sections })
    }

    /// The section called `name`.
    #[must_use]
    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|section| section.name == name)
    }

    /// The VU microcode overlays listed in `.DVP.ovlytab`: 12-byte entries of
    /// (name offset in `.DVP.ovlystrtab`, EE load address, VU address).
    ///
    /// # Errors
    ///
    /// If a table, a named section or the code is missing or out of range.
    pub fn vu_overlays(&self) -> Result<Vec<Overlay>> {
        let table = self.section(".DVP.ovlytab").context(".DVP.ovlytab")?;
        let names = self.section(".DVP.ovlystrtab").context(".DVP.ovlystrtab")?;
        let vutext = self.section(".vutext").context(".vutext")?;
        let word = |bytes: &[u8], at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        table
            .data
            .as_chunks::<12>()
            .0
            .iter()
            .map(|entry| {
                let name_at = word(entry, 0) as usize;
                let name_end = names.data[name_at..]
                    .iter()
                    .position(|&b| b == 0)
                    .context("overlay name")?
                    + name_at;
                let name = String::from_utf8_lossy(&names.data[name_at..name_end]).into_owned();
                let (ee_address, vu_address) = (word(entry, 4), word(entry, 8));
                let size = self
                    .section(&name)
                    .with_context(|| format!("section {name}"))?
                    .data
                    .len();
                let start = ee_address
                    .checked_sub(vutext.address)
                    .context("overlay before .vutext")? as usize;
                let code = vutext
                    .data
                    .get(start..start + size)
                    .context("overlay beyond .vutext")?
                    .to_vec();
                Ok(Overlay {
                    name,
                    vu_address,
                    ee_address,
                    code,
                })
            })
            .collect()
    }

    /// The little-endian 32-bit words of a section with their addresses.
    pub fn words(section: &Section) -> impl Iterator<Item = (u32, u32)> + '_ {
        let words = section.data.as_chunks::<4>().0.iter();
        (section.address..)
            .step_by(4)
            .zip(words.map(|bytes| u32::from_le_bytes(*bytes)))
    }
}
