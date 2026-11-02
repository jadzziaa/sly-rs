//! Reading PlayStation 2 disc images.
//!
//! PS2 games often keep their data outside the file system and find it by
//! sector number (all three Sly Cooper games do), so raw sector access by
//! logical block address (LBA) is the primary interface here. The ISO 9660
//! file system, `SYSTEM.CNF` and SHA-1 identification sit on top of it.
//!
//! A [`Disc`] reads through the [`ReadAt`] trait: a [`std::fs::File`] in
//! normal use, a byte slice in tests.

mod digest;
mod error;
mod iso9660;
mod source;
mod system_cnf;

use std::fs::File;
use std::path::Path;

pub use digest::Sha1Digest;
pub use error::DiscError;
pub use iso9660::{DirEntry, Volume};
pub use source::ReadAt;
pub use system_cnf::SystemCnf;

/// Size of a DVD-ROM sector (Mode 1 user data), in bytes.
pub const SECTOR_SIZE: usize = 2048;

/// [`SECTOR_SIZE`] as a `u64`, for offset arithmetic.
const SECTOR_SIZE_U64: u64 = SECTOR_SIZE as u64;

/// [`SECTOR_SIZE`] as a `u32`, for sector counts.
const SECTOR_SIZE_U32: u32 = 2048;
const _: () = assert!(SECTOR_SIZE_U32 as usize == SECTOR_SIZE);

/// Chunk size for streaming reads (hashing).
const STREAM_CHUNK: usize = 8 << 20;

/// A disc image opened for reading.
#[derive(Debug)]
pub struct Disc<R = File> {
    source: R,
    len: u64,
}

impl Disc<File> {
    /// Opens the disc image at `path`.
    ///
    /// # Errors
    ///
    /// [`DiscError::Open`] if the file cannot be opened or its size read.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DiscError> {
        let path = path.as_ref();
        let open_error = |source| DiscError::Open {
            path: path.to_path_buf(),
            source,
        };
        let file = File::open(path).map_err(open_error)?;
        let len = file.size().map_err(open_error)?;
        Ok(Self { source: file, len })
    }
}

impl<R: ReadAt> Disc<R> {
    /// Wraps any [`ReadAt`] source, such as an in-memory image.
    ///
    /// # Errors
    ///
    /// [`DiscError::Read`] if the source cannot report its size.
    pub fn from_source(source: R) -> Result<Self, DiscError> {
        let len = source.size().map_err(|source| DiscError::Read {
            offset: 0,
            len: 0,
            source,
        })?;
        Ok(Self { source, len })
    }

    /// Size of the image in bytes.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the image is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Number of whole sectors in the image.
    #[must_use]
    pub fn sector_count(&self) -> u32 {
        u32::try_from(self.len / SECTOR_SIZE_U64).unwrap_or(u32::MAX)
    }

    /// Reads `buf.len()` bytes starting at byte `offset`.
    ///
    /// # Errors
    ///
    /// [`DiscError::OutOfBounds`] if the range extends past the end of the
    /// image, [`DiscError::Read`] if the read fails.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), DiscError> {
        let len = buf.len();
        let end = offset.checked_add(len as u64);
        if end.is_none_or(|end| end > self.len) {
            return Err(DiscError::OutOfBounds {
                offset,
                len,
                image_len: self.len,
            });
        }
        self.source
            .read_exact_at(buf, offset)
            .map_err(|source| DiscError::Read {
                offset,
                len,
                source,
            })
    }

    /// Reads whole sectors starting at `lba` into `buf`, whose length must be
    /// a multiple of [`SECTOR_SIZE`].
    ///
    /// # Errors
    ///
    /// [`DiscError::UnalignedBuffer`] if `buf` is not a whole number of
    /// sectors, otherwise as [`Disc::read_at`].
    pub fn read_sectors(&self, lba: u32, buf: &mut [u8]) -> Result<(), DiscError> {
        if !buf.len().is_multiple_of(SECTOR_SIZE) {
            return Err(DiscError::UnalignedBuffer { len: buf.len() });
        }
        self.read_at(u64::from(lba) * SECTOR_SIZE_U64, buf)
    }

    /// Reads `count` sectors starting at `lba` into a new buffer.
    ///
    /// # Errors
    ///
    /// As [`Disc::read_sectors`].
    pub fn sectors(&self, lba: u32, count: u32) -> Result<Vec<u8>, DiscError> {
        let mut buf = vec![0; count as usize * SECTOR_SIZE];
        self.read_sectors(lba, &mut buf)?;
        Ok(buf)
    }

    /// SHA-1 of the whole image, read in large chunks. `progress` is called
    /// after each chunk with the number of bytes hashed so far.
    ///
    /// # Errors
    ///
    /// As [`Disc::read_at`].
    pub fn sha1(&self, mut progress: impl FnMut(u64)) -> Result<Sha1Digest, DiscError> {
        let mut hasher = digest::Hasher::new();
        let mut buf = vec![0; STREAM_CHUNK];
        let mut offset = 0;
        while offset < self.len {
            let n =
                usize::try_from(self.len - offset).map_or(buf.len(), |rest| rest.min(buf.len()));
            let chunk = &mut buf[..n];
            self.read_at(offset, chunk)?;
            hasher.update(chunk);
            offset += n as u64;
            progress(offset);
        }
        Ok(hasher.finish())
    }

    /// The ISO 9660 primary volume descriptor.
    ///
    /// # Errors
    ///
    /// [`DiscError::NotIso9660`] if sector 16 holds no primary volume
    /// descriptor, otherwise as [`Disc::read_at`].
    pub fn volume(&self) -> Result<Volume, DiscError> {
        let mut sector = [0; SECTOR_SIZE];
        self.read_sectors(iso9660::PVD_LBA, &mut sector)?;
        Volume::parse(&sector)
    }

    /// The entries of the directory `dir`, without `.` and `..`.
    ///
    /// # Errors
    ///
    /// [`DiscError::BadDirectory`] if a record is malformed, otherwise as
    /// [`Disc::read_at`].
    pub fn read_dir(&self, dir: &DirEntry) -> Result<Vec<DirEntry>, DiscError> {
        let data = self.sectors(dir.lba, dir.sector_count())?;
        iso9660::parse_directory(dir, &data)
    }

    /// The entries of the root directory.
    ///
    /// # Errors
    ///
    /// As [`Disc::volume`] and [`Disc::read_dir`].
    pub fn root(&self) -> Result<Vec<DirEntry>, DiscError> {
        self.read_dir(&self.volume()?.root)
    }

    /// Finds a file in the root directory by name, ignoring ASCII case and the
    /// `;1` version suffix.
    ///
    /// # Errors
    ///
    /// As [`Disc::root`].
    pub fn find(&self, name: &str) -> Result<Option<DirEntry>, DiscError> {
        let name = iso9660::strip_version(name);
        Ok(self
            .root()?
            .into_iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name)))
    }

    /// Reads the whole contents of a file.
    ///
    /// # Errors
    ///
    /// As [`Disc::read_at`].
    pub fn read_file(&self, entry: &DirEntry) -> Result<Vec<u8>, DiscError> {
        let mut buf = vec![0; entry.size as usize];
        self.read_at(u64::from(entry.lba) * SECTOR_SIZE_U64, &mut buf)?;
        Ok(buf)
    }

    /// Reads and parses `SYSTEM.CNF` from the root directory.
    ///
    /// # Errors
    ///
    /// [`DiscError::NotFound`] if there is no `SYSTEM.CNF`,
    /// [`DiscError::BadSystemCnf`] if it cannot be parsed, otherwise as
    /// [`Disc::root`].
    pub fn system_cnf(&self) -> Result<SystemCnf, DiscError> {
        let entry = self
            .find("SYSTEM.CNF")?
            .ok_or_else(|| DiscError::NotFound("SYSTEM.CNF".to_owned()))?;
        SystemCnf::parse(&String::from_utf8_lossy(&self.read_file(&entry)?))
    }
}

#[cfg(test)]
mod tests;
