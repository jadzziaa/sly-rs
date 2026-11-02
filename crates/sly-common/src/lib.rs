//! What the three Sly Cooper games share.
//!
//! For now: the table of builds sly-rs supports ([`builds`]), identification
//! of a disc against it, where the user keeps their discs ([`config`]), and
//! known function names ([`mod@symbols`]).

pub mod builds;
pub mod config;
pub mod symbols;

pub use builds::{BUILDS, Build, Game, Identification, identify};
pub use config::{Config, ConfigError};
pub use symbols::{Symbol, symbols};
