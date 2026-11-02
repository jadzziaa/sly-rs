//! The parts of ISO 9660 a PS2 disc needs: the primary volume descriptor and
//! directory records. Joliet, Rock Ridge and UDF are not read.

use crate::{DiscError, SECTOR_SIZE, SECTOR_SIZE_U32};

/// Sector of the primary volume descriptor.
pub(crate) const PVD_LBA: u32 = 16;

/// Smallest valid directory record (33 fixed bytes plus a 1-byte name).
const MIN_RECORD: usize = 34;

/// The primary volume descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// System identifier (`PLAYSTATION` on PS2 discs).
    pub system_id: String,
    /// Volume identifier (often empty on PS2 discs).
    pub volume_id: String,
    /// Volume size in sectors.
    pub sectors: u32,
    /// The root directory.
    pub root: DirEntry,
}

/// A directory record: a file or a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    /// Name without the `;1` version suffix.
    pub name: String,
    /// First sector of the extent.
    pub lba: u32,
    /// Size in bytes.
    pub size: u32,
    /// Whether this is a directory.
    pub is_dir: bool,
}

impl DirEntry {
    /// Number of sectors the extent occupies.
    #[must_use]
    pub fn sector_count(&self) -> u32 {
        self.size.div_ceil(SECTOR_SIZE_U32)
    }

    /// First sector after the extent.
    #[must_use]
    pub fn end_lba(&self) -> u32 {
        self.lba.saturating_add(self.sector_count())
    }
}

impl Volume {
    /// Parses a primary volume descriptor sector.
    pub(crate) fn parse(sector: &[u8; SECTOR_SIZE]) -> Result<Self, DiscError> {
        if sector[0] != 1 || &sector[1..6] != b"CD001" || sector[6] != 1 {
            return Err(DiscError::NotIso9660(
                "no primary volume descriptor in sector 16",
            ));
        }
        if le_u16(sector, 128) as usize != SECTOR_SIZE {
            return Err(DiscError::NotIso9660("logical block size is not 2048"));
        }
        let root =
            Record::parse(&sector[156..156 + MIN_RECORD]).ok_or(DiscError::BadDirectory {
                lba: PVD_LBA,
                reason: "root directory record",
            })?;
        Ok(Self {
            system_id: text(&sector[8..40]),
            volume_id: text(&sector[40..72]),
            sectors: le_u32(sector, 80),
            root: root.entry(String::new()),
        })
    }
}

/// Parses the records of directory `dir` from its sectors `data`.
pub(crate) fn parse_directory(dir: &DirEntry, data: &[u8]) -> Result<Vec<DirEntry>, DiscError> {
    let mut entries = Vec::new();
    let used = (dir.size as usize).min(data.len());
    for (index, sector) in data[..used.next_multiple_of(SECTOR_SIZE).min(data.len())]
        .chunks(SECTOR_SIZE)
        .enumerate()
    {
        let lba = dir
            .lba
            .saturating_add(u32::try_from(index).unwrap_or(u32::MAX));
        let bad = |reason| DiscError::BadDirectory { lba, reason };
        let mut at = 0;
        // Records never cross a sector boundary; a zero length byte pads to its end.
        while at < sector.len() && sector[at] != 0 {
            let len = sector[at] as usize;
            let record = sector
                .get(at..at + len)
                .ok_or(bad("record crosses the sector end"))?;
            let record = Record::parse(record).ok_or(bad("record too short"))?;
            if !record.is_self_or_parent() {
                let name = strip_version(&String::from_utf8_lossy(record.name)).to_owned();
                entries.push(record.entry(name));
            }
            at += len;
        }
    }
    Ok(entries)
}

/// The fields of one directory record.
struct Record<'a> {
    lba: u32,
    size: u32,
    is_dir: bool,
    name: &'a [u8],
}

impl<'a> Record<'a> {
    /// Parses one record; `None` if it is too short for its name.
    fn parse(record: &'a [u8]) -> Option<Self> {
        if record.len() < MIN_RECORD {
            return None;
        }
        let name_len = record[32] as usize;
        Some(Self {
            lba: le_u32(record, 2),
            size: le_u32(record, 10),
            is_dir: record[25] & 0x02 != 0,
            name: record.get(33..33 + name_len)?,
        })
    }

    /// Whether this is the `.` or `..` entry of a directory.
    fn is_self_or_parent(&self) -> bool {
        self.name == [0] || self.name == [1]
    }

    fn entry(&self, name: String) -> DirEntry {
        DirEntry {
            name,
            lba: self.lba,
            size: self.size,
            is_dir: self.is_dir,
        }
    }
}

/// Removes the `;N` version suffix of an ISO 9660 file name.
pub(crate) fn strip_version(name: &str) -> &str {
    name.rsplit_once(';').map_or(name, |(base, _)| base)
}

/// A space-padded identifier field.
fn text(field: &[u8]) -> String {
    String::from_utf8_lossy(field).trim_end().to_owned()
}

fn le_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
