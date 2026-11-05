//! Replays: scripted pad input with checkpoints (plan §6, phase 5).
//!
//! A replay is a TOML file:
//!
//! ```toml
//! game = "sly1"
//! fields = 2000
//! card = "formatted"   # a memory card: a blank one, or an image's path
//!                      #   (relative to the replay); none if left out
//! presses = ["cross@700", "start@900"]   # BUTTON@FIELD, held six fields
//! inputs = ["1200:0040", "1206:0000:80807f00"]
//!
//! [[checkpoint]]
//! field = 600
//! world = 0          # what the game state says (Sly 1: world, level,
//! level = 2          #   and optionally the interface state)
//! hash = "…"         # the machine's state hash, filled in by --record
//! ```
//!
//! `inputs` is the exact form live play records: the pad's state from
//! `FIELD` on, until the next entry, as `FIELD:BUTTONS` or
//! `FIELD:BUTTONS:STICKS` in hex (buttons in the DualShock's bit order, bit
//! set = pressed, as [`BUTTONS`]; sticks right x, right y, left x, left y,
//! `80` centred, left out when all are). Presses are added to it.
//!
//! Input changes only at field boundaries, so a replay runs the same way
//! every time, interpreted or recompiled, live or replayed.
//!
//! A replay's card is read, never written: the game saves to a copy in
//! memory, so the replay starts from the same card every time.

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
    /// An input is not `FIELD:BUTTONS[:STICKS]` in hex, or out of order.
    #[error("bad input {0:?} (FIELD:BUTTONS[:STICKS] in hex, in field order)")]
    Input(String),
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

/// A replay's input: exact changes, and presses added to them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Script {
    presses: Vec<Press>,
    /// (field, the pad from then on), in field order.
    changes: Vec<(u64, PadInput)>,
}

impl From<Vec<Press>> for Script {
    fn from(presses: Vec<Press>) -> Self {
        Self {
            presses,
            changes: Vec::new(),
        }
    }
}

impl Script {
    /// A replay's presses and inputs.
    ///
    /// # Errors
    ///
    /// A press or input that does not parse, or inputs out of field order.
    pub fn of(replay: &Replay) -> Result<Self, ReplayError> {
        let changes = replay
            .inputs
            .iter()
            .map(|i| parse_input(i).ok_or_else(|| ReplayError::Input(i.clone())))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(w) = changes.windows(2).find(|w| w[0].0 >= w[1].0) {
            return Err(ReplayError::Input(format_input(w[1].0, w[1].1)));
        }
        Ok(Self {
            presses: parse_presses(&replay.presses)?,
            changes,
        })
    }

    /// The pad's state during `field`.
    #[must_use]
    pub fn input_at(&self, field: u64) -> PadInput {
        let base = match self.changes.partition_point(|&(f, _)| f <= field) {
            0 => PadInput::default(),
            k => self.changes[k - 1].1,
        };
        let pressed = input_at(&self.presses, field).buttons;
        PadInput {
            buttons: base.buttons | pressed,
            ..base
        }
    }
}

/// `FIELD:BUTTONS[:STICKS]`, in hex (see the module's comment).
fn parse_input(text: &str) -> Option<(u64, PadInput)> {
    let mut parts = text.split(':');
    let field = parts.next()?.parse().ok()?;
    let buttons = u16::from_str_radix(parts.next()?, 16).ok()?;
    let sticks = match parts.next() {
        None => [0x80; 4],
        Some(s) if s.len() == 8 => u32::from_str_radix(s, 16).ok()?.to_be_bytes(),
        Some(_) => return None,
    };
    parts
        .next()
        .is_none()
        .then_some((field, PadInput { buttons, sticks }))
}

/// The pad's state from `field` on, as `inputs` writes it.
#[must_use]
pub fn format_input(field: u64, pad: PadInput) -> String {
    if pad.sticks == [0x80; 4] {
        format!("{field}:{:04x}", pad.buttons)
    } else {
        format!(
            "{field}:{:04x}:{:08x}",
            pad.buttons,
            u32::from_be_bytes(pad.sticks)
        )
    }
}

/// Records live input as a replay's `inputs`: an entry each time the pad's
/// state changes.
#[derive(Clone, Debug, Default)]
pub struct Recorder {
    inputs: Vec<String>,
    last: PadInput,
}

impl Recorder {
    /// The pad's state during `field` (fields in order).
    pub fn record(&mut self, field: u64, pad: PadInput) {
        if pad != self.last {
            self.inputs.push(format_input(field, pad));
            self.last = pad;
        }
    }

    /// The entries so far.
    #[must_use]
    pub fn inputs(&self) -> &[String] {
        &self.inputs
    }
}

/// Runs one field with the pad in state `pad`: the step replays and live
/// play share.
pub fn field(machine: &mut Machine, pad: PadInput) -> Stop {
    machine.iop.set_pad(pad);
    machine.run(FIELD_CYCLES)
}

/// Runs `fields` fields from the machine's current one, with `script`'s
/// input.
pub fn run(machine: &mut Machine, fields: u64, script: &Script) -> Stop {
    let first = machine.cpu.cycles / FIELD_CYCLES;
    for f in first..first + fields {
        let stop = field(machine, script.input_at(f));
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presses: Vec<String>,
    /// The memory card in port 0: [`card::FORMATTED`](crate::card::FORMATTED)
    /// or an image's path, relative to the replay. None if left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
    /// The pad's state as it changes, `FIELD:BUTTONS[:STICKS]` (hex).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Live input recorded as `inputs` reads back the same, field for
    /// field, with presses added on top.
    #[test]
    fn recorded_inputs_replay_field_for_field() {
        let live = |f: u64| PadInput {
            buttons: if f % 37 < 5 { 1 << (f / 37 % 16) } else { 0 },
            sticks: if (100..140).contains(&f) {
                [0x80, 0x80, u8::try_from(f * 3 % 256).unwrap_or(0), 0x10]
            } else {
                [0x80; 4]
            },
        };
        let mut recorder = Recorder::default();
        for f in 0..400 {
            recorder.record(f, live(f));
        }
        let replay = Replay {
            game: "sly1".to_owned(),
            fields: 400,
            presses: vec!["cross@200".to_owned()],
            card: Some(crate::card::FORMATTED.to_owned()),
            inputs: recorder.inputs().to_vec(),
            checkpoints: Vec::new(),
        };
        let text = toml::to_string(&replay).unwrap();
        let back: Replay = toml::from_str(&text).unwrap();
        assert_eq!(back.card.as_deref(), Some("formatted"));
        let script = Script::of(&back).unwrap();
        for f in 0..400 {
            let mut expected = live(f);
            if (200..206).contains(&f) {
                expected.buttons |= 1 << 14;
            }
            assert_eq!(script.input_at(f), expected, "field {f}");
        }
        assert!(
            Script::of(&Replay {
                inputs: vec!["5:0001".to_owned(), "5:0002".to_owned()],
                ..Replay::default()
            })
            .is_err()
        );
    }
}
