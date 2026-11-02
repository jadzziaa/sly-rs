//! An executable as the EE sees it after loading: its allocated sections
//! placed at their addresses in one flat memory image.

use std::ops::Range;

use object::elf::{SHF_ALLOC, SHF_EXECINSTR, SHT_NOBITS};
use object::read::elf::{ElfFile32, SectionHeader};
use object::{Endianness, Object, ObjectSection};

use crate::AnalysisError;

/// What a section holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionKind {
    /// EE code (`.text`).
    Code,
    /// VU microcode packets (`.vutext`): executable in the ELF's eyes, data
    /// to the EE.
    VuCode,
    /// Initialised data.
    Data,
    /// Zero-initialised data.
    Bss,
}

/// An allocated section.
#[derive(Clone, Debug)]
pub struct Section {
    /// Section name.
    pub name: String,
    /// Address range.
    pub range: Range<u32>,
    /// What it holds.
    pub kind: SectionKind,
}

/// A loaded executable.
#[derive(Clone, Debug)]
pub struct Image {
    /// Entry point.
    pub entry: u32,
    /// Allocated sections, by address.
    pub sections: Vec<Section>,
    /// Sections that are not loaded (`.DVP.ovlytab` and the like), with
    /// their contents.
    pub unloaded: Vec<(String, Vec<u8>)>,
    base: u32,
    memory: Vec<u8>,
}

impl Image {
    /// Loads an ELF32 little-endian MIPS executable.
    ///
    /// # Errors
    ///
    /// [`AnalysisError::Elf`] if the file cannot be parsed, or
    /// [`AnalysisError::Layout`] if it has no allocated sections or they
    /// span more than 64 MiB.
    pub fn from_elf(data: &[u8]) -> Result<Self, AnalysisError> {
        let elf = ElfFile32::<Endianness>::parse(data)?;
        let endian = elf.endian();
        let mut sections = Vec::new();
        let mut unloaded = Vec::new();
        for section in elf.sections() {
            let header = section.elf_section_header();
            let name = section.name().unwrap_or_default().to_owned();
            let flags = header.sh_flags(endian);
            let size = header.sh_size(endian);
            if flags.0 & SHF_ALLOC.0 == 0 || section.address() == 0 {
                if !name.is_empty() && header.sh_type(endian) != SHT_NOBITS {
                    unloaded.push((name, section.data()?.to_vec()));
                }
                continue;
            }
            let start = header.sh_addr(endian);
            let kind = if header.sh_type(endian) == SHT_NOBITS {
                SectionKind::Bss
            } else if flags.0 & SHF_EXECINSTR.0 != 0 {
                if name == ".vutext" {
                    SectionKind::VuCode
                } else {
                    SectionKind::Code
                }
            } else {
                SectionKind::Data
            };
            let end = start
                .checked_add(size)
                .ok_or(AnalysisError::Layout("section wraps"))?;
            sections.push(Section {
                name,
                range: start..end,
                kind,
            });
        }
        sections.sort_by_key(|section| section.range.start);
        let base = sections
            .first()
            .ok_or(AnalysisError::Layout("no allocated sections"))?
            .range
            .start;
        let top = sections
            .iter()
            .map(|section| section.range.end)
            .max()
            .unwrap_or(base);
        if top - base > 64 << 20 {
            return Err(AnalysisError::Layout(
                "allocated sections span more than 64 MiB",
            ));
        }
        let mut memory = vec![0; (top - base) as usize];
        for section in elf.sections() {
            let Some(placed) = sections.iter().find(|s| {
                u64::from(s.range.start) == section.address()
                    && s.name == section.name().unwrap_or_default()
            }) else {
                continue;
            };
            if placed.kind == SectionKind::Bss {
                continue;
            }
            let bytes = section.data()?;
            let at = (placed.range.start - base) as usize;
            memory[at..at + bytes.len()].copy_from_slice(bytes);
        }
        Ok(Self {
            entry: u32::try_from(elf.entry()).map_err(|_| AnalysisError::Layout("entry point"))?,
            sections,
            unloaded,
            base,
            memory,
        })
    }

    /// An image from raw memory placed at `base`, for code that is not an
    /// ELF file (tests, overlays loaded at run time). Sections must lie
    /// within the memory.
    #[must_use]
    pub fn from_memory(entry: u32, base: u32, memory: Vec<u8>, mut sections: Vec<Section>) -> Self {
        sections.sort_by_key(|section| section.range.start);
        Self {
            entry,
            sections,
            unloaded: Vec::new(),
            base,
            memory,
        }
    }

    /// The bytes at `range`, if loaded.
    #[must_use]
    pub fn bytes(&self, range: Range<u32>) -> Option<&[u8]> {
        let start = range.start.checked_sub(self.base)? as usize;
        let end = range.end.checked_sub(self.base)? as usize;
        self.memory.get(start..end)
    }

    /// The little-endian word at `address`.
    #[must_use]
    pub fn word(&self, address: u32) -> Option<u32> {
        let bytes = self.bytes(address..address.checked_add(4)?)?;
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// The section called `name`.
    #[must_use]
    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|section| section.name == name)
    }

    /// The section containing `address`.
    #[must_use]
    pub fn section_at(&self, address: u32) -> Option<&Section> {
        self.sections
            .iter()
            .find(|section| section.range.contains(&address))
    }

    /// The EE code range (`.text`).
    ///
    /// # Errors
    ///
    /// [`AnalysisError::Layout`] if there is no `.text`.
    pub fn text(&self) -> Result<Range<u32>, AnalysisError> {
        self.section(".text")
            .map(|section| section.range.clone())
            .ok_or(AnalysisError::Layout("no .text section"))
    }

    /// An unloaded section's contents.
    #[must_use]
    pub fn unloaded(&self, name: &str) -> Option<&[u8]> {
        self.unloaded
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, data)| data.as_slice())
    }
}
