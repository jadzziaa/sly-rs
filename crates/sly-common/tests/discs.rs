//! Tests against the user's own discs. Ignored by default (the repository
//! holds no game data); `cargo xtask test-full` runs them with the discs
//! named in `sly-rs.toml` or `SLY_RS_DISC_*`.

#![allow(
    clippy::expect_used,
    reason = "test helpers fail the test with a message"
)]

use std::path::PathBuf;

use ps2_disc::Disc;
use sly_common::{Build, Config, Game, identify};

fn disc_path(game: Game) -> PathBuf {
    let config = Config::load().expect("configuration");
    let Some(path) = config.disc(game) else {
        panic!(
            "no disc configured for {game}: set [discs] {game} in sly-rs.toml or SLY_RS_DISC_{}",
            game.key().to_ascii_uppercase()
        )
    };
    PathBuf::from(path)
}

#[test]
#[ignore = "needs the user's discs: cargo xtask test-full"]
fn each_disc_is_its_known_build() {
    for game in Game::ALL {
        let disc = Disc::open(disc_path(game)).unwrap();
        let id = identify(&disc).unwrap();
        assert_eq!(
            id.build,
            Some(Build::of(game)),
            "{game}: boot ELF {} {}",
            id.boot_elf,
            id.elf_sha1
        );
        assert!(id.size_matches, "{game}: image size {}", disc.len());
        assert_eq!(id.boot_elf, Build::of(game).boot_elf);
    }
}

#[test]
#[ignore = "needs the user's discs: cargo xtask test-full"]
fn each_image_hash_is_known() {
    for game in Game::ALL {
        let disc = Disc::open(disc_path(game)).unwrap();
        assert_eq!(
            disc.sha1(|_| {}).unwrap(),
            Build::of(game).image_sha1,
            "{game}"
        );
    }
}
