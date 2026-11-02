//! A summary of a PS2 executable for `info`: its sections, VU microcode
//! overlays and embedded IOP modules. Full ELF loading belongs to the
//! analysis crate (phase 2).

use anyhow::{Context, Result};
use object::{Object, ObjectSection};

/// Prefix of the sections `dvp-as` emits for VU microcode overlays.
const DVP_OVERLAY: &str = ".DVP.overlay";

/// An allocated section of the executable.
pub(crate) struct Section {
    pub(crate) name: String,
    pub(crate) address: u64,
    pub(crate) size: u64,
}

/// What `info` reports about an executable.
pub(crate) struct Summary {
    pub(crate) entry: u64,
    pub(crate) sections: Vec<Section>,
    pub(crate) vu_overlays: usize,
    /// Names of the IOP modules linked into the `.irx` section.
    pub(crate) embedded_modules: Vec<String>,
}

/// Summarises an EE executable.
pub(crate) fn summarize(data: &[u8]) -> Result<Summary> {
    let file = object::File::parse(data).context("not an ELF file")?;
    let mut sections = Vec::new();
    let mut vu_overlays = 0;
    let mut embedded_modules = Vec::new();
    for section in file.sections() {
        let name = section.name().unwrap_or_default();
        if name.starts_with(DVP_OVERLAY) {
            vu_overlays += 1;
        } else if section.address() != 0 {
            sections.push(Section {
                name: name.to_owned(),
                address: section.address(),
                size: section.size(),
            });
        }
        if name == ".irx" {
            let irx = section.data().context(".irx section")?;
            embedded_modules = embedded_irx_names(irx);
        }
    }
    sections.sort_by_key(|section| section.address);
    Ok(Summary {
        entry: file.entry(),
        sections,
        vu_overlays,
        embedded_modules,
    })
}

/// The module name of an IRX (IOP relocatable executable): the string in
/// its `.iopmod` section after six words and a version halfword.
pub(crate) fn irx_name(data: &[u8]) -> Option<String> {
    let file = object::File::parse(data).ok()?;
    let iopmod = file.section_by_name(".iopmod")?.data().ok()?;
    let name = iopmod.get(0x1a..)?;
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    Some(String::from_utf8_lossy(&name[..end]).into_owned())
}

/// Names of the IRX modules concatenated in an `.irx` section, found by
/// their ELF headers on 16-byte boundaries.
fn embedded_irx_names(irx: &[u8]) -> Vec<String> {
    (0..irx.len())
        .step_by(16)
        .filter(|&at| irx[at..].starts_with(b"\x7fELF"))
        .filter_map(|at| irx_name(&irx[at..]))
        .collect()
}
