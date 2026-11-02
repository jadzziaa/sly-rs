//! What the three Sly Cooper games share.
//!
//! For now: the table of builds sly-rs supports ([`builds`]), identification
//! of a disc against it, and where the user keeps their discs ([`config`]).

pub mod builds;
pub mod config;

pub use builds::{BUILDS, Build, Game, Identification, identify};
pub use config::{Config, ConfigError};
