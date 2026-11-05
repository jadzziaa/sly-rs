//! Memory cards for a run: a blank one or an image file, and saving an
//! image without leaving a half-written file behind.

use std::io;
use std::path::{Path, PathBuf};

use ps2_memcard::{Card, CardError};

/// A card given as `formatted`, for a blank card.
pub const FORMATTED: &str = "formatted";

/// Errors from loading a card.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    /// The image could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Read {
        /// The image's path.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// The image is not a card sly-rs can use.
    #[error("{}: {source}", path.display())]
    Card {
        /// The image's path.
        path: PathBuf,
        /// What is wrong with it.
        source: CardError,
    },
}

/// The card `spec` names: [`FORMATTED`], or an image's path, relative to
/// `base`.
///
/// # Errors
///
/// [`LoadError`] if the image cannot be read or is not an 8 MB card.
pub fn load(spec: &str, base: &Path) -> Result<Card, LoadError> {
    if spec == FORMATTED {
        return Ok(Card::formatted());
    }
    open(&base.join(spec))
}

/// The card image at `path`.
///
/// # Errors
///
/// [`LoadError`] if the image cannot be read or is not an 8 MB card.
pub fn open(path: &Path) -> Result<Card, LoadError> {
    let image = std::fs::read(path).map_err(|source| LoadError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    Card::new(image).map_err(|source| LoadError::Card {
        path: path.to_path_buf(),
        source,
    })
}

/// Writes a card's image to `path`: to a file beside it first, then renamed
/// over it, so that an interrupted save leaves the old image whole.
///
/// # Errors
///
/// If the file cannot be written.
pub fn save(card: &Card, path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    std::fs::write(&partial, card.image())?;
    std::fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_card_loads_back() {
        let dir = std::env::temp_dir().join(format!("sly-rs-card-{}", std::process::id()));
        let path = dir.join("cards").join("sly1.ps2");
        let card = Card::formatted();
        save(&card, &path).unwrap_or_else(|e| panic!("{e}"));
        let back = load("cards/sly1.ps2", &dir).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.image(), card.image());
        assert!(load("missing.ps2", &dir).is_err());
        assert_eq!(
            load(FORMATTED, Path::new("/nowhere"))
                .map(|c| c.image().len())
                .ok(),
            Some(ps2_memcard::IMAGE_SIZE)
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
