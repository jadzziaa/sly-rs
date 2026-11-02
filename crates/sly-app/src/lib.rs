//! The sly-rs program: its command line, shared by the `sly-rs` binary
//! (interpreter only) and the binaries `cargo xtask recomp` generates for
//! each game, which pass their [`Recompiled`] code to [`main`].

mod analyze;
mod disasm;
mod elf;
mod elf_run;
mod info;
mod lockstep;
mod run;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ps2_core::rec::{GuestFn, Table};
use tracing_subscriber::EnvFilter;

/// A game's recompiled executable, as `cargo xtask recomp` generates it.
pub struct Recompiled {
    /// The game's key (`sly1`, ...).
    pub game: &'static str,
    /// [`ps2_analysis::Image::code_hash`] of the executable it was made
    /// from.
    pub code_hash: u64,
    /// Every block start with its function, one slice per generated crate.
    pub chunks: &'static [&'static [(u32, GuestFn)]],
}

/// Boots `game`, running `recompiled` code where it has some.
///
/// # Errors
///
/// If the game does not boot, or the recompiled code was made from another
/// executable.
pub fn boot(
    game: sly_common::Game,
    recompiled: Option<&Recompiled>,
) -> anyhow::Result<(ps2_ee::Machine, ps2_analysis::Image)> {
    let (mut machine, image) = sly_common::machine::boot(game)?;
    if let Some(recompiled) = recompiled {
        anyhow::ensure!(
            recompiled.game == game.key() && recompiled.code_hash == image.code_hash(),
            "the recompiled code is for another executable ({} {:016x}; this is {} {:016x})",
            recompiled.game,
            recompiled.code_hash,
            game.key(),
            image.code_hash()
        );
        machine.attach(recompiled.table());
    }
    Ok((machine, image))
}

impl Recompiled {
    /// The table the machine runs from.
    #[must_use]
    pub fn table(&self) -> Table {
        let blocks: Vec<(u32, GuestFn)> =
            self.chunks.iter().flat_map(|c| c.iter().copied()).collect();
        Table::new(&blocks)
    }
}

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
    /// Boot a game, headless, and report how far it gets: kernel calls,
    /// threads, hardware touched, subsystems not built yet, and where its
    /// instructions ran. Recompiled code runs where this binary has it.
    Run {
        /// Game key (sly1, sly2, sly3).
        game: String,
        /// Fields (1/59.94 s) to run.
        #[arg(long, default_value_t = 60)]
        fields: u64,
        /// Addresses (hex) to watch: each hit's arguments and return address
        /// are reported, with any string the arguments point to. Implies
        /// --interpret.
        #[arg(long, value_delimiter = ',')]
        watch: Vec<String>,
        /// Use the interpreter only (and sample where time goes).
        #[arg(long)]
        interpret: bool,
    },
    /// Run the recompiled game and the interpreter side by side and compare
    /// them at every field: registers, RAM and the scratchpad must be equal.
    Lockstep {
        /// Game key (sly1, sly2, sly3).
        game: String,
        /// Fields (1/59.94 s) to run.
        #[arg(long, default_value_t = 300)]
        fields: u64,
    },
    /// Run any PS2 executable in the interpreter (a test program) and print
    /// what it writes to the EE's debug console.
    Elf {
        /// The ELF file.
        path: PathBuf,
        /// Fields (1/59.94 s) to run at most.
        #[arg(long, default_value_t = 600)]
        fields: u64,
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

/// Runs the command line; `recompiled` is the generated code this binary
/// carries, if any.
#[must_use]
pub fn main(recompiled: Option<&Recompiled>) -> ExitCode {
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
        Command::Elf { path, fields } => elf_run::run(&path, fields),
        Command::Run {
            game,
            fields,
            watch,
            interpret,
        } => run::run(&game, fields, &watch, recompiled.filter(|_| !interpret)),
        Command::Lockstep { game, fields } => lockstep::run(&game, fields, recompiled),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
