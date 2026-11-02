//! `sly-rs info`: identify discs and print what they contain, the facts of
//! docs/research/disc-survey.md as a reproducible command.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ps2_disc::{DirEntry, Disc, SECTOR_SIZE};
use sly_common::{Build, Config, Game, Identification, identify};

use crate::elf;

pub(crate) fn run(args: &[String], verify: bool, config_path: Option<&Path>) -> Result<()> {
    let config = match config_path {
        Some(path) => {
            let mut config = Config::from_file(path)?;
            config.apply_env(|key| std::env::var_os(key));
            config
        }
        None => Config::load()?,
    };
    let targets = targets(args, &config)?;
    let mut out = io::stdout().lock();
    let mut failed = 0;
    for path in &targets {
        if let Err(error) = report(&mut out, path, verify) {
            tracing::error!("{}: {error:#}", path.display());
            failed += 1;
        }
    }
    if failed > 0 {
        bail!("{failed} of {} discs could not be read", targets.len());
    }
    Ok(())
}

/// The images to report on: the arguments (paths or game keys), or every
/// configured disc.
fn targets(args: &[String], config: &Config) -> Result<Vec<PathBuf>> {
    let configured = |game: Game| {
        config.disc(game).map(Path::to_path_buf).with_context(|| {
            format!(
                "no disc configured for {game} (sly-rs.toml or SLY_RS_DISC_{})",
                game.key().to_ascii_uppercase()
            )
        })
    };
    if args.is_empty() {
        let all: Vec<_> = Game::ALL
            .into_iter()
            .filter_map(|game| config.disc(game))
            .map(Path::to_path_buf)
            .collect();
        if all.is_empty() {
            bail!(
                "no disc given and none configured: copy sly-rs.example.toml to sly-rs.toml and edit it"
            );
        }
        return Ok(all);
    }
    args.iter()
        .map(|arg| Game::from_key(arg).map_or_else(|| Ok(PathBuf::from(arg)), configured))
        .collect()
}

fn report(out: &mut impl Write, path: &Path, verify: bool) -> Result<()> {
    let disc = Disc::open(path)?;
    let id = identify(&disc)?;
    let root = disc.root()?;
    write_identity(out, path, &disc, &id)?;
    write_files(out, &root, disc.volume()?.sectors)?;
    let summary = elf::summarize(&id.elf)?;
    write_executable(out, &summary)?;
    write_iop_modules(out, &disc, &root, &summary)?;
    if verify {
        verify_image(out, &disc, id.build)?;
    }
    writeln!(out)?;
    Ok(())
}

fn write_identity(
    out: &mut impl Write,
    path: &Path,
    disc: &Disc,
    id: &Identification,
) -> Result<()> {
    match id.build {
        Some(build) => writeln!(out, "{} - {}", build.title, build.serial)?,
        None => writeln!(out, "Unknown disc: not a build sly-rs supports")?,
    }
    writeln!(out, "  image     {}", path.display())?;
    let size_note = match id.build {
        Some(_) if id.size_matches => "size as expected",
        Some(_) => "SIZE DIFFERS from the known build",
        None => "",
    };
    writeln!(
        out,
        "            {} bytes, {} sectors; {size_note}",
        group(disc.len()),
        group(u64::from(disc.sector_count()))
    )?;
    let cnf = disc.system_cnf()?;
    writeln!(
        out,
        "  boot      {} (VER {}, VMODE {})",
        cnf.boot2,
        cnf.ver.as_deref().unwrap_or("?"),
        cnf.vmode.as_deref().unwrap_or("?")
    )?;
    let known = if id.build.is_some() {
        "a supported build"
    } else {
        "UNKNOWN"
    };
    writeln!(
        out,
        "  ELF       {}, {} bytes, SHA-1 {} ({known})",
        id.boot_elf,
        group(id.elf.len() as u64),
        id.elf_sha1
    )?;
    Ok(())
}

/// The root directory, and how much of the volume no file covers (where
/// these games keep their data).
fn write_files(out: &mut impl Write, root: &[DirEntry], volume_sectors: u32) -> Result<()> {
    writeln!(out, "  files     {:>9}  {:>14}  name", "LBA", "bytes")?;
    for entry in root {
        writeln!(
            out,
            "            {:>9}  {:>14}  {}",
            entry.lba,
            group(u64::from(entry.size)),
            entry.name
        )?;
    }
    let in_files: u64 = root
        .iter()
        .map(|entry| u64::from(entry.sector_count()))
        .sum();
    let outside = u64::from(volume_sectors).saturating_sub(in_files);
    writeln!(
        out,
        "            {} sectors ({:.2} GiB) lie outside any file",
        group(outside),
        gib(outside * SECTOR_SIZE as u64)
    )?;
    Ok(())
}

fn write_executable(out: &mut impl Write, summary: &elf::Summary) -> Result<()> {
    writeln!(out, "  sections  entry {:#010x}", summary.entry)?;
    for section in &summary.sections {
        let note = if section.name == ".text" {
            format!(" ({} instructions)", group(section.size / 4))
        } else {
            String::new()
        };
        writeln!(
            out,
            "            {:#010x}  {:>14}  {}{note}",
            section.address,
            group(section.size),
            section.name
        )?;
    }
    writeln!(
        out,
        "            {} VU microcode overlay sections",
        summary.vu_overlays
    )?;
    Ok(())
}

/// IOP modules shipped as files (Sly 1) or linked into the ELF (Sly 2, 3).
fn write_iop_modules(
    out: &mut impl Write,
    disc: &Disc,
    root: &[DirEntry],
    summary: &elf::Summary,
) -> Result<()> {
    let on_disc: Vec<String> = root
        .iter()
        .filter(|entry| entry.name.to_ascii_uppercase().ends_with(".IRX"))
        .map(|entry| {
            let name = disc
                .read_file(entry)
                .ok()
                .and_then(|data| elf::irx_name(&data));
            format!("{} ({})", name.as_deref().unwrap_or("?"), entry.name)
        })
        .collect();
    if !on_disc.is_empty() {
        writeln!(out, "  IOP       on disc: {}", on_disc.join(", "))?;
    }
    if !summary.embedded_modules.is_empty() {
        writeln!(
            out,
            "  IOP       linked into the ELF: {}",
            summary.embedded_modules.join(", ")
        )?;
    }
    Ok(())
}

/// Hashes the whole image, showing progress on stderr.
fn verify_image(out: &mut impl Write, disc: &Disc, build: Option<&Build>) -> Result<()> {
    let total = disc.len().max(1);
    let mut err = io::stderr().lock();
    let digest = disc.sha1(|done| {
        // Progress is best-effort: a closed stderr must not stop the hash.
        let _ = write!(err, "\r  hashing   {:>3}%", done * 100 / total);
        let _ = err.flush();
    });
    let _ = writeln!(err);
    let digest = digest?;
    let verdict = match build {
        Some(build) if build.image_sha1 == digest => "matches the known image",
        Some(_) => "DIFFERS from the known image",
        None => "no known image to compare with",
    };
    writeln!(out, "  verify    image SHA-1 {digest} ({verdict})")?;
    Ok(())
}

/// `1234567` as `1,234,567`.
fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[allow(clippy::cast_precision_loss, reason = "a display value")]
fn gib(bytes: u64) -> f64 {
    bytes as f64 / f64::from(1u32 << 30)
}

#[cfg(test)]
mod tests {
    use super::group;

    #[test]
    fn groups_thousands() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1000), "1,000");
        assert_eq!(group(2_802_319_360), "2,802,319,360");
    }
}
