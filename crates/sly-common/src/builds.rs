//! The builds sly-rs supports, and identifying a disc.
//!
//! Every per-game address in sly-rs is keyed to one exact build, so a disc
//! is accepted only when its boot executable's SHA-1 is in [`BUILDS`]. The
//! image size is checked too; the whole-image SHA-1 is recorded for an
//! optional slow check (`sly-rs info --verify`).

use std::fmt;

use ps2_disc::{Disc, DiscError, ReadAt, Sha1Digest};

/// One of the three games.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Game {
    /// Sly Cooper and the Thievius Raccoonus (2002).
    Sly1,
    /// Sly 2: Band of Thieves (2004).
    Sly2,
    /// Sly 3: Honor Among Thieves (2005).
    Sly3,
}

impl Game {
    /// All three, in release order.
    pub const ALL: [Self; 3] = [Self::Sly1, Self::Sly2, Self::Sly3];

    /// Short key used in configuration and on the command line.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Sly1 => "sly1",
            Self::Sly2 => "sly2",
            Self::Sly3 => "sly3",
        }
    }

    /// The game for a [`Game::key`].
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|game| game.key().eq_ignore_ascii_case(key))
    }
}

impl fmt::Display for Game {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

/// A supported build of one game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Build {
    /// Which game.
    pub game: Game,
    /// Title as on the disc's label.
    pub title: &'static str,
    /// Product serial.
    pub serial: &'static str,
    /// Boot executable in the root directory.
    pub boot_elf: &'static str,
    /// SHA-1 of the boot executable.
    pub elf_sha1: Sha1Digest,
    /// Size of the disc image in bytes.
    pub image_size: u64,
    /// SHA-1 of the whole disc image.
    pub image_sha1: Sha1Digest,
}

/// The supported builds: the USA 1.00 releases, measured from the user's
/// discs on 2026-11-02 (docs/research/disc-survey.md). The executables'
/// hashes agree with the Sly 1 decompilation's target and community records.
pub const BUILDS: [Build; 3] = [
    Build {
        game: Game::Sly1,
        title: "Sly Cooper and the Thievius Raccoonus (USA)",
        serial: "SCUS-97198",
        boot_elf: "SCUS_971.98",
        elf_sha1: Sha1Digest::from_hex("57dc305db57932ad3f1122966cdb695d2e62a47a"),
        image_size: 2_802_319_360,
        image_sha1: Sha1Digest::from_hex("347b54b4a0d8b2c2e6c18cdbde5f1cabcc45b5da"),
    },
    Build {
        game: Game::Sly2,
        title: "Sly 2: Band of Thieves (USA)",
        serial: "SCUS-97316",
        boot_elf: "SCUS_973.16",
        elf_sha1: Sha1Digest::from_hex("de2af92a54de71e33edc8a160c31c0337a95e6f5"),
        image_size: 2_884_141_056,
        image_sha1: Sha1Digest::from_hex("7241cd566c9ee94ae1ba294ea31ed8f63fd56999"),
    },
    Build {
        game: Game::Sly3,
        title: "Sly 3: Honor Among Thieves (USA)",
        serial: "SCUS-97464",
        boot_elf: "SCUS_974.64",
        elf_sha1: Sha1Digest::from_hex("5941b7877e6bf5dfb8acb2d04a3c5059367f385b"),
        image_size: 3_553_755_136,
        image_sha1: Sha1Digest::from_hex("9f89eba20011bda2ea8e839c96b9100cf7762fc6"),
    },
];

impl Build {
    /// The supported build of `game`.
    #[must_use]
    pub fn of(game: Game) -> &'static Self {
        match game {
            Game::Sly1 => &BUILDS[0],
            Game::Sly2 => &BUILDS[1],
            Game::Sly3 => &BUILDS[2],
        }
    }
}

/// What a disc turned out to be.
#[derive(Clone, Debug)]
pub struct Identification {
    /// The supported build it matches, if any.
    pub build: Option<&'static Build>,
    /// The boot executable named by `SYSTEM.CNF`.
    pub boot_elf: String,
    /// SHA-1 of the boot executable.
    pub elf_sha1: Sha1Digest,
    /// The boot executable's bytes.
    pub elf: Vec<u8>,
    /// Whether the image size matches the build's (`false` without a build).
    pub size_matches: bool,
}

/// Identifies a disc by its boot executable: reads `SYSTEM.CNF`, hashes the
/// executable it names and looks the hash up in [`BUILDS`].
///
/// # Errors
///
/// Any [`DiscError`] from reading the file system, `SYSTEM.CNF` or the
/// executable.
pub fn identify<R: ReadAt>(disc: &Disc<R>) -> Result<Identification, DiscError> {
    let boot_elf = disc.system_cnf()?.boot_file().to_owned();
    let entry = disc
        .find(&boot_elf)?
        .ok_or_else(|| DiscError::NotFound(boot_elf.clone()))?;
    let elf = disc.read_file(&entry)?;
    let elf_sha1 = Sha1Digest::of(&elf);
    let build = BUILDS.iter().find(|build| build.elf_sha1 == elf_sha1);
    Ok(Identification {
        build,
        size_matches: build.is_some_and(|build| build.image_size == disc.len()),
        boot_elf,
        elf_sha1,
        elf,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        for game in Game::ALL {
            assert_eq!(Game::from_key(game.key()), Some(game));
            assert_eq!(Build::of(game).game, game);
        }
        assert_eq!(Game::from_key("SLY2"), Some(Game::Sly2));
        assert_eq!(Game::from_key("sly4"), None);
    }

    #[test]
    fn builds_are_distinct() {
        for (i, a) in BUILDS.iter().enumerate() {
            for b in &BUILDS[i + 1..] {
                assert_ne!(a.elf_sha1, b.elf_sha1);
                assert_ne!(a.image_sha1, b.image_sha1);
            }
        }
    }
}
