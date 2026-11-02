//! `sly-rs`: the command-line entry point. Phase 0 provides `info`; the game
//! itself comes later (docs/plan.md).

mod analyze;
mod disasm;
mod elf;
mod info;
mod run;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

/// A Rust static recompilation of the PS2 Sly Cooper trilogy.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Identify discs and print what they contain: files, executable sections,
    /// VU microcode overlays and IOP modules.
    Info {
        /// Disc images, or game keys (sly1, sly2, sly3) looked up in
        /// sly-rs.toml. Default: every configured disc.
        discs: Vec<String>,
        /// Also hash each whole image and compare it with the known build
        /// (reads the full disc).
        #[arg(long)]
        verify: bool,
        /// Configuration file (default: searched; see sly-rs.example.toml).
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Analyse games' executables: functions, padding, jump tables, kernel
    /// call stubs and VU overlays. Default: every configured game.
    Analyze {
        /// Game keys (sly1, sly2, sly3).
        games: Vec<String>,
        /// Also list every function.
        #[arg(long)]
        functions: bool,
    },
    /// Boot a game in the interpreter, headless, and report how far it gets:
    /// kernel calls, threads, hardware touched, subsystems not built yet.
    Run {
        /// Game key (sly1, sly2, sly3).
        game: String,
        /// Fields (1/59.94 s) to run.
        #[arg(long, default_value_t = 60)]
        fields: u64,
        /// Addresses (hex) to watch: each hit's arguments and return address
        /// are reported, with any string the arguments point to.
        #[arg(long, value_delimiter = ',')]
        watch: Vec<String>,
    },
    /// Disassemble a game's executable around an address, marking the
    /// functions discovery finds (blind) and the known symbols.
    Disasm {
        /// Game key (sly1, sly2, sly3).
        game: String,
        /// First address (hex).
        address: String,
        /// Number of instructions.
        #[arg(default_value_t = 32)]
        count: u32,
    },
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("SLY_RS_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let result = match Cli::parse().command {
        Command::Info {
            discs,
            verify,
            config,
        } => info::run(&discs, verify, config.as_deref()),
        Command::Disasm {
            game,
            address,
            count,
        } => disasm::run(&game, &address, count),
        Command::Analyze { games, functions } => analyze::run(&games, functions),
        Command::Run {
            game,
            fields,
            watch,
        } => run::run(&game, fields, &watch),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
