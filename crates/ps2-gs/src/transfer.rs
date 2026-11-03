//! Image transfers (TRXDIR): host to local memory through IMAGE packets or
//! HWREG, local to local at once, and local to host into a queue the EE
//! reads through VIF1.
//!
//! Pixels go row by row through the TRXREG rectangle from TRXPOS's corner,
//! packed as the format's transfer size: 32, 24, 16, 8 or 4 bits (low
//! nibble first); PSMT8H, 4HL and 4HH transfer 8 or 4 bits.

use crate::batch::Region;
use crate::regs::bits;
use crate::{Gs, Psm};

/// A host-to-local transfer under way.
#[derive(Clone, Debug)]
pub(crate) struct Transfer {
    psm: Psm,
    bp: u32,
    bw: u32,
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    /// The next pixel.
    x: u32,
    y: u32,
    /// Bytes of a pixel split across writes.
    pending: Vec<u8>,
    /// The blocks it writes (for drawing on several threads).
    region: Region,
}

/// Bits a pixel of `psm` takes in a transfer.
pub(crate) const fn transfer_bits(psm: Psm) -> u32 {
    match psm {
        Psm::T8h => 8,
        Psm::T4hl | Psm::T4hh => 4,
        _ => psm.bits(),
    }
}

impl Gs {
    /// TRXDIR was written: starts a transfer in `dir` (0 host to local, 1
    /// local to host, 2 local to local).
    pub(crate) fn start_transfer(&mut self, dir: u32) {
        let b = self.bitbltbuf;
        let (w, h) = (bits(self.trxreg, 0, 12), bits(self.trxreg, 32, 12));
        self.transfer = None;
        match dir {
            0 => {
                let Some(psm) = Psm::from_bits(u64::from(bits(b, 56, 6))) else {
                    return;
                };
                let (bp, bw) = (bits(b, 32, 14), bits(b, 48, 6));
                let (x0, y0) = (bits(self.trxpos, 32, 11), bits(self.trxpos, 48, 11));
                // Coordinates wrap at 2048: a rectangle that does, anywhere.
                let region = if x0 + w > 2048 || y0 + h > 2048 {
                    Region::ALL
                } else {
                    let rect = [x0, x0 + w - 1, y0, y0 + h - 1].map(|c| c as i32);
                    Region::of(psm, bp, bw, rect)
                };
                self.transfer = (w > 0 && h > 0).then(|| Transfer {
                    psm,
                    bp,
                    bw,
                    x0,
                    y0,
                    w,
                    h,
                    x: 0,
                    y: 0,
                    pending: Vec::new(),
                    region,
                });
            }
            1 => {
                self.flush();
                self.read_back(w, h);
            }
            2 => {
                self.flush();
                self.copy(w, h);
            }
            _ => {}
        }
    }

    /// Transfer data from the host.
    pub(crate) fn image(&mut self, data: &[u8]) {
        let Some(mut t) = self.transfer.take() else {
            return;
        };
        self.before_upload(t.region);
        let size = transfer_bits(t.psm);
        let write = |gs: &mut Self, t: &mut Transfer, value: u32| {
            if t.y >= t.h {
                return;
            }
            let (x, y) = ((t.x0 + t.x) & 0x7FF, (t.y0 + t.y) & 0x7FF);
            gs.mem.write(t.psm, t.bp, t.bw, x, y, value);
            t.x += 1;
            if t.x == t.w {
                t.x = 0;
                t.y += 1;
            }
        };
        if size == 4 {
            for &b in data {
                write(self, &mut t, u32::from(b & 0xF));
                write(self, &mut t, u32::from(b >> 4));
            }
        } else {
            let n = (size / 8) as usize;
            t.pending.extend_from_slice(data);
            let whole = t.pending.len() / n * n;
            let bytes = std::mem::take(&mut t.pending);
            for px in bytes[..whole].chunks_exact(n) {
                let value = px.iter().rev().fold(0, |v, &b| (v << 8) | u32::from(b));
                write(self, &mut t, value);
            }
            t.pending = bytes[whole..].to_vec();
        }
        if t.y < t.h {
            self.transfer = Some(t);
        }
    }

    /// A local-to-local copy of the TRXPOS source rectangle.
    fn copy(&mut self, w: u32, h: u32) {
        let b = self.bitbltbuf;
        let (Some(spsm), Some(dpsm)) = (
            Psm::from_bits(u64::from(bits(b, 24, 6))),
            Psm::from_bits(u64::from(bits(b, 56, 6))),
        ) else {
            return;
        };
        let (sbp, sbw, dbp, dbw) = (
            bits(b, 0, 14),
            bits(b, 16, 6),
            bits(b, 32, 14),
            bits(b, 48, 6),
        );
        let p = self.trxpos;
        let (sx, sy, dx, dy) = (
            bits(p, 0, 11),
            bits(p, 16, 11),
            bits(p, 32, 11),
            bits(p, 48, 11),
        );
        // DIR picks the order, which matters only when the rectangles
        // overlap: go the way that reads each pixel before it is written.
        let dir = bits(p, 59, 2);
        let rows: Vec<u32> = if dir & 2 != 0 {
            (0..h).rev().collect()
        } else {
            (0..h).collect()
        };
        let cols: Vec<u32> = if dir & 1 != 0 {
            (0..w).rev().collect()
        } else {
            (0..w).collect()
        };
        for &y in &rows {
            for &x in &cols {
                let v = self
                    .mem
                    .read(spsm, sbp, sbw, (sx + x) & 0x7FF, (sy + y) & 0x7FF);
                self.mem
                    .write(dpsm, dbp, dbw, (dx + x) & 0x7FF, (dy + y) & 0x7FF, v);
            }
        }
    }

    /// A local-to-host transfer: the source rectangle, packed, queued for
    /// the EE.
    fn read_back(&mut self, w: u32, h: u32) {
        let b = self.bitbltbuf;
        let Some(psm) = Psm::from_bits(u64::from(bits(b, 24, 6))) else {
            return;
        };
        let (bp, bw) = (bits(b, 0, 14), bits(b, 16, 6));
        let (sx, sy) = (bits(self.trxpos, 0, 11), bits(self.trxpos, 16, 11));
        let size = transfer_bits(psm);
        let mut bytes = Vec::with_capacity((w * h * size / 8) as usize + 16);
        let mut nibble: Option<u8> = None;
        for y in 0..h {
            for x in 0..w {
                let v = self
                    .mem
                    .read(psm, bp, bw, (sx + x) & 0x7FF, (sy + y) & 0x7FF);
                if size == 4 {
                    match nibble.take() {
                        None => nibble = Some(v as u8 & 0xF),
                        Some(low) => bytes.push(low | ((v as u8 & 0xF) << 4)),
                    }
                } else {
                    bytes.extend_from_slice(&v.to_le_bytes()[..(size / 8) as usize]);
                }
            }
        }
        bytes.extend(nibble);
        bytes.resize(bytes.len().next_multiple_of(16), 0);
        self.readback.extend(
            bytes
                .as_chunks::<16>()
                .0
                .iter()
                .map(|c| u128::from_le_bytes(*c)),
        );
    }

    /// Takes a quadword of local-to-host data, if any waits.
    pub fn read_fifo(&mut self) -> Option<u128> {
        self.readback.pop_front()
    }

    /// Quadwords of local-to-host data waiting.
    #[must_use]
    pub fn fifo_len(&self) -> usize {
        self.readback.len()
    }
}

#[cfg(test)]
mod tests {
    use crate::Gs;
    use crate::regs::addr;

    fn setup(gs: &mut Gs, dpsm: u64, w: u64, h: u64) {
        gs.write(
            addr::BITBLTBUF,
            (1 << 48) | (dpsm << 56) | (1 << 16) | (dpsm << 24),
        );
        gs.write(addr::TRXPOS, (2 << 32) | (3 << 48));
        gs.write(addr::TRXREG, w | (h << 32));
        gs.write(addr::TRXDIR, 0);
    }

    #[test]
    fn host_to_local_24_bit_spans_writes() {
        let mut gs = Gs::default();
        setup(&mut gs, 1, 3, 2);
        let pixels: Vec<u8> = (0..18).collect();
        gs.image(&pixels[..7]);
        gs.image(&pixels[7..]);
        assert_eq!(gs.mem.read(crate::Psm::Ct24, 0, 1, 2, 3), 0x02_0100);
        assert_eq!(gs.mem.read(crate::Psm::Ct24, 0, 1, 3, 3), 0x05_0403);
        assert_eq!(gs.mem.read(crate::Psm::Ct24, 0, 1, 4, 4), 0x11_100F);
        assert!(gs.transfer.is_none());
    }

    #[test]
    fn host_to_local_4_bit_low_nibble_first_and_reads_back() {
        let mut gs = Gs::default();
        setup(&mut gs, 0x14, 4, 1);
        gs.image(&[0x21, 0x43]);
        let read = |gs: &Gs, x| gs.mem.read(crate::Psm::T4, 0, 1, x, 3);
        assert_eq!(
            [read(&gs, 2), read(&gs, 3), read(&gs, 4), read(&gs, 5)],
            [1, 2, 3, 4]
        );
        gs.write(addr::TRXPOS, 2 | (3 << 16));
        gs.write(addr::TRXDIR, 1);
        assert_eq!(gs.read_fifo(), Some(0x4321));
        assert_eq!(gs.read_fifo(), None);
    }

    #[test]
    fn local_to_local_copies() {
        let mut gs = Gs::default();
        setup(&mut gs, 0, 2, 1);
        gs.image(&[1, 0, 0, 0, 2, 0, 0, 0]);
        // Copy (2, 3)..(3, 3) to (10, 20).
        gs.write(addr::TRXPOS, 2 | (3 << 16) | (10 << 32) | (20 << 48));
        gs.write(addr::TRXDIR, 2);
        assert_eq!(gs.mem.read(crate::Psm::Ct32, 0, 1, 11, 20), 2);
    }
}
