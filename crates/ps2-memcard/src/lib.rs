//! PlayStation 2 memory card images, as PCSX2 keeps them (`.ps2`): an
//! 8 MB card's 16,384 pages of 512 bytes, each followed by 16 spare bytes
//! holding its error-correcting code.
//!
//! The card's file system: page 0 holds the superblock; clusters are two
//! pages; the indirect FAT (cluster 8) lists the FAT's 32 clusters (9 to
//! 40); allocatable clusters start at cluster 41 and are numbered from
//! there, the root directory first; the last two erase blocks are kept
//! for backups. Layout and values follow PCSX2's `MemoryCardFolder` and
//! `MemoryCardFile` (read for facts) and mymc's formatter, which describe
//! what Sony's formatter writes.
//!
//! [`Files`] is the card's file system as the IOP's card manager (`mcman`)
//! keeps it: what the game's `libmc` calls open, read, write, list and
//! delete.

use std::fmt;

mod files;

pub use files::{Files, Time, info, open, result};

/// Bytes of data in a page.
pub const PAGE_SIZE: usize = 512;
/// Spare bytes after each page: four 3-byte ECCs, then 0xFF.
pub const SPARE_SIZE: usize = 16;
/// A page as the image stores it.
pub const RAW_PAGE_SIZE: usize = PAGE_SIZE + SPARE_SIZE;
/// Pages on an 8 MB card.
pub const PAGES: usize = 16_384;
/// Bytes in an image with ECC (8,650,752).
pub const IMAGE_SIZE: usize = PAGES * RAW_PAGE_SIZE;

pub(crate) const PAGES_PER_CLUSTER: usize = 2;
const CLUSTERS_PER_BLOCK: u32 = 8;
pub(crate) const CLUSTER_SIZE: usize = PAGE_SIZE * PAGES_PER_CLUSTER;
const PAGES_PER_BLOCK: usize = 16;
const CLUSTERS: u32 = 8192;
/// FAT entries per cluster.
const PER_CLUSTER: u32 = 256;
/// The indirect FAT's cluster: the first after the superblock's block.
const INDIRECT_FAT: u32 = 8;
const FAT_CLUSTERS: u32 = CLUSTERS / PER_CLUSTER;
/// The first allocatable cluster.
const ALLOC_OFFSET: u32 = INDIRECT_FAT + 1 + FAT_CLUSTERS;
/// Allocatable clusters: up to the backup blocks.
const ALLOC_END: u32 = BACKUP_BLOCK2 * CLUSTERS_PER_BLOCK - ALLOC_OFFSET;
const BACKUP_BLOCK1: u32 = 1023;
const BACKUP_BLOCK2: u32 = 1022;

const MAGIC: &[u8; 28] = b"Sony PS2 Memory Card Format ";
const VERSION: &[u8; 12] = b"1.2.0.0\0\0\0\0\0";
/// Card type 2: a PS2 card.
const CARD_TYPE: u8 = 2;
/// Uses ECC (0x01), has a bad block list (0x08), and two flags every
/// formatted card carries (0x02, 0x20).
const CARD_FLAGS: u8 = 0x2B;

/// FAT entries: a cluster in use carries this bit; the last of a chain is
/// all ones; a free one is all ones without it.
pub(crate) const IN_USE: u32 = 0x8000_0000;
pub(crate) const CHAIN_END: u32 = 0xFFFF_FFFF;
const FREE: u32 = 0x7FFF_FFFF;

/// Directory entry modes.
pub mod mode {
    /// Readable.
    pub const READ: u16 = 0x0001;
    /// Writable.
    pub const WRITE: u16 = 0x0002;
    /// Executable.
    pub const EXECUTE: u16 = 0x0004;
    /// A file.
    pub const FILE: u16 = 0x0010;
    /// A directory.
    pub const DIR: u16 = 0x0020;
    /// Set on every entry Sony's library writes.
    pub const UNKNOWN_0400: u16 = 0x0400;
    /// Hidden (the root's `..`).
    pub const HIDDEN: u16 = 0x2000;
    /// The entry exists.
    pub const EXISTS: u16 = 0x8000;
}

/// Bytes in a directory entry: two to a page.
const ENTRY_SIZE: usize = 512;

/// Each byte's column parity mask (bits 0-2 and 4-6) and, in bit 7, the
/// parity of the byte itself.
const ECC_TABLE: [u8; 256] = {
    const fn odd(b: usize, mask: usize) -> u8 {
        ((b & mask).count_ones() & 1) as u8
    }
    let mut table = [0u8; 256];
    let mut b = 0;
    while b < 256 {
        table[b] = odd(b, 0x55)
            | odd(b, 0x33) << 1
            | odd(b, 0x0F) << 2
            | odd(b, 0xAA) << 4
            | odd(b, 0xCC) << 5
            | odd(b, 0xF0) << 6
            | odd(b, 0xFF) << 7;
        b += 1;
    }
    table
};

/// The 3-byte error-correcting code of 128 bytes of a page: column
/// parities, then the line parities of the odd bytes' indices and of their
/// complements.
#[must_use]
pub fn ecc(chunk: &[u8; 128]) -> [u8; 3] {
    let mut column = 0u8;
    let mut line_complement = 0u8;
    let mut line = 0u8;
    for (i, &b) in (0u8..).zip(chunk) {
        let c = ECC_TABLE[usize::from(b)];
        column ^= c;
        if c & 0x80 != 0 {
            line_complement ^= !i;
            line ^= i;
        }
    }
    [!column & 0x77, !line_complement & 0x7F, !line & 0x7F]
}

/// A page's spare bytes: the ECCs of its four 128-byte quarters, then 0xFF.
#[must_use]
pub fn spare(page: &[u8; PAGE_SIZE]) -> [u8; SPARE_SIZE] {
    let mut spare = [0xFF; SPARE_SIZE];
    for (k, quarter) in page.as_chunks::<128>().0.iter().enumerate() {
        spare[3 * k..3 * k + 3].copy_from_slice(&ecc(quarter));
    }
    spare
}

/// What is wrong with an image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardError {
    /// Not the size of an 8 MB card, with or without ECC.
    Size(usize),
    /// Not formatted: no superblock.
    Unformatted,
    /// Formatted with a geometry other than an 8 MB card's.
    Geometry,
    /// A page whose data does not match its ECC.
    Ecc(usize),
}

impl fmt::Display for CardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Size(size) => write!(
                f,
                "{size} bytes is not an 8 MB memory card image ({IMAGE_SIZE} bytes, or {} without ECC)",
                PAGES * PAGE_SIZE
            ),
            Self::Unformatted => write!(f, "the memory card is not formatted"),
            Self::Geometry => write!(f, "the memory card is not formatted as an 8 MB card"),
            Self::Ecc(page) => write!(f, "page {page} of the memory card does not match its ECC"),
        }
    }
}

impl std::error::Error for CardError {}

/// A directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// [`mode`] bits.
    pub mode: u16,
    /// Bytes in a file; entries in a directory.
    pub length: u32,
    /// The first cluster (allocatable numbering).
    pub cluster: u32,
    /// The name.
    pub name: String,
}

/// An 8 MB memory card image with ECC.
#[derive(Clone)]
pub struct Card {
    image: Vec<u8>,
}

impl Card {
    /// A freshly formatted card: an empty root directory, every other
    /// allocatable cluster free, unwritten pages erased (0xFF).
    #[must_use]
    pub fn formatted() -> Self {
        let mut pages = vec![[0xFFu8; PAGE_SIZE]; PAGES];

        let sb = &mut pages[0];
        sb[..0x28].copy_from_slice(&[MAGIC.as_slice(), VERSION.as_slice()].concat());
        let fields: [(usize, &[u8]); 10] = [
            (0x28, &u16::try_from(PAGE_SIZE).unwrap_or(0).to_le_bytes()),
            (
                0x2A,
                &u16::try_from(PAGES_PER_CLUSTER).unwrap_or(0).to_le_bytes(),
            ),
            (
                0x2C,
                &u16::try_from(PAGES_PER_BLOCK).unwrap_or(0).to_le_bytes(),
            ),
            (0x2E, &0xFF00u16.to_le_bytes()),
            (0x30, &CLUSTERS.to_le_bytes()),
            (0x34, &ALLOC_OFFSET.to_le_bytes()),
            (0x38, &ALLOC_END.to_le_bytes()),
            (0x3C, &0u32.to_le_bytes()),
            (0x40, &BACKUP_BLOCK1.to_le_bytes()),
            (0x44, &BACKUP_BLOCK2.to_le_bytes()),
        ];
        for (at, bytes) in fields {
            sb[at..at + bytes.len()].copy_from_slice(bytes);
        }
        sb[0x48..0x50].fill(0);
        // The indirect FAT clusters, then zeros; no bad blocks.
        sb[0x50..0xD0].fill(0);
        sb[0x50..0x54].copy_from_slice(&INDIRECT_FAT.to_le_bytes());
        sb[0xD0..0x150].fill(0xFF);
        sb[0x150] = CARD_TYPE;
        sb[0x151] = CARD_FLAGS;

        let mut write_cluster = |cluster: u32, words: &[u32]| {
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            for (k, page) in bytes.chunks(PAGE_SIZE).enumerate() {
                let p = &mut pages[cluster as usize * PAGES_PER_CLUSTER + k];
                p[..page.len()].copy_from_slice(page);
            }
        };
        let indirect: Vec<u32> = (0..FAT_CLUSTERS).map(|k| INDIRECT_FAT + 1 + k).collect();
        write_cluster(INDIRECT_FAT, &indirect);
        let fat: Vec<u32> = (0..CLUSTERS)
            .map(|c| match c {
                0 => CHAIN_END,
                c if c < ALLOC_END => FREE,
                _ => CHAIN_END,
            })
            .collect();
        for (k, words) in (0u32..).zip(fat.chunks(PER_CLUSTER as usize)) {
            write_cluster(INDIRECT_FAT + 1 + k, words);
        }

        let root = ALLOC_OFFSET as usize * PAGES_PER_CLUSTER;
        pages[root] = entry(
            mode::READ
                | mode::WRITE
                | mode::EXECUTE
                | mode::DIR
                | mode::UNKNOWN_0400
                | mode::EXISTS,
            2,
            ".",
        );
        pages[root + 1] = entry(
            mode::WRITE
                | mode::EXECUTE
                | mode::DIR
                | mode::UNKNOWN_0400
                | mode::HIDDEN
                | mode::EXISTS,
            0,
            "..",
        );

        let mut image = Vec::with_capacity(IMAGE_SIZE);
        for page in &pages {
            image.extend_from_slice(page);
            image.extend_from_slice(&spare(page));
        }
        Self { image }
    }

    /// Reads an image: 8 MB with ECC, or without (PCSX2 accepts both; the
    /// ECC is added). It must be formatted as an 8 MB card, and every page
    /// must match its ECC or be erased.
    ///
    /// # Errors
    ///
    /// [`CardError`] says what is wrong with it.
    pub fn new(image: Vec<u8>) -> Result<Self, CardError> {
        let image = match image.len() {
            IMAGE_SIZE => image,
            n if n == PAGES * PAGE_SIZE => image
                .as_chunks::<PAGE_SIZE>()
                .0
                .iter()
                .flat_map(|page| [page.as_slice(), &spare(page)].concat())
                .collect(),
            n => return Err(CardError::Size(n)),
        };
        let card = Self { image };
        for p in 0..PAGES {
            let raw = &card.image[p * RAW_PAGE_SIZE..][..RAW_PAGE_SIZE];
            // An erased page (all ones, spare included) has no ECC: the
            // card manager leaves its backup blocks so.
            let erased = raw.iter().all(|&b| b == 0xFF);
            if !erased && raw[PAGE_SIZE..][..12] != spare(card.page(p))[..12] {
                return Err(CardError::Ecc(p));
            }
        }
        let sb = card.page(0);
        if &sb[..28] != MAGIC {
            return Err(CardError::Unformatted);
        }
        if usize::from(card.u16_at(0, 0x28)) != PAGE_SIZE
            || usize::from(card.u16_at(0, 0x2A)) != PAGES_PER_CLUSTER
            || card.u32_at(0, 0x30) != CLUSTERS
            || card.u32_at(0, 0x34) != ALLOC_OFFSET
        {
            return Err(CardError::Geometry);
        }
        Ok(card)
    }

    /// The image, with ECC.
    #[must_use]
    pub fn image(&self) -> &[u8] {
        &self.image
    }

    pub(crate) fn page(&self, page: usize) -> &[u8; PAGE_SIZE] {
        self.image[page * RAW_PAGE_SIZE..][..PAGE_SIZE]
            .try_into()
            .unwrap_or(&[0xFF; PAGE_SIZE])
    }

    /// Replaces a page's data, and its ECC with it. The spare's last four
    /// bytes are zeros, as the card manager writes them.
    pub(crate) fn set_page(&mut self, page: usize, data: &[u8; PAGE_SIZE]) {
        let at = page * RAW_PAGE_SIZE;
        if let Some(raw) = self.image.get_mut(at..at + RAW_PAGE_SIZE) {
            raw[..PAGE_SIZE].copy_from_slice(data);
            raw[PAGE_SIZE..PAGE_SIZE + 12].copy_from_slice(&spare(data)[..12]);
            raw[PAGE_SIZE + 12..].fill(0);
        }
    }

    fn u16_at(&self, page: usize, at: usize) -> u16 {
        let p = self.page(page);
        u16::from_le_bytes([p[at], p[at + 1]])
    }

    fn u32_at(&self, page: usize, at: usize) -> u32 {
        let p = self.page(page);
        u32::from_le_bytes([p[at], p[at + 1], p[at + 2], p[at + 3]])
    }

    /// A word of a (physical) cluster.
    fn cluster_word(&self, cluster: u32, index: u32) -> u32 {
        let byte = index as usize * 4;
        self.u32_at(
            cluster as usize * PAGES_PER_CLUSTER + byte / PAGE_SIZE,
            byte % PAGE_SIZE,
        )
    }

    /// The (physical) cluster of the FAT that holds an allocatable
    /// cluster's entry.
    fn fat_cluster(&self, cluster: u32) -> u32 {
        let fat_index = cluster / PER_CLUSTER;
        let indirect = self.u32_at(0, 0x50 + 4 * (fat_index / PER_CLUSTER) as usize);
        self.cluster_word(indirect, fat_index % PER_CLUSTER)
    }

    /// The FAT entry of an allocatable cluster.
    #[must_use]
    pub fn fat(&self, cluster: u32) -> u32 {
        self.cluster_word(self.fat_cluster(cluster), cluster % PER_CLUSTER)
    }

    /// Sets the FAT entry of an allocatable cluster.
    pub(crate) fn set_fat(&mut self, cluster: u32, value: u32) {
        let byte = (cluster % PER_CLUSTER) as usize * 4;
        let page = self.fat_cluster(cluster) as usize * PAGES_PER_CLUSTER + byte / PAGE_SIZE;
        let mut data = *self.page(page);
        data[byte % PAGE_SIZE..][..4].copy_from_slice(&value.to_le_bytes());
        self.set_page(page, &data);
    }

    /// Allocatable clusters: those below this are the file system's.
    pub(crate) fn alloc_end(&self) -> u32 {
        self.u32_at(0, 0x38)
    }

    /// The first page of an allocatable cluster.
    pub(crate) fn cluster_page(cluster: u32) -> usize {
        (ALLOC_OFFSET + cluster) as usize * PAGES_PER_CLUSTER
    }

    /// The root directory's first cluster.
    pub(crate) fn root_cluster(&self) -> u32 {
        self.u32_at(0, 0x3C)
    }

    /// Allocatable clusters not in use.
    #[must_use]
    pub fn free_clusters(&self) -> u32 {
        let end = self.alloc_end();
        u32::try_from((0..end).filter(|&c| self.fat(c) & IN_USE == 0).count()).unwrap_or(0)
    }

    /// The entries of the directory starting at an allocatable cluster,
    /// `.` and `..` included.
    fn directory(&self, first: u32) -> Vec<Entry> {
        let mut entries = Vec::new();
        let mut cluster = first;
        let mut count = None;
        let per_cluster = CLUSTER_SIZE / ENTRY_SIZE;
        while count.is_none_or(|n| entries.len() < n) && cluster != CHAIN_END & !IN_USE {
            for k in 0..per_cluster {
                let page = (ALLOC_OFFSET + cluster) as usize * PAGES_PER_CLUSTER + k;
                let raw = self.page(page);
                let name = raw[0x40..0x60]
                    .split(|&b| b == 0)
                    .next()
                    .unwrap_or_default();
                let e = Entry {
                    mode: self.u16_at(page, 0),
                    length: self.u32_at(page, 4),
                    cluster: self.u32_at(page, 0x10),
                    name: String::from_utf8_lossy(name).into_owned(),
                };
                count.get_or_insert(e.length as usize);
                entries.push(e);
                if count.is_some_and(|n| entries.len() >= n) {
                    break;
                }
            }
            let next = self.fat(cluster);
            if next & IN_USE == 0 {
                break;
            }
            cluster = next & !IN_USE;
        }
        entries
    }

    /// The root directory's entries, `.` and `..` included.
    #[must_use]
    pub fn root(&self) -> Vec<Entry> {
        self.directory(self.u32_at(0, 0x3C))
    }
}

/// A directory entry's page: no times (as PCSX2 formats), no cluster.
fn entry(mode: u16, length: u32, name: &str) -> [u8; PAGE_SIZE] {
    let mut e = [0u8; PAGE_SIZE];
    e[0..2].copy_from_slice(&mode.to_le_bytes());
    e[4..8].copy_from_slice(&length.to_le_bytes());
    e[0x40..0x40 + name.len()].copy_from_slice(name.as_bytes());
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecc_table_matches_pcsx2() {
        // Entries of the table in PCSX2's FolderMemoryCard::CalculateECC.
        for (b, c) in [
            (0x00, 0x00),
            (0x01, 0x87),
            (0x02, 0x96),
            (0x03, 0x11),
            (0x04, 0xA5),
            (0x10, 0xC3),
            (0x20, 0xD2),
            (0x57, 0x96),
            (0x80, 0xF0),
            (0xFF, 0x00),
        ] {
            assert_eq!(ECC_TABLE[b], c, "{b:#x}");
        }
    }

    #[test]
    fn ecc_of_known_pages() {
        // Erased (or zeroed) data.
        assert_eq!(ecc(&[0xFF; 128]), [0x77, 0x7F, 0x7F]);
        assert_eq!(ecc(&[0; 128]), [0x77, 0x7F, 0x7F]);
        // One set bit, bit 0 of byte 0: its column bits and line 0.
        let mut one = [0; 128];
        one[0] = 1;
        assert_eq!(ecc(&one), [0x70, 0x00, 0x7F]);
    }

    #[test]
    fn a_formatted_card_reads_back() {
        let card = Card::formatted();
        assert_eq!(card.image().len(), IMAGE_SIZE);
        assert_eq!(&card.image()[..28], MAGIC);
        let card = Card::new(card.image().to_vec()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(ALLOC_END, 8135);
        assert_eq!(card.fat(0), CHAIN_END);
        assert_eq!(card.fat(1), FREE);
        assert_eq!(card.free_clusters(), ALLOC_END - 1);
        let root = card.root();
        assert_eq!(
            root.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            [".", ".."]
        );
        assert_eq!(root[0].mode, 0x8427);
        assert_eq!(root[1].mode, 0xA426);
    }

    #[test]
    fn images_without_ecc_gain_it() {
        let card = Card::formatted();
        let plain: Vec<u8> = card
            .image()
            .as_chunks::<RAW_PAGE_SIZE>()
            .0
            .iter()
            .flat_map(|p| p[..PAGE_SIZE].to_vec())
            .collect();
        let back = Card::new(plain).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.image(), card.image());
    }

    #[test]
    fn damaged_or_blank_images_are_refused() {
        let mut image = Card::formatted().image().to_vec();
        image[5 * RAW_PAGE_SIZE] ^= 1;
        assert_eq!(Card::new(image).err(), Some(CardError::Ecc(5)));
        let blank: Vec<u8> = (0..PAGES)
            .flat_map(|_| [[0xFF; PAGE_SIZE].as_slice(), &spare(&[0xFF; PAGE_SIZE])].concat())
            .collect();
        assert_eq!(Card::new(blank).err(), Some(CardError::Unformatted));
        // Erased pages, spare included, are fine (the backup blocks).
        let mut image = Card::formatted().image().to_vec();
        image[16352 * RAW_PAGE_SIZE..16353 * RAW_PAGE_SIZE].fill(0xFF);
        assert!(Card::new(image).is_ok());
        assert_eq!(Card::new(vec![0; 100]).err(), Some(CardError::Size(100)));
    }
}
