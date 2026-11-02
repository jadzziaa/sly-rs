use std::io;
use std::path::PathBuf;

/// Errors from reading a disc image.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DiscError {
    /// The image file could not be opened.
    #[error("cannot open {}: {source}", path.display())]
    Open {
        /// Path of the image.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// A read from the image failed.
    #[error("cannot read {len} bytes at offset {offset:#x}: {source}")]
    Read {
        /// Byte offset of the read.
        offset: u64,
        /// Length of the read.
        len: usize,
        /// The underlying error.
        source: io::Error,
    },
    /// A read extends past the end of the image.
    #[error(
        "{len} bytes at offset {offset:#x} extend past the end of the image ({image_len} bytes)"
    )]
    OutOfBounds {
        /// Byte offset of the read.
        offset: u64,
        /// Length of the read.
        len: usize,
        /// Size of the image.
        image_len: u64,
    },
    /// A sector read was given a buffer that is not a whole number of sectors.
    #[error("buffer of {len} bytes is not a whole number of 2048-byte sectors")]
    UnalignedBuffer {
        /// Length of the buffer.
        len: usize,
    },
    /// The image has no ISO 9660 primary volume descriptor.
    #[error("not an ISO 9660 image: {0}")]
    NotIso9660(&'static str),
    /// A directory record is malformed.
    #[error("malformed directory record in sector {lba}: {reason}")]
    BadDirectory {
        /// Sector holding the record.
        lba: u32,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// `SYSTEM.CNF` could not be parsed.
    #[error("SYSTEM.CNF: {0}")]
    BadSystemCnf(String),
    /// A file was not found.
    #[error("no file named {0}")]
    NotFound(String),
}
