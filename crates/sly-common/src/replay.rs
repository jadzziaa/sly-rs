//! Replays: scripted pad input with checkpoints (plan §6, phase 5).
//!
//! A replay is a TOML file:
//!
//! ```toml
//! game = "sly1"
//! fields = 2000
//! presses = ["cross@700", "start@900"]   # BUTTON@FIELD, held six fields
//!
//! [[checkpoint]]
//! field = 600
//! world = 0          # what the game state says (Sly 1: world, level,
//! level = 2          #   and optionally the interface state)
//! hash = "…"         # the machine's state hash, filled in by --record
//! ```
//!
//! Input changes only at field boundaries, so a replay runs the same way
//! every time, interpreted or recompiled.

use std::path::Path;

use ps2_ee::{FIELD_CYCLES, Machine, Stop};
use ps2_iop::pad::PadInput;
use serde::{Deserialize, Serialize};

/// Fields a press lasts (a tenth of a second).
const HOLD: u64 = 6;

/// The DualShock's buttons, in bit order.
pub const BUTTONS: [&str; 16] = [
    "select", "l3", "r3", "start", "up", "right", "down", "left", "l2", "r2", "l1", "r1",
    "triangle", "circle", "cross", "square",
];

/// Errors from replays.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReplayError {
    /// A press is not `BUTTON@FIELD` with a known button.
    #[error("bad press {0:?} (BUTTON@FIELD, a button among {buttons})", buttons = BUTTONS.join(", "))]
    Press(String),
    /// The file could not be read or written.
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
    /// The file is not a replay.
    #[error("{0}: {1}")]
    Toml(String, String),
}

/// A button pressed at a field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Press {
    /// The button's bit.
    pub button: u16,
    /// The field it goes down in.
    pub field: u64,
}

/// Parses `start@300` presses.
///
/// # Errors
///
/// A press that is not `BUTTON@FIELD`.
pub fn parse_presses(presses: &[String]) -> Result<Vec<Press>, ReplayError> {
    presses
        .iter()
        .map(|p| {
            let bad = || ReplayError::Press(p.clone());
            let (name, field) = p.split_once('@').ok_or_else(bad)?;
            let bit = BUTTONS
                .iter()
                .position(|b| b.eq_ignore_ascii_case(name))
                .ok_or_else(bad)?;
            Ok(Press {
                button: 1 << bit,
                field: field.parse().map_err(|_| bad())?,
            })
        })
        .collect()
}

/// The pad's state during `field`.
#[must_use]
pub fn input_at(presses: &[Press], field: u64) -> PadInput {
    let buttons = presses
        .iter()
        .filter(|p| (p.field..p.field + HOLD).contains(&field))
        .fold(0, |b, p| b | p.button);
    PadInput {
        buttons,
        ..PadInput::default()
    }
}

/// Runs `fields` fields from the machine's current one, pressing as
/// scripted.
pub fn run(machine: &mut Machine, fields: u64, presses: &[Press]) -> Stop {
    let first = machine.cpu.cycles / FIELD_CYCLES;
    for field in first..first + fields {
        machine.iop.set_pad(input_at(presses, field));
        let stop = machine.run(FIELD_CYCLES);
        if stop != Stop::Budget {
            return stop;
        }
    }
    Stop::Budget
}

/// A checkpoint: the state expected after `field` fields.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// After this many fields.
    pub field: u64,
    /// The game state's world.
    pub world: i32,
    /// Its level.
    pub level: i32,
    /// The interface state, when it matters (Sly 1's title and a new
    /// game's intro are the same level: attract 1, in game 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<i32>,
    /// The machine's state hash, in hex, once recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// A replay.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replay {
    /// The game's key.
    pub game: String,
    /// Fields to run.
    pub fields: u64,
    /// Presses, `BUTTON@FIELD`.
    pub presses: Vec<String>,
    /// Checkpoints, in field order.
    #[serde(default, rename = "checkpoint")]
    pub checkpoints: Vec<Checkpoint>,
}

impl Replay {
    /// Reads a replay file.
    ///
    /// # Errors
    ///
    /// If the file cannot be read or is not a replay.
    pub fn load(path: &Path) -> Result<Self, ReplayError> {
        let name = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|e| ReplayError::Io(name.clone(), e))?;
        toml::from_str(&text).map_err(|e| ReplayError::Toml(name, e.to_string()))
    }

    /// Writes the replay back (after recording hashes), keeping the comment
    /// lines at its top.
    ///
    /// # Errors
    ///
    /// If the file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), ReplayError> {
        let name = path.display().to_string();
        let old = std::fs::read_to_string(path).unwrap_or_default();
        let mut header = String::new();
        for line in old.lines().take_while(|l| l.starts_with('#')) {
            header.push_str(line);
            header.push('\n');
        }
        let body =
            toml::to_string(self).map_err(|e| ReplayError::Toml(name.clone(), e.to_string()))?;
        std::fs::write(path, format!("{header}{body}")).map_err(|e| ReplayError::Io(name, e))
    }
}
