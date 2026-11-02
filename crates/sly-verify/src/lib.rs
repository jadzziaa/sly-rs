//! Checks of sly-rs's layers against independent references, on the user's
//! own discs. The checks are this crate's tests, ignored by default because
//! they need the discs; `cargo xtask test-full` runs them.
//!
//! The library half loads what the checks share: each configured game's
//! boot executable and its sections.

use std::path::PathBuf;

use anyhow::{Context, Result};
use object::{Object, ObjectSection};
use ps2_analysis::Image;
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

/// A game's boot executable, read from the configured disc.
pub struct Executable {
    /// Which game.
    pub game: Game,
    /// Its sections with contents, in file order.
    pub sections: Vec<Section>,
    /// The ELF file.
    pub elf: Vec<u8>,
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
        Ok(Self {
            game,
            sections,
            elf: id.elf,
        })
    }

    /// The section called `name`.
    #[must_use]
    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|section| section.name == name)
    }

    /// The executable loaded as the EE sees it.
    ///
    /// # Errors
    ///
    /// If the ELF cannot be loaded.
    pub fn image(&self) -> Result<Image> {
        Ok(Image::from_elf(&self.elf)?)
    }

    /// The little-endian 32-bit words of a section with their addresses.
    pub fn words(section: &Section) -> impl Iterator<Item = (u32, u32)> + '_ {
        let words = section.data.as_chunks::<4>().0.iter();
        (section.address..)
            .step_by(4)
            .zip(words.map(|bytes| u32::from_le_bytes(*bytes)))
    }
}
