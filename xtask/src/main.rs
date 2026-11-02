//! Project automation, run as `cargo xtask <command>`:
//!
//! - `ci`: everything a change must pass: formatting, clippy, rustdoc, tests,
//!   `cargo deny` (when installed) and `check-deps`.
//! - `test-full`: the ignored tests, in release mode: those that need the
//!   user's discs and the exhaustive ones (every 32-bit word).
//! - `check-deps`: no `ps2-*` crate may depend on a `sly-*` crate (goal G5
//!   of docs/plan.md).
//! - `recomp <game> [--build | --check]`: recompiles a game's executable
//!   into crates under `generated/<game>/`; `--build` builds its binary,
//!   `--check` also runs it in lockstep with the interpreter (the phase 4
//!   gate).
//! - `conformance-build`: builds `conformance/conformance.elf` with the
//!   ps2dev toolchain (the `ps2dev/ps2dev` image, run with podman).
//! - `conformance-record`: records PCSX2's output for it, recompiler and
//!   interpreter, into `conformance/pcsx2-*.txt` (headless, in gamescope).
//! - `import-sly1-symbols <symbol_addrs.txt>`: converts the function symbols
//!   of the Sly 1 decompilation (github.com/TheOnlyZac/sly1, CC0) into
//!   `games/sly1/symbols.tsv`.

// Step headers and results go to stderr; this is a developer tool.
#![allow(clippy::print_stderr)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

mod recomp;

fn main() -> Result<()> {
    let root = workspace_root();
    let task = std::env::args().nth(1).unwrap_or_default();
    match task.as_str() {
        "ci" => ci(&root),
        "test-full" => test_full(&root),
        "check-deps" => check_deps(&root),
        "recomp" => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            recomp::run(&root, &args)
        }
        "conformance-build" => conformance_build(&root),
        "conformance-record" => conformance_record(&root),
        "import-sly1-symbols" => {
            let source = std::env::args()
                .nth(2)
                .context("usage: cargo xtask import-sly1-symbols <symbol_addrs.txt>")?;
            import_sly1_symbols(&root, Path::new(&source))
        }
        _ => {
            eprintln!(
                "usage: cargo xtask <ci | test-full | check-deps | recomp GAME [--build|--check] | \
                 conformance-build | conformance-record | import-sly1-symbols FILE>"
            );
            bail!("unknown task {task:?}")
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn ci(root: &Path) -> Result<()> {
    cargo(root, "format", &["fmt", "--all", "--check"], &[])?;
    cargo(
        root,
        "clippy",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
        &[],
    )?;
    cargo(
        root,
        "rustdoc",
        &["doc", "--workspace", "--no-deps", "--locked"],
        &[("RUSTDOCFLAGS", "-D warnings")],
    )?;
    if has_cargo_subcommand(root, "nextest") {
        cargo(
            root,
            "tests (nextest)",
            &["nextest", "run", "--workspace", "--locked"],
            &[],
        )?;
        cargo(
            root,
            "doctests",
            &["test", "--workspace", "--doc", "--locked"],
            &[],
        )?;
    } else {
        cargo(root, "tests", &["test", "--workspace", "--locked"], &[])?;
    }
    if has_cargo_subcommand(root, "deny") {
        cargo(root, "cargo deny", &["deny", "check"], &[])?;
    } else {
        eprintln!("== cargo deny: SKIPPED (not installed: cargo install cargo-deny)");
    }
    check_deps(root)?;
    eprintln!("== ci: all steps passed");
    Ok(())
}

fn test_full(root: &Path) -> Result<()> {
    cargo(
        root,
        "ignored tests (discs, exhaustive)",
        &[
            "test",
            "--workspace",
            "--release",
            "--locked",
            "--",
            "--ignored",
        ],
        &[],
    )
}

/// Fails if any `ps2-*` package depends on a `sly-*` package.
fn check_deps(root: &Path) -> Result<()> {
    eprintln!("== dependency direction (ps2-* never depends on sly-*)");
    let output = Command::new(cargo_bin())
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--no-deps", "--locked"])
        .output()
        .context("running cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("cargo metadata output")?;
    let mut violations = Vec::new();
    for package in metadata["packages"].as_array().into_iter().flatten() {
        let name = package["name"].as_str().unwrap_or_default();
        if !name.starts_with("ps2-") {
            continue;
        }
        for dep in package["dependencies"].as_array().into_iter().flatten() {
            let dep_name = dep["name"].as_str().unwrap_or_default();
            if dep_name.starts_with("sly-") {
                violations.push(format!("{name} -> {dep_name}"));
            }
        }
    }
    if !violations.is_empty() {
        bail!(
            "game-agnostic crates depend on Sly crates: {}",
            violations.join(", ")
        );
    }
    Ok(())
}

/// The ps2dev toolchain image the conformance program is built with.
const PS2DEV_IMAGE: &str = "docker.io/ps2dev/ps2dev:v2.0.0";

fn conformance_build(root: &Path) -> Result<()> {
    let dir = root.join("conformance");
    eprintln!("== conformance.elf ({PS2DEV_IMAGE})");
    let volume = format!("{}:/w:Z", dir.display());
    run(
        Command::new("podman")
            .args(["run", "--rm", "-v", &volume, "-w", "/w", PS2DEV_IMAGE])
            .args(["sh", "./build.sh"]),
        "podman",
    )
}

fn conformance_record(root: &Path) -> Result<()> {
    let dir = root.join("conformance");
    for mode in ["recompiler", "interpreter"] {
        eprintln!("== PCSX2 ({mode})");
        run(
            Command::new(dir.join("record-pcsx2.sh"))
                .arg(dir.join(format!("pcsx2-{mode}.txt")))
                .arg(mode),
            "record-pcsx2.sh",
        )?;
    }
    Ok(())
}

fn run(command: &mut Command, name: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("running {name}"))?;
    if !status.success() {
        bail!("{name} failed");
    }
    Ok(())
}

fn cargo(root: &Path, step: &str, args: &[&str], env: &[(&str, &str)]) -> Result<()> {
    eprintln!("== {step}: cargo {}", args.join(" "));
    let status = Command::new(cargo_bin())
        .current_dir(root)
        .args(args)
        .envs(env.iter().copied())
        .status()
        .with_context(|| format!("running cargo {}", args.join(" ")))?;
    if !status.success() {
        bail!("{step} failed");
    }
    Ok(())
}

fn has_cargo_subcommand(root: &Path, name: &str) -> bool {
    Command::new(cargo_bin())
        .current_dir(root)
        .args([name, "--version"])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn cargo_bin() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

/// Writes `games/sly1/symbols.tsv` from the decompilation's
/// `config/symbol_addrs.txt`: every uncommented `name = 0xADDR; // type:func`
/// line, first name per address, sorted by address.
fn import_sly1_symbols(root: &Path, source: &Path) -> Result<()> {
    let text =
        std::fs::read_to_string(source).with_context(|| format!("reading {}", source.display()))?;
    let mut symbols = std::collections::BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("//") || !line.contains("type:func") {
            continue;
        }
        let Some((name, rest)) = line.split_once('=') else {
            continue;
        };
        let address = rest.trim().split(';').next().unwrap_or_default().trim();
        let address = u32::from_str_radix(
            address.trim_start_matches("0x").trim_start_matches("0X"),
            16,
        )
        .with_context(|| format!("address in {line:?}"))?;
        symbols
            .entry(address)
            .or_insert_with(|| name.trim().to_owned());
    }
    let mut out = String::from(
        "# Sly 1 (SCUS_971.98, SHA-1 57dc305db57932ad3f1122966cdb695d2e62a47a) function symbols.\n\
         # From the Sly 1 decompilation, github.com/TheOnlyZac/sly1 config/symbol_addrs.txt (CC0-1.0);\n\
         # names from the May 2002 prototype's debug symbols. Generated by `cargo xtask import-sly1-symbols`.\n\
         # address\tmangled name\treadable name\n",
    );
    for (address, name) in &symbols {
        let _ = writeln!(out, "{address:08x}\t{name}\t{}", readable(name));
    }
    let path = root.join("games/sly1/symbols.tsv");
    std::fs::create_dir_all(path.parent().context("games/sly1")?)?;
    std::fs::write(&path, out)?;
    eprintln!(
        "== wrote {} function symbols to {}",
        symbols.len(),
        path.display()
    );
    Ok(())
}

/// A readable form of a GCC 2.x mangled name, enough to read a listing:
/// `Func__FiP4CRef` gives `Func`, `Method__6CFrameP6CFrame` gives
/// `CFrame::Method`, `Startup__10CSplotheapii` gives `CSplotheap::Startup`,
/// `Name__Q23Foo3Bar` gives `Foo::Bar::Name`. Operators, constructors and
/// unmangled names are kept as they are.
fn readable(mangled: &str) -> String {
    let Some(split) = mangled.get(1..).and_then(|s| s.find("__")).map(|i| i + 1) else {
        return mangled.to_owned();
    };
    let (base, rest) = (&mangled[..split], &mangled[split + 2..]);
    let mut class = Vec::new();
    let mut rest = rest;
    if let Some(nested) = rest.strip_prefix('Q') {
        let count: usize = nested.get(..1).and_then(|d| d.parse().ok()).unwrap_or(0);
        rest = nested.get(1..).unwrap_or_default().trim_start_matches('_');
        for _ in 0..count {
            let Some((name, after)) = length_prefixed(rest) else {
                break;
            };
            class.push(name);
            rest = after;
        }
    } else if let Some((name, _)) = length_prefixed(rest) {
        class.push(name);
    }
    if class.is_empty() {
        base.to_owned()
    } else {
        format!("{}::{base}", class.join("::"))
    }
}

/// Splits `<len><name>...` into `name` and the rest.
fn length_prefixed(text: &str) -> Option<(&str, &str)> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    let len: usize = text.get(..digits)?.parse().ok()?;
    let name = text.get(digits..digits + len)?;
    Some((name, &text[digits + len..]))
}
