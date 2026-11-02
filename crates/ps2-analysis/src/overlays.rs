//! VU microcode overlays as Sony's `dvp-ld` lays them out.
//!
//! The `.DVP.overlay..<vu address>.<id>.<seq>.<n>` sections hold only zeros
//! of the overlay's size. The code itself is in `.vutext`, inside the DMA
//! packets that upload it; `.DVP.ovlytab` locates it with one 12-byte entry
//! per overlay: name offset in `.DVP.ovlystrtab`, EE address, VU address.

use crate::{AnalysisError, Image};

/// A VU microcode overlay.
#[derive(Clone, Debug)]
pub struct Overlay {
    /// The overlay section's name.
    pub name: String,
    /// Byte address in VU micro memory.
    pub vu_address: u32,
    /// Address of the code in EE memory (inside `.vutext`).
    pub ee_address: u32,
    /// The microcode: little-endian 64-bit instruction pairs.
    pub code: Vec<u8>,
}

/// The overlays of `image`, in table order; empty if it has no
/// `.DVP.ovlytab`.
///
/// # Errors
///
/// [`AnalysisError::Layout`] if the table names a missing section or code
/// outside the loaded image.
pub fn vu_overlays(image: &Image) -> Result<Vec<Overlay>, AnalysisError> {
    let Some(table) = image.unloaded(".DVP.ovlytab") else {
        return Ok(Vec::new());
    };
    let names = image
        .unloaded(".DVP.ovlystrtab")
        .ok_or(AnalysisError::Layout("no .DVP.ovlystrtab"))?;
    table
        .as_chunks::<12>()
        .0
        .iter()
        .map(|entry| {
            let word = |at: usize| {
                u32::from_le_bytes([entry[at], entry[at + 1], entry[at + 2], entry[at + 3]])
            };
            let name_at = word(0) as usize;
            let tail = names
                .get(name_at..)
                .ok_or(AnalysisError::Layout("overlay name offset"))?;
            let name_len = tail
                .iter()
                .position(|&b| b == 0)
                .ok_or(AnalysisError::Layout("overlay name"))?;
            let name = String::from_utf8_lossy(&tail[..name_len]).into_owned();
            let size = image
                .unloaded(&name)
                .ok_or(AnalysisError::Layout("overlay section missing"))?
                .len();
            let (ee_address, vu_address) = (word(4), word(8));
            let end = ee_address
                .checked_add(
                    u32::try_from(size).map_err(|_| AnalysisError::Layout("overlay size"))?,
                )
                .ok_or(AnalysisError::Layout("overlay wraps"))?;
            let code = image
                .bytes(ee_address..end)
                .ok_or(AnalysisError::Layout("overlay code not loaded"))?
                .to_vec();
            Ok(Overlay {
                name,
                vu_address,
                ee_address,
                code,
            })
        })
        .collect()
}
