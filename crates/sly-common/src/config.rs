//! Where the user keeps their discs and memory cards, and how `play`
//! behaves.
//!
//! The configuration is a TOML file, `sly-rs.toml`:
//!
//! ```toml
//! [discs]
//! sly1 = "oracle/Sly Cooper and the Thievius Raccoonus (USA)/Sly Cooper and the Thievius Raccoonus (USA).iso"
//!
//! [cards]              # memory card images `play` saves to (default:
//! sly1 = "cards/sly1.ps2"  #   cards/GAME.ps2 beside this file)
//!
//! [play]
//! pause_on_focus_loss = true
//! window = [1280, 896]
//! [play.keys]          # DualShock button or stick direction = key
//! cross = "KeyK"
//! left-up = "KeyW"
//! ```
//!
//! Relative paths are resolved against the file's directory. The file is
//! found at `$SLY_RS_CONFIG`, else as `sly-rs.toml` in the current directory
//! or any parent, else in the user's configuration directory
//! (`$XDG_CONFIG_HOME/sly-rs/` or `~/.config/sly-rs/`; `%APPDATA%\sly-rs\` on
//! Windows). `SLY_RS_DISC_SLY1`, `_SLY2` and `_SLY3` override single entries.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::Game;

/// File name searched for.
pub const FILE_NAME: &str = "sly-rs.toml";

/// Errors from loading the configuration.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Read {
        /// Path of the file.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// The file is not valid configuration.
    #[error("{}: {source}", path.display())]
    Parse {
        /// Path of the file.
        path: PathBuf,
        /// The underlying error.
        source: Box<toml::de::Error>,
    },
}

/// The loaded configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    path: Option<PathBuf>,
    /// The directory relative paths are resolved against.
    base: PathBuf,
    discs: BTreeMap<Game, PathBuf>,
    cards: BTreeMap<Game, PathBuf>,
    play: Play,
}

/// How `play` behaves.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Play {
    /// Pause the game while the window is not focused.
    pub pause_on_focus_loss: bool,
    /// The window's size at start, in logical pixels.
    pub window: [u32; 2],
    /// Key bindings that replace the defaults: a DualShock button
    /// (`cross`, `start`, `l1`, ... as in a replay's presses) or a stick
    /// direction (`left-up`, `right-left`, ...) to a key's name as winit
    /// gives it (`KeyW`, `Space`, `ArrowUp`, `Digit1`, `ShiftLeft`, ...).
    pub keys: BTreeMap<String, String>,
    /// Gamepad buttons that replace the defaults: a gamepad button, by
    /// where gilrs places it (`south`, `east`, `north`, `west`, `l1`, `r1`,
    /// `l2`, `r2`, `l3`, `r3`, `select`, `start`, `up`, `down`, `left`,
    /// `right`), to the DualShock button it gives (`cross`, `square`, ...
    /// as in a replay's presses).
    pub gamepad: BTreeMap<String, String>,
    /// Draw on the GPU (the GPU GS) instead of the CPU.
    pub gpu: bool,
    /// When drawing on the GPU, draw at this many times the GS's
    /// resolution (1 to 4).
    pub scale: u32,
    /// Sound volume, in percent (0 is silent: no sound device is opened).
    pub volume: u32,
}

impl Default for Play {
    fn default() -> Self {
        Self {
            pause_on_focus_loss: true,
            window: [1280, 896],
            keys: BTreeMap::new(),
            gamepad: BTreeMap::new(),
            gpu: false,
            scale: 1,
            volume: 100,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileFormat {
    #[serde(default)]
    discs: DiscsFormat,
    #[serde(default)]
    cards: DiscsFormat,
    #[serde(default)]
    play: Play,
}

/// A path per game (`[discs]`, `[cards]`).
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscsFormat {
    sly1: Option<PathBuf>,
    sly2: Option<PathBuf>,
    sly3: Option<PathBuf>,
}

impl Config {
    /// Finds and loads the configuration as described in the module
    /// documentation, then applies the environment overrides. No file is not
    /// an error: the result is then empty apart from the overrides.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] if a file is found but cannot be read or parsed.
    pub fn load() -> Result<Self, ConfigError> {
        let mut config = match find_file() {
            Some(path) => Self::from_file(&path)?,
            None => Self::default(),
        };
        config.apply_env(|key| std::env::var_os(key));
        Ok(config)
    }

    /// Loads the file at `path`, without environment overrides.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] if the file cannot be read or parsed.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let base = path.parent().unwrap_or(Path::new("."));
        let mut config = Self::parse(&text, base).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        config.path = Some(path.to_path_buf());
        Ok(config)
    }

    /// Parses configuration text, resolving relative paths against `base`.
    ///
    /// # Errors
    ///
    /// The TOML error if the text is not valid configuration.
    pub fn parse(text: &str, base: &Path) -> Result<Self, toml::de::Error> {
        let file: FileFormat = toml::from_str(text)?;
        let paths = |format: DiscsFormat| {
            [
                (Game::Sly1, format.sly1),
                (Game::Sly2, format.sly2),
                (Game::Sly3, format.sly3),
            ]
            .into_iter()
            .filter_map(|(game, path)| Some((game, base.join(path?))))
            .collect()
        };
        Ok(Self {
            path: None,
            base: base.to_path_buf(),
            discs: paths(file.discs),
            cards: paths(file.cards),
            play: file.play,
        })
    }

    /// Applies `SLY_RS_DISC_<GAME>` overrides read through `var`.
    pub fn apply_env(&mut self, var: impl Fn(&str) -> Option<OsString>) {
        for game in Game::ALL {
            let key = format!("SLY_RS_DISC_{}", game.key().to_ascii_uppercase());
            if let Some(path) = var(&key).filter(|path| !path.is_empty()) {
                self.discs.insert(game, PathBuf::from(path));
            }
        }
    }

    /// The configured disc image of `game`.
    #[must_use]
    pub fn disc(&self, game: Game) -> Option<&Path> {
        self.discs.get(&game).map(PathBuf::as_path)
    }

    /// The memory card image `play` uses for `game`: as configured, else
    /// `cards/GAME.ps2` beside the configuration file (or in the current
    /// directory, without one).
    #[must_use]
    pub fn card(&self, game: Game) -> PathBuf {
        self.cards
            .get(&game)
            .cloned()
            .unwrap_or_else(|| self.base.join("cards").join(format!("{}.ps2", game.key())))
    }

    /// How `play` behaves.
    #[must_use]
    pub const fn play(&self) -> &Play {
        &self.play
    }

    /// The file the configuration was loaded from, if any.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// The configuration file to load, by the search order of the module docs.
fn find_file() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SLY_RS_CONFIG").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let cwd = std::env::current_dir().ok()?;
    if let Some(found) = cwd
        .ancestors()
        .map(|dir| dir.join(FILE_NAME))
        .find(|path| path.is_file())
    {
        return Some(found);
    }
    user_config_dir()
        .map(|dir| dir.join("sly-rs").join(FILE_NAME))
        .filter(|path| path.is_file())
}

fn user_config_dir() -> Option<PathBuf> {
    let var = |key| {
        std::env::var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(windows) {
        var("APPDATA")
    } else {
        var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|home| home.join(".config")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_and_absolute_paths() {
        let text = "[discs]\nsly1 = \"oracle/sly1.iso\"\nsly3 = \"/discs/sly3.iso\"\n";
        let config = Config::parse(text, Path::new("/work/sly-rs")).unwrap();
        assert_eq!(
            config.disc(Game::Sly1),
            Some(Path::new("/work/sly-rs/oracle/sly1.iso"))
        );
        assert_eq!(config.disc(Game::Sly2), None);
        assert_eq!(config.disc(Game::Sly3), Some(Path::new("/discs/sly3.iso")));
    }

    #[test]
    fn cards_default_beside_the_file() {
        let config =
            Config::parse("[cards]\nsly2 = \"/mc/two.ps2\"\n", Path::new("/work")).unwrap();
        assert_eq!(config.card(Game::Sly1), Path::new("/work/cards/sly1.ps2"));
        assert_eq!(config.card(Game::Sly2), Path::new("/mc/two.ps2"));
    }

    #[test]
    fn play_settings_default_and_override() {
        let config = Config::parse("", Path::new(".")).unwrap();
        assert!(config.play().pause_on_focus_loss);
        let text = "[play]\npause_on_focus_loss = false\n[play.keys]\ncross = \"KeyK\"\n";
        let config = Config::parse(text, Path::new(".")).unwrap();
        assert!(!config.play().pause_on_focus_loss);
        assert_eq!(config.play().window, [1280, 896]);
        assert_eq!(config.play().keys["cross"], "KeyK");
        assert!(Config::parse("[play]\nloudness = 3\n", Path::new(".")).is_err());
        assert_eq!(
            Config::parse("[play]\nvolume = 30\n", Path::new("."))
                .unwrap()
                .play()
                .volume,
            30
        );
    }

    #[test]
    fn unknown_keys_are_errors() {
        assert!(Config::parse("[discs]\nsly4 = \"x.iso\"\n", Path::new(".")).is_err());
        assert!(Config::parse("[disks]\n", Path::new(".")).is_err());
        assert_eq!(Config::parse("", Path::new("")).unwrap(), Config::default());
    }

    #[test]
    fn environment_overrides() {
        let mut config = Config::parse("[discs]\nsly2 = \"a.iso\"\n", Path::new("/b")).unwrap();
        config.apply_env(|key| match key {
            "SLY_RS_DISC_SLY2" => Some("/env/sly2.iso".into()),
            "SLY_RS_DISC_SLY3" => Some(OsString::new()),
            _ => None,
        });
        assert_eq!(config.disc(Game::Sly2), Some(Path::new("/env/sly2.iso")));
        assert_eq!(config.disc(Game::Sly3), None);
    }
}
