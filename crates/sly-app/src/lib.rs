//! The sly-rs program: its command line, shared by the `sly-rs` binary
//! (interpreter only) and the binaries `cargo xtask recomp` generates for
//! each game, which pass their [`Recompiled`] code to [`main`].

mod analyze;
mod disasm;
mod elf;
mod elf_run;
mod gs_replay;
mod info;
mod lockstep;
mod play;
mod replay;
mod run;
mod vu_bench;

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
    /// The VU microcode overlays, translated.
    pub vu: &'static [ps2_vu::micro::Translation],
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
        machine.attach_vu(recompiled.vu);
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
        /// Pad presses, BUTTON@FIELD (start@300,cross@420): each holds the
        /// button for six fields.
        #[arg(long, value_delimiter = ',')]
        press: Vec<String>,
        /// Write what the display shows at the end to this PNG file.
        #[arg(long)]
        screenshot: Option<PathBuf>,
        /// Write the last --gs-dump-fields fields' GS traffic to this file
        /// as a PCSX2 GS dump (`.gs`), for comparing pictures with PCSX2.
        /// It holds the game's graphics: keep it out of the repository.
        #[arg(long)]
        gs_dump: Option<PathBuf>,
        /// Fields the GS dump covers.
        #[arg(long, default_value_t = 2)]
        gs_dump_fields: u64,
    },
    /// Run the recompiled game and the interpreter side by side and compare
    /// them at every field: registers, RAM and the scratchpad must be equal.
    Lockstep {
        /// Game key (sly1, sly2, sly3).
        game: String,
        /// Fields (1/59.94 s) to run.
        #[arg(long, default_value_t = 300)]
        fields: u64,
        /// Drive both machines with a replay's presses.
        #[arg(long)]
        replay: Option<PathBuf>,
    },
    /// Run VU1 programs recorded with `$SLY_RS_VU_RECORD=PATH,SKIP,COUNT`
    /// through the interpreter and the translated microcode: time both and
    /// check they agree.
    #[command(hide = true)]
    VuBench {
        /// The recording.
        path: PathBuf,
        /// Times to run it (the best is reported).
        #[arg(long, default_value_t = 3)]
        repeat: u32,
        /// Time the translated microcode alone (for profiling it): no
        /// interpreter run, no comparison.
        #[arg(long)]
        translated_only: bool,
    },
    /// List the programs of a VU1 recording, or step one through the
    /// interpreter, printing each pair and the registers it changed.
    #[command(hide = true)]
    VuTrace {
        /// The recording.
        path: PathBuf,
        /// The program to step (default: list them all).
        #[arg(long)]
        program: Option<usize>,
        /// A 32-bit word (hex) to look for in data memory and the packets
        /// sent.
        #[arg(long, value_parser = parse_hex)]
        word: Option<u32>,
        /// Data memory to print as the program starts: the first qword
        /// (hex) and how many (`238,16`).
        #[arg(long, value_parser = parse_range)]
        data: Option<(usize, usize)>,
    },
    /// Play the game in a window, in real time, with the keyboard or a
    /// gamepad. Keys: WASD left stick, IJKL right stick, arrows D-pad,
    /// Space Cross, E Circle, Q Square, F Triangle, 2/3 L1/R1, 1/4 L2/R2,
    /// G/H L3/R3, Enter Start, Backspace Select; P pauses, Escape quits.
    /// The game pauses while the window is not focused.
    Play {
        /// Game key (sly1, sly2, sly3).
        game: String,
        /// Record the session as a replay in this file (written on quitting;
        /// `sly-rs replay FILE` checks it reproduces).
        #[arg(long)]
        record: Option<PathBuf>,
        /// Play this replay through first, at full speed, then hand over
        /// (the recording keeps its input).
        #[arg(long)]
        replay: Option<PathBuf>,
        /// Use the interpreter only.
        #[arg(long)]
        interpret: bool,
    },
    /// Play a GS dump (`run --gs-dump`) through sly-rs's GS alone: time it
    /// (a benchmark of drawing) and write what the display shows at its
    /// end.
    GsReplay {
        /// The dump.
        path: PathBuf,
        /// Times to play it.
        #[arg(long, default_value_t = 1)]
        repeat: u32,
        /// Threads to draw with (the result is the same).
        #[arg(long, default_value_t = 1)]
        threads: usize,
        /// Write what the display shows at the end to this PNG file.
        #[arg(long)]
        screenshot: Option<PathBuf>,
        /// Write what the display shows at every V-sync into this
        /// directory, `frameNNNNN.png` numbered as PCSX2 numbers the
        /// dump's frames (pictures of the game: keep them out of the
        /// repository).
        #[arg(long)]
        frames: Option<PathBuf>,
    },
    /// Run a replay (scripted presses) and check its checkpoints: the game's
    /// world and level, and the machine's state hash.
    Replay {
        /// The replay file (games/sly1/replays/...).
        path: PathBuf,
        /// Record the state hashes into the file instead of checking them.
        #[arg(long)]
        record: bool,
        /// Use the interpreter only.
        #[arg(long)]
        interpret: bool,
        /// Write what the display shows every --every fields into this
        /// directory, `fieldNNNNNN.png` (pictures of the game: keep them
        /// out of the repository).
        #[arg(long)]
        frames: Option<PathBuf>,
        /// Fields between pictures.
        #[arg(long, default_value_t = 60)]
        every: u64,
        /// Fields to run past the replay's end, the pad left alone.
        #[arg(long, default_value_t = 0)]
        beyond: u64,
    },
    /// Write a freshly formatted 8 MB memory card image (`.ps2`, with ECC,
    /// as PCSX2 keeps them), or check an existing one with --check.
    FormatCard {
        /// The image.
        path: PathBuf,
        /// Read the image and report on it instead of writing one.
        #[arg(long)]
        check: bool,
    },
    /// Run any PS2 executable in the interpreter (a test program) and print
    /// what it writes to the EE's debug console.
    Elf {
        /// The ELF file.
        path: PathBuf,
        /// Fields (1/59.94 s) to run at most.
        #[arg(long, default_value_t = 600)]
        fields: u64,
        /// Print what the IOP's modules print instead of the EE's console.
        #[arg(long)]
        iop: bool,
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
#[allow(clippy::too_many_lines, reason = "one arm per command")]
pub fn main(recompiled: Option<&'static Recompiled>) -> ExitCode {
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
        Command::Elf { path, fields, iop } => elf_run::run(&path, fields, iop),
        Command::Run {
            game,
            fields,
            watch,
            interpret,
            press,
            screenshot,
            gs_dump,
            gs_dump_fields,
        } => run::run(
            &game,
            fields,
            &watch,
            &press,
            &run::Pictures {
                screenshot: screenshot.as_deref(),
                gs_dump: gs_dump.as_deref().map(|p| (p, gs_dump_fields)),
            },
            recompiled.filter(|_| !interpret),
        ),
        Command::Lockstep {
            game,
            fields,
            replay,
        } => lockstep::run(&game, fields, replay.as_deref(), recompiled),
        Command::Replay {
            path,
            record,
            interpret,
            frames,
            every,
            beyond,
        } => replay::run(
            &path,
            record,
            &replay::Frames {
                dir: frames.as_deref(),
                every,
                beyond,
            },
            recompiled.filter(|_| !interpret),
        ),
        Command::VuBench {
            path,
            repeat,
            translated_only,
        } => vu_bench::bench(
            &path,
            repeat,
            translated_only,
            recompiled.map_or(&[], |r| r.vu),
        ),
        Command::VuTrace {
            path,
            program,
            word,
            data,
        } => vu_bench::trace(&path, program, word, data),
        Command::Play {
            game,
            record,
            replay,
            interpret,
        } => sly_common::Game::from_key(&game)
            .ok_or_else(|| anyhow::anyhow!("unknown game {game:?}"))
            .and_then(|game| {
                play::run(
                    play::Options {
                        game,
                        record,
                        replay,
                        settings: sly_common::config::Config::load()
                            .map(|c| c.play().clone())
                            .unwrap_or_default(),
                    },
                    recompiled.filter(|_| !interpret),
                )
            }),
        Command::FormatCard { path, check } => format_card(&path, check),
        Command::GsReplay {
            path,
            repeat,
            threads,
            screenshot,
            frames,
        } => gs_replay::run(
            &path,
            repeat,
            threads,
            screenshot.as_deref(),
            frames.as_deref(),
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

fn format_card(path: &std::path::Path, check: bool) -> anyhow::Result<()> {
    use anyhow::Context as _;
    if check {
        let image = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let card = ps2_memcard::Card::new(image).with_context(|| path.display().to_string())?;
        let names: Vec<_> = card.root().into_iter().map(|e| e.name).collect();
        println!(
            "{}: formatted, {} KB free; root: {}",
            path.display(),
            card.free_clusters(),
            names.join(" ")
        );
        return Ok(());
    }
    if path.exists() {
        anyhow::bail!("{} exists: not overwriting a memory card", path.display());
    }
    std::fs::write(path, ps2_memcard::Card::formatted().image())
        .with_context(|| format!("writing {}", path.display()))?;
    println!("{}: a formatted 8 MB memory card", path.display());
    Ok(())
}

fn parse_hex(s: &str) -> Result<u32, std::num::ParseIntError> {
    u32::from_str_radix(s.trim_start_matches("0x"), 16)
}

fn parse_range(s: &str) -> Result<(usize, usize), String> {
    let (from, count) = s.split_once(',').ok_or("expected FIRST,COUNT")?;
    let from =
        usize::from_str_radix(from.trim_start_matches("0x"), 16).map_err(|e| e.to_string())?;
    Ok((
        from,
        count
            .parse()
            .map_err(|e: std::num::ParseIntError| e.to_string())?,
    ))
}
