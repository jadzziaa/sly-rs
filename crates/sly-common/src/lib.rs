//! What the three Sly Cooper games share.
//!
//! For now: the table of builds sly-rs supports ([`builds`]), identification
//! of a disc against it, where the user keeps their discs ([`config`]),
//! known function names ([`mod@symbols`]), and the game's state read from its
//! memory ([`state`]).

pub mod builds;
pub mod config;
pub mod hooks;
pub mod machine;
pub mod replay;
pub mod state;
pub mod symbols;

pub use builds::{BUILDS, Build, Game, Identification, identify};
pub use config::{Config, ConfigError};
pub use symbols::{Symbol, symbols};
