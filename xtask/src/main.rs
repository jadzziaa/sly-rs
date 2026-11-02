//! Project automation, run as `cargo xtask <command>`:
//!
//! - `ci`: everything a change must pass: formatting, clippy, rustdoc, tests,
//!   `cargo deny` (when installed) and `check-deps`.
//! - `test-full`: the ignored tests, in release mode: those that need the
//!   user's discs and the exhaustive ones (every 32-bit word).
//! - `check-deps`: no `ps2-*` crate may depend on a `sly-*` crate (goal G5
//!   of docs/plan.md).

// Step headers and results go to stderr; this is a developer tool.
#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let root = workspace_root();
    let task = std::env::args().nth(1).unwrap_or_default();
    match task.as_str() {
        "ci" => ci(&root),
        "test-full" => test_full(&root),
        "check-deps" => check_deps(&root),
        _ => {
            eprintln!("usage: cargo xtask <ci | test-full | check-deps>");
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
