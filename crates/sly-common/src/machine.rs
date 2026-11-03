//! A game booted in the EE interpreter: its executable loaded from the
//! configured disc, the IOP side given the disc and the loader version.

use std::path::Path;
use std::rc::Rc;

use ps2_analysis::Image;
use ps2_disc::Disc;
use ps2_ee::Machine;

use crate::{Build, Config, Game, identify};

/// Errors from booting a game.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BootError {
    /// No disc is configured for the game.
    #[error("no disc configured for {0} (sly-rs.toml or SLY_RS_DISC_*)")]
    NoDisc(Game),
    /// The disc is not the supported build.
    #[error("{0}: not the supported build")]
    WrongBuild(String),
    /// The configuration could not be loaded.
    #[error(transparent)]
    Config(#[from] crate::ConfigError),
    /// The disc could not be read.
    #[error(transparent)]
    Disc(#[from] ps2_disc::DiscError),
    /// The executable could not be loaded.
    #[error(transparent)]
    Analysis(#[from] ps2_analysis::AnalysisError),
    /// A section does not fit in memory.
    #[error("section at {0:#x} does not fit in memory")]
    Layout(u32),
}

/// The game's disc as the IOP's CD/DVD servers read it.
struct DiscSectors(Rc<Disc>);

impl ps2_iop::cdvd::Sectors for DiscSectors {
    fn read(&self, lba: u32, count: u32) -> Option<Vec<u8>> {
        self.0.sectors(lba, count).ok()
    }
}

/// The game's disc as the IOP's module loader reads it: `cdrom0:\NAME;1`.
struct DiscFiles(Rc<Disc>);

impl ps2_iop::Files for DiscFiles {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let name = path
            .strip_prefix("cdrom0:")?
            .trim_start_matches(['\\', '/']);
        let name = name.split(';').next()?;
        let entry = self.0.find(name).ok()??;
        self.0.read_file(&entry).ok()
    }
}

/// `game`'s executable, from its configured disc.
///
/// # Errors
///
/// As [`boot`].
pub fn executable(game: Game) -> Result<(Image, Disc), BootError> {
    let config = Config::load()?;
    let path = config.disc(game).ok_or(BootError::NoDisc(game))?;
    executable_from(game, path)
}

fn executable_from(game: Game, path: &Path) -> Result<(Image, Disc), BootError> {
    let disc = Disc::open(path)?;
    let id = identify(&disc)?;
    if id.build != Some(Build::of(game)) {
        return Err(BootError::WrongBuild(path.display().to_string()));
    }
    Ok((Image::from_elf(&id.elf)?, disc))
}

/// Boots `game` from its configured disc, ready to run at the entry point.
///
/// # Errors
///
/// If no disc is configured, it cannot be read, it is not the supported
/// build, or its executable does not load.
pub fn boot(game: Game) -> Result<(Machine, Image), BootError> {
    let (image, disc) = executable(game)?;
    let argv0 = format!("cdrom0:\\{};1", Build::of(game).boot_elf);
    let mut machine = Machine::boot(&image, &argv0).map_err(BootError::Layout)?;
    let disc = Rc::new(disc);
    machine
        .iop
        .attach_disc(Rc::new(DiscSectors(Rc::clone(&disc))));
    machine.iop.attach_files(Box::new(DiscFiles(disc)));
    machine.iop.attach_loader(Build::of(game).iop_version);
    for &(address, name, hook) in crate::hooks::hooks(game) {
        machine.hook(address, name, hook);
    }
    Ok((machine, image))
}
