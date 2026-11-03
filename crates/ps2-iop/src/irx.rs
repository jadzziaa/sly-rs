//! IRX modules: relocatable MIPS ELF files (type `0xFF80`) that the IOP's
//! loader places anywhere in RAM.
//!
//! [`Irx::parse`] reads the module information (`.iopmod`: entry point,
//! `$gp`, sizes, name) and the loadable image with its relocations;
//! [`Irx::load`] relocates the image for a base address and finds its import
//! tables. An import table is a header (magic `0x41E00000`, library name,
//! version) followed by one stub per function: `jr $ra` and
//! `addiu $zero, $zero, index`. The loader links each stub to the library's
//! function.

use std::fmt;

/// An import table's magic word.
pub const IMPORT_MAGIC: u32 = 0x41E0_0000;
/// An export table's magic word.
pub const EXPORT_MAGIC: u32 = 0x41C0_0000;
/// `jr $ra`, the first word of an import stub.
const JR_RA: u32 = 0x03E0_0008;

/// Why a module did not load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrxError(pub String);

impl fmt::Display for IrxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for IrxError {}

fn error(what: impl Into<String>) -> IrxError {
    IrxError(what.into())
}

fn u16_at(data: &[u8], at: usize) -> Result<u16, IrxError> {
    data.get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| error(format!("truncated at {at:#x}")))
}

fn u32_at(data: &[u8], at: usize) -> Result<u32, IrxError> {
    data.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| error(format!("truncated at {at:#x}")))
}

/// A relocation: where, and how.
#[derive(Clone, Copy, Debug)]
struct Reloc {
    offset: u32,
    kind: u8,
}

/// A parsed module, not yet placed.
#[derive(Clone, Debug)]
pub struct Irx {
    /// The module's name (from `.iopmod`).
    pub name: String,
    /// Its version.
    pub version: u16,
    /// Entry point, relative to the base.
    pub entry: u32,
    /// `$gp`, relative to the base.
    pub gp: u32,
    /// The image: text and data, then zeros for bss.
    pub image: Vec<u8>,
    relocs: Vec<Reloc>,
}

/// An import stub to link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    /// The library.
    pub library: String,
    /// Its version (major in the high byte).
    pub version: u16,
    /// The function's index in the library.
    pub index: u16,
    /// The stub's address.
    pub stub: u32,
}

const R_MIPS_32: u8 = 2;
const R_MIPS_26: u8 = 4;
const R_MIPS_HI16: u8 = 5;
const R_MIPS_LO16: u8 = 6;

impl Irx {
    /// Parses an IRX file.
    ///
    /// # Errors
    ///
    /// If it is not an IRX module this loader understands.
    pub fn parse(data: &[u8]) -> Result<Self, IrxError> {
        if data.get(..4) != Some(b"\x7fELF".as_slice()) || u16_at(data, 16)? != 0xFF80 {
            return Err(error("not an IRX module (ELF type 0xFF80)"));
        }
        let (phoff, shoff) = (u32_at(data, 0x1C)? as usize, u32_at(data, 0x20)? as usize);
        let (phentsize, phnum) = (u16_at(data, 0x2A)? as usize, u16_at(data, 0x2C)? as usize);
        let (shentsize, shnum) = (u16_at(data, 0x2E)? as usize, u16_at(data, 0x30)? as usize);
        let shstrndx = u16_at(data, 0x32)? as usize;

        // Program headers: the .iopmod record, then one loadable segment.
        let mut iopmod = None;
        let mut load = None;
        for i in 0..phnum {
            let at = phoff + i * phentsize;
            let (kind, offset, filesz, memsz) = (
                u32_at(data, at)?,
                u32_at(data, at + 4)? as usize,
                u32_at(data, at + 16)? as usize,
                u32_at(data, at + 20)? as usize,
            );
            match kind {
                0x7000_0080 => iopmod = Some(offset),
                1 if load.is_none() => load = Some((offset, filesz, memsz)),
                _ => {}
            }
        }
        let iopmod = iopmod.ok_or_else(|| error("no .iopmod record"))?;
        let (offset, filesz, memsz) = load.ok_or_else(|| error("no loadable segment"))?;
        let mut image = data
            .get(offset..offset + filesz)
            .ok_or_else(|| error("segment outside the file"))?
            .to_vec();
        image.resize(memsz.max(filesz), 0);
        // .iopmod: info address, entry, gp, text, data and bss sizes,
        // version, name.
        let entry = u32_at(data, iopmod + 4)?;
        let gp = u32_at(data, iopmod + 8)?;
        let version = u16_at(data, iopmod + 24)?;
        let name: Vec<u8> = data
            .get(iopmod + 26..)
            .unwrap_or_default()
            .iter()
            .take_while(|&&b| b != 0)
            .copied()
            .collect();

        // Relocations: every SHT_REL section.
        let names = {
            let at = shoff + shstrndx * shentsize;
            u32_at(data, at + 16)? as usize
        };
        let mut relocs = Vec::new();
        for i in 0..shnum {
            let at = shoff + i * shentsize;
            if u32_at(data, at + 4)? != 9 {
                continue;
            }
            let (offset, size) = (
                u32_at(data, at + 16)? as usize,
                u32_at(data, at + 20)? as usize,
            );
            let _ = names;
            for r in (offset..offset + size).step_by(8) {
                relocs.push(Reloc {
                    offset: u32_at(data, r)?,
                    kind: (u32_at(data, r + 4)? & 0xFF) as u8,
                });
            }
        }
        Ok(Self {
            name: String::from_utf8_lossy(&name).into_owned(),
            version,
            entry,
            gp,
            image,
            relocs,
        })
    }

    /// The image relocated for `base`, and its import stubs (at their final
    /// addresses).
    ///
    /// # Errors
    ///
    /// A relocation the loader does not know, or one outside the image.
    pub fn load(&self, base: u32) -> Result<(Vec<u8>, Vec<Import>), IrxError> {
        let mut image = self.image.clone();
        let word = |image: &[u8], at: u32| u32_at(image, at as usize);
        let put = |image: &mut [u8], at: u32, value: u32| {
            if let Some(slot) = image.get_mut(at as usize..at as usize + 4) {
                slot.copy_from_slice(&value.to_le_bytes());
            }
        };
        // HI16s wait for the LO16 that completes their addend.
        let mut pending_hi: Vec<u32> = Vec::new();
        for r in &self.relocs {
            let w = word(&image, r.offset)?;
            match r.kind {
                R_MIPS_32 => put(&mut image, r.offset, w.wrapping_add(base)),
                R_MIPS_26 => {
                    let target = ((w & 0x03FF_FFFF) << 2).wrapping_add(base);
                    put(
                        &mut image,
                        r.offset,
                        (w & 0xFC00_0000) | ((target >> 2) & 0x03FF_FFFF),
                    );
                }
                R_MIPS_HI16 => pending_hi.push(r.offset),
                R_MIPS_LO16 => {
                    let lo = i32::from((w as u16).cast_signed());
                    for hi_at in pending_hi.drain(..) {
                        let hi = word(&image, hi_at)?;
                        let value = ((hi & 0xFFFF) << 16)
                            .wrapping_add(lo.cast_unsigned())
                            .wrapping_add(base);
                        let high = value.wrapping_add(0x8000) >> 16;
                        put(&mut image, hi_at, (hi & 0xFFFF_0000) | high);
                    }
                    let value = (lo.cast_unsigned()).wrapping_add(base);
                    put(&mut image, r.offset, (w & 0xFFFF_0000) | (value & 0xFFFF));
                }
                0 => {}
                other => return Err(error(format!("relocation type {other} at {:#x}", r.offset))),
            }
        }
        if !pending_hi.is_empty() {
            return Err(error("HI16 relocation without a LO16"));
        }
        let imports = imports(&image, base);
        Ok((image, imports))
    }
}

/// The import stubs in a loaded image.
fn imports(image: &[u8], base: u32) -> Vec<Import> {
    let mut out = Vec::new();
    let words = image.len() / 4;
    let w = |i: usize| u32_at(image, 4 * i).unwrap_or(0);
    let mut i = 0;
    while i + 5 < words {
        if w(i) == IMPORT_MAGIC && w(i + 1) == 0 {
            let version = (w(i + 2) & 0xFFFF) as u16;
            let name: Vec<u8> = image[4 * i + 12..4 * i + 20]
                .iter()
                .take_while(|&&b| b != 0)
                .copied()
                .collect();
            let library = String::from_utf8_lossy(&name).into_owned();
            let mut j = i + 5;
            while j + 1 < words && w(j) == JR_RA {
                out.push(Import {
                    library: library.clone(),
                    version,
                    index: (w(j + 1) & 0xFFFF) as u16,
                    stub: base + 4 * j as u32,
                });
                j += 2;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// An export table at `address` in IOP memory, as a module registers it:
/// the library's name, version and function addresses.
#[must_use]
pub fn exports(ram: &[u8], address: u32) -> Option<(String, u16, Vec<u32>)> {
    let at = (address & 0x1F_FFFF) as usize;
    if u32_at(ram, at).ok()? != EXPORT_MAGIC {
        return None;
    }
    let version = u16_at(ram, at + 8).ok()?;
    let name: Vec<u8> = ram
        .get(at + 12..at + 20)?
        .iter()
        .take_while(|&&b| b != 0)
        .copied()
        .collect();
    let mut functions = Vec::new();
    let mut f = at + 20;
    while let Ok(address) = u32_at(ram, f) {
        if address == 0 {
            break;
        }
        functions.push(address);
        f += 4;
    }
    Some((
        String::from_utf8_lossy(&name).into_owned(),
        version,
        functions,
    ))
}
