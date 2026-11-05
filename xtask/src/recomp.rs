//! `cargo xtask recomp <game>`: recompiles a game's executable into Rust
//! crates under `generated/<game>/`, a Cargo workspace of its own:
//!
//! - `rec-NN/`: the functions, about [`CHUNK`] instructions per crate, each
//!   depending on `ps2-core` alone;
//! - `vu/`: the VU microcode overlays, translated (depending on `ps2-vu`);
//! - `game/`: the binary `<game>`, `sly-app`'s command line with the
//!   generated code attached (`run`, `lockstep`, ...);
//! - `Cargo.toml`, `stamp.txt`; builds go to `generated/<game>/target/`;
//! - `.cargo/config.toml`: builds for this machine's CPU (the generated
//!   code holds the game's and is never distributed, so nothing is lost),
//!   which Cargo reads when run from `generated/<game>/`.
//!
//! Files are written only when their content changes, so a regeneration
//! rebuilds only the crates that moved. `--build` then builds the binary in
//! release mode and reports the time; `--check` also runs the phase 4 gate,
//! the binary's `lockstep` against the interpreter for 300 fields.
//!
//! `--pgo REPLAY` (repeatable, implies `--build`) builds it with
//! profile-guided optimisation: an instrumented build (in
//! `target/pgo/build/`) plays each replay twice, drawing on the CPU and on
//! the GPU, `llvm-profdata` (or `$LLVM_PROFDATA`, of the LLVM rustc uses)
//! merges the counts into `target/pgo/game.profdata`, and the release
//! binary is built with them. The code is the same, so the binary's
//! results are too; only its speed changes.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use ps2_analysis::{Function, discover, vu_overlays};
use sly_common::Game;

/// Instructions per generated crate (rac-rs found ~12,000 a good balance
/// between parallel builds and per-crate overhead).
const CHUNK: u32 = 12_000;

/// Writes `content` to `path` unless it already holds exactly that; true if
/// it wrote.
fn write(path: &Path, content: &str) -> Result<bool> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == content) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

/// The functions, cut into chunks of about [`CHUNK`] instructions, in
/// address order.
fn chunks(functions: &[Function]) -> Vec<Vec<&Function>> {
    let mut chunks: Vec<Vec<&Function>> = vec![Vec::new()];
    let mut size = 0;
    for function in functions {
        let n = (function.end - function.start) / 4;
        if size + n > CHUNK && size > 0 {
            chunks.push(Vec::new());
            size = 0;
        }
        size += n;
        if let Some(last) = chunks.last_mut() {
            last.push(function);
        }
    }
    chunks
}

/// What `recomp` was asked for: the game, whether to check and to build,
/// and the replays to train profile-guided optimisation on.
fn options(args: &[String]) -> Result<(Game, bool, bool, Vec<&Path>)> {
    let usage = "usage: cargo xtask recomp <sly1|sly2|sly3> [--build | --check] [--pgo REPLAY]...";
    let key = args.first().context(usage)?;
    let game = Game::from_key(key).with_context(|| format!("unknown game {key:?}; {usage}"))?;
    let check = args.iter().any(|a| a == "--check");
    let pgo: Vec<&Path> = args
        .windows(2)
        .filter(|w| w[0] == "--pgo")
        .map(|w| Path::new(&w[1]))
        .collect();
    if args.last().is_some_and(|a| a == "--pgo") {
        bail!("--pgo takes a replay; {usage}");
    }
    let build = check || !pgo.is_empty() || args.iter().any(|a| a == "--build");
    Ok((game, check, build, pgo))
}

pub(crate) fn run(root: &Path, args: &[String]) -> Result<()> {
    let (game, check, build, pgo) = options(args)?;

    let started = Instant::now();
    let (image, _) = sly_common::machine::executable(game)?;
    let mut found = discover(&image, &[])?;
    // Hooked and patched functions stay out of generated code: calls to them
    // reach the machine, which runs the hook or interprets the patched code.
    let hooked = sly_common::hooks::left_out(game);
    found.functions.retain(|f| !hooked.contains(&f.start));
    let chunks = chunks(&found.functions);
    let dir = root.join("generated").join(game.key());
    // From generated/<game>/<crate>/ back to the repository's crates.
    let (core, vu, app) = (
        "../../../crates/ps2-core",
        "../../../crates/ps2-vu",
        "../../../crates/sly-app",
    );

    let mut written = 0;
    let mut lines = 0;
    let mut members = Vec::new();
    for (index, functions) in chunks.iter().enumerate() {
        let name = format!("{}-rec-{index:02}", game.key());
        let source = ps2_codegen::module(&image, &found, functions);
        lines += source.lines().count();
        let lib = format!(
            "//! {game}: functions {:#010x}..{:#010x}, generated by `cargo xtask recomp`.\n\
             #![allow(warnings, clippy::all, clippy::pedantic, missing_docs, reason = \"generated code\")]\n\n\
             {source}",
            functions.first().map_or(0, |f| f.start),
            functions.last().map_or(0, |f| f.end)
        );
        let manifest = format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
             license = \"GPL-3.0-or-later\"\npublish = false\n\n\
             [dependencies]\nps2-core = {{ path = \"{core}\" }}\n"
        );
        let crate_dir = dir.join(format!("rec-{index:02}"));
        written += usize::from(write(&crate_dir.join("src/lib.rs"), &lib)?);
        written += usize::from(write(&crate_dir.join("Cargo.toml"), &manifest)?);
        members.push(name);
    }

    // The VU microcode.
    let (vu_name, vu_written, vu_lines) = vu_crate(&dir, game, &image, vu)?;
    written += vu_written;
    lines += vu_lines;

    // The game binary.
    let mut deps = format!("{vu_name} = {{ path = \"../vu\" }}\n");
    let mut list = String::new();
    for (index, name) in members.iter().enumerate() {
        let _ = writeln!(deps, "{name} = {{ path = \"../rec-{index:02}\" }}");
        let _ = writeln!(list, "    {}::BLOCKS,", name.replace('-', "_"));
    }
    let main = format!(
        "//! {game}, recompiled: `sly-rs` with the generated code attached.\n\n\
         static RECOMPILED: sly_app::Recompiled = sly_app::Recompiled {{\n    \
         game: \"{key}\",\n    code_hash: 0x{hash:016x},\n    chunks: &[\n{list}    ],\n    \
         vu: {vu_crate}::TRANSLATIONS,\n}};\n\n\
         fn main() -> std::process::ExitCode {{\n    sly_app::main(Some(&RECOMPILED))\n}}\n",
        key = game.key(),
        hash = image.code_hash(),
        vu_crate = vu_name.replace('-', "_"),
    );
    let manifest = format!(
        "[package]\nname = \"{key}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
         license = \"GPL-3.0-or-later\"\npublish = false\n\n\
         [dependencies]\nsly-app = {{ path = \"{app}\" }}\n{deps}",
        key = game.key()
    );
    written += usize::from(write(&dir.join("game/src/main.rs"), &main)?);
    written += usize::from(write(&dir.join("game/Cargo.toml"), &manifest)?);

    written += usize::from(write(&dir.join("Cargo.toml"), &workspace(&members))?);
    written += usize::from(write(&dir.join(".cargo/config.toml"), NATIVE)?);
    // Start from the main workspace's lock, so versions match.
    let lock = dir.join("Cargo.lock");
    if !lock.exists() {
        std::fs::copy(root.join("Cargo.lock"), &lock).context("copying Cargo.lock")?;
    }
    let stamp = format!(
        "game {key}\ncode hash {hash:016x}\nps2-codegen {version}\nfunctions {functions}\n\
         crates {crates}\n",
        key = game.key(),
        hash = image.code_hash(),
        version = env!("CARGO_PKG_VERSION"),
        functions = found.functions.len(),
        crates = members.len(),
    );
    written += usize::from(write(&dir.join("stamp.txt"), &stamp)?);
    eprintln!(
        "== {game}: {} functions in {} crates, {lines} lines; {written} files written; {:.1} s",
        found.functions.len(),
        members.len(),
        started.elapsed().as_secs_f64()
    );

    if build {
        let profile = if pgo.is_empty() {
            None
        } else {
            Some(train(&dir, game, &pgo)?)
        };
        build_and_check(&dir, game, check, profile.as_deref())?;
    }
    Ok(())
}

/// The flags every build of the generated workspace takes (as
/// `.cargo/config.toml` gives them, which `RUSTFLAGS` replaces).
const NATIVE_FLAGS: &str = "-Ctarget-cpu=native";

/// Profile-guided optimisation's first half: builds the binary instrumented,
/// plays each of `replays` on it drawing on the CPU and on the GPU, and
/// merges the counts. Returns the merged profile.
fn train(dir: &Path, game: Game, replays: &[&Path]) -> Result<std::path::PathBuf> {
    let pgo = dir.join("target/pgo");
    let raw = pgo.join("raw");
    if raw.exists() {
        std::fs::remove_dir_all(&raw).context("clearing old profiles")?;
    }
    std::fs::create_dir_all(&raw).context("creating target/pgo/raw")?;
    let started = Instant::now();
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
        .args(["build", "--release", "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .current_dir(dir)
        .env(
            "RUSTFLAGS",
            format!("{NATIVE_FLAGS} -Cprofile-generate={}", raw.display()),
        )
        .env("CARGO_TARGET_DIR", pgo.join("build"))
        .status()
        .context("running cargo build")?;
    if !status.success() {
        bail!("the instrumented build failed");
    }
    eprintln!(
        "== built the instrumented binary in {:.1} s",
        started.elapsed().as_secs_f64()
    );
    let binary = pgo.join("build/release").join(game.key());
    for replay in replays {
        for gpu in [false, true] {
            eprintln!(
                "== training: replay {}{}",
                replay.display(),
                if gpu { " --gpu" } else { "" }
            );
            // Two drawing threads: with many, the threads' updates of the
            // same counters make the instrumented binary crawl.
            let mut run = Command::new(&binary);
            run.arg("replay").arg(replay).env("SLY_RS_GS_THREADS", "2");
            if gpu {
                run.arg("--gpu");
            }
            let status = run
                .status()
                .with_context(|| format!("running {}", binary.display()))?;
            if !status.success() {
                // Without a GPU the GPU's code goes untrained; the rest is.
                eprintln!("== that run failed: its counts are left out");
            }
        }
    }
    let profile = pgo.join("game.profdata");
    let tool = std::env::var("LLVM_PROFDATA").unwrap_or_else(|_| "llvm-profdata".to_owned());
    let status = Command::new(&tool)
        .args(["merge", "-o"])
        .arg(&profile)
        .arg(&raw)
        .status()
        .with_context(|| {
            format!("running {tool} (set LLVM_PROFDATA to the one of rustc's LLVM)")
        })?;
    if !status.success() {
        bail!("{tool} could not merge the profiles");
    }
    Ok(profile)
}

/// The generated workspace's Cargo configuration: this machine's CPU, for
/// the four-lane float code of the VUs above all (AVX2's per-lane shifts
/// and wider registers). Rust never fuses or reorders floating-point
/// operations, so results do not change.
const NATIVE: &str = "# Generated by cargo xtask recomp: do not edit.\n\
    # Built on and for this machine: the generated code is never distributed.\n\
    [build]\nrustflags = [\"-C\", \"target-cpu=native\"]\n";

/// The generated workspace's manifest: generated crates optimised even in
/// dev builds, one codegen unit each in release (they build in parallel
/// with each other; the VU crate, built alone, keeps the default units).
fn workspace(members: &[String]) -> String {
    let mut profiles = String::new();
    for name in members {
        let _ = writeln!(
            profiles,
            "[profile.dev.package.{name}]\nopt-level = 1\n\
             [profile.release.package.{name}]\ncodegen-units = 1\n"
        );
    }
    format!(
        "# Generated by cargo xtask recomp: do not edit.\n\
         [workspace]\nresolver = \"3\"\nmembers = [\"rec-*\", \"vu\", \"game\"]\n\n\
         [profile.release]\nlto = \"thin\"\ndebug = \"line-tables-only\"\n\n{profiles}"
    )
}

/// Writes the crate of `game`'s translated VU microcode, `vu/`, depending
/// on `ps2-vu` at `vu` (relative to it): its name, files written and lines.
fn vu_crate(
    dir: &Path,
    game: Game,
    image: &ps2_analysis::Image,
    vu: &str,
) -> Result<(String, usize, usize)> {
    let overlays = vu_overlays(image)?;
    let vu_source = ps2_codegen::vu::module(&overlays);
    let vu_name = format!("{}-vu", game.key());
    let lib = format!(
        "//! {game}: the VU microcode overlays, translated by `cargo xtask recomp`.\n\
         #![allow(warnings, clippy::all, clippy::pedantic, missing_docs, reason = \"generated code\")]\n\n\
         {vu_source}"
    );

    let manifest = format!(
        "[package]\nname = \"{vu_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
         license = \"GPL-3.0-or-later\"\npublish = false\n\n\
         [dependencies]\nps2-vu = {{ path = \"{vu}\" }}\n"
    );
    let written = usize::from(write(&dir.join("vu/src/lib.rs"), &lib)?)
        + usize::from(write(&dir.join("vu/Cargo.toml"), &manifest)?);
    Ok((vu_name, written, vu_source.lines().count()))
}

/// Plays `games/<game>/replays/<name>.toml` on the generated binary, which
/// checks its checkpoints.
pub(crate) fn replay(root: &Path, game: Game, name: &str) -> Result<()> {
    let binary = root
        .join("generated")
        .join(game.key())
        .join("target/release")
        .join(game.key());
    let file = root
        .join("games")
        .join(game.key())
        .join("replays")
        .join(format!("{name}.toml"));
    eprintln!(
        "== recompiled replay: {} replay {}",
        binary.display(),
        file.display()
    );
    let status = Command::new(&binary)
        .arg("replay")
        .arg(&file)
        .status()
        .with_context(|| format!("running {}", binary.display()))?;
    if !status.success() {
        bail!("the recompiled game missed the replay's checkpoints");
    }
    Ok(())
}

/// Builds the generated binary in release mode (optimised with `profile`
/// if given) and, with `check`, runs it in lockstep with the interpreter
/// for 300 fields.
fn build_and_check(dir: &Path, game: Game, check: bool, profile: Option<&Path>) -> Result<()> {
    let started = Instant::now();
    // From the workspace's directory, so Cargo reads its configuration.
    let mut cargo = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()));
    cargo
        .args(["build", "--release", "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .current_dir(dir);
    if let Some(profile) = profile {
        cargo.env(
            "RUSTFLAGS",
            format!("{NATIVE_FLAGS} -Cprofile-use={}", profile.display()),
        );
    }
    let status = cargo.status().context("running cargo build")?;
    if !status.success() {
        bail!("building {} failed", dir.display());
    }
    eprintln!(
        "== built generated/{key}/target/release/{key} in {:.1} s",
        started.elapsed().as_secs_f64(),
        key = game.key()
    );
    if check {
        let binary = dir.join("target/release").join(game.key());
        eprintln!(
            "== lockstep: {} lockstep {} --fields 300",
            binary.display(),
            game.key()
        );
        let status = Command::new(&binary)
            .args(["lockstep", game.key(), "--fields", "300"])
            .status()
            .with_context(|| format!("running {}", binary.display()))?;
        if !status.success() {
            bail!("the recompiled game and the interpreter differ");
        }
    }
    Ok(())
}
