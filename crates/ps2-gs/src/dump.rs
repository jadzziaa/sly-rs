//! GS dumps in PCSX2's `.gs` format, so PCSX2 can draw exactly what the GS
//! here was sent and the two pictures can be compared.
//!
//! A dump is a header, the GS state to start from (PCSX2's state version 9
//! layout: registers, transfer state, local memory, GIF path states, Q),
//! the privileged registers, then packets: GIF transfers per path, the
//! privileged registers at each V-sync, and V-syncs. The layout follows
//! PCSX2's `GS/GSDump.cpp`, `GS/GSState.cpp` and `GS/GSLzma.cpp` (read for
//! facts). A dump starts between frames, so no packet, transfer or
//! primitive is under way and those parts of the state are left empty.
//!
//! A dump holds the game's pictures and textures: it never goes into the
//! repository.

use crate::{Gs, mem};

/// PCSX2's GS state version this writes.
const STATE_VERSION: u32 = 9;
/// The privileged registers' block (`GSPrivRegSet`), in bytes.
pub const PRIV_SIZE: usize = 8192;

/// Packet kinds.
mod packet {
    pub(super) const TRANSFER: u8 = 0;
    pub(super) const VSYNC: u8 = 1;
    pub(super) const REGISTERS: u8 = 3;
}

/// A dump being written.
#[derive(Debug, Default)]
pub(crate) struct Dump {
    bytes: Vec<u8>,
}

impl Dump {
    fn u32(&mut self, v: u32) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }
}

impl Gs {
    /// Starts a dump from the GS's state now; `privileged` is the
    /// privileged registers' block (`0x12000000..0x12002000`).
    pub fn start_dump(&mut self, privileged: &[u8; PRIV_SIZE]) {
        // The dump starts from local memory with everything queued drawn.
        self.flush();
        self.sync_memory();
        let state = self.freeze();
        let mut d = Dump::default();
        // A new-style header: CRC 0xFFFFFFFF, the header's size, the header
        // (no serial, no screenshot), the state, the registers.
        d.u32(0xFFFF_FFFF);
        d.u32(9 * 4);
        d.u32(STATE_VERSION);
        d.u32(state.len() as u32);
        d.u32(9 * 4); // serial offset
        d.u32(0); // serial size
        d.u32(0); // CRC
        d.u32(0); // screenshot width
        d.u32(0); // screenshot height
        d.u32(9 * 4); // screenshot offset
        d.u32(0); // screenshot size
        d.bytes.extend_from_slice(&state);
        d.bytes.extend_from_slice(privileged);
        self.dump = Some(d);
    }

    /// Whether a dump is being written.
    #[must_use]
    pub const fn dumping(&self) -> bool {
        self.dump.is_some()
    }

    /// Records a V-sync of `field` (0 or 1) with the privileged registers.
    pub fn dump_vsync(&mut self, field: u8, privileged: &[u8; PRIV_SIZE]) {
        if let Some(d) = self.dump.as_mut() {
            d.bytes.push(packet::REGISTERS);
            d.bytes.extend_from_slice(privileged);
            d.bytes.push(packet::VSYNC);
            d.bytes.push(field);
        }
    }

    /// Ends the dump and gives its bytes.
    pub fn finish_dump(&mut self) -> Option<Vec<u8>> {
        self.dump.take().map(|d| d.bytes)
    }

    /// Records a GIF transfer on `path` (1 to 3).
    pub(crate) fn dump_transfer(&mut self, path: usize, data: &[u128]) {
        if let Some(d) = self.dump.as_mut()
            && !data.is_empty()
        {
            d.bytes.push(packet::TRANSFER);
            d.bytes.push(path as u8);
            d.u32((data.len() * 16) as u32);
            for q in data {
                d.bytes.extend_from_slice(&q.to_le_bytes());
            }
        }
    }

    /// The GS state in PCSX2's layout (`GSState::Freeze`, version 9).
    fn freeze(&self) -> Vec<u8> {
        let mut d = Dump::default();
        d.u32(STATE_VERSION);
        for v in [
            self.prim,
            self.prmodecont,
            self.texclut,
            self.scanmsk,
            self.texa,
            self.fogcol,
            self.dimx,
            self.dthe,
            self.colclamp,
            self.pabe,
            self.bitbltbuf,
            3, // TRXDIR: no transfer
            self.trxpos,
            self.trxreg,
            self.trxreg,
        ] {
            d.u64(v);
        }
        for c in &self.ctx {
            for v in [
                c.xyoffset, c.tex0, c.tex1, c.clamp, c.miptbp1, c.miptbp2, c.scissor, c.alpha,
                c.test, c.fba, c.frame, c.zbuf,
            ] {
                d.u64(v);
            }
        }
        // The vertex registers as PCSX2 keeps them (`GSVertex`): RGBAQ and
        // ST as 64 bits, UV and FOG as 32 (FOG's upper word), XYZ; then an
        // obsolete register.
        d.u64(self.rgbaq);
        d.u64(self.st);
        d.u32(self.uv as u32);
        d.u32((self.fog >> 32) as u32);
        d.u64(0);
        d.u64(0);
        // The transfer: x, y, w, h; BITBLTBUF, TRXPOS, TRXREG; its
        // rectangle; total, start, end; write.
        for _ in 0..4 {
            d.u32(0);
        }
        for v in [self.bitbltbuf, self.trxpos, self.trxreg] {
            d.u64(v);
        }
        d.bytes.extend_from_slice(&[0; 16]);
        for _ in 0..3 {
            d.u32(0);
        }
        d.bytes.push(0);
        let memory = self.mem.to_bytes();
        debug_assert_eq!(memory.len(), mem::SIZE);
        d.bytes.extend_from_slice(&memory);
        // Four GIF paths: tag and register index, idle.
        for _ in 0..4 {
            d.bytes.extend_from_slice(&[0; 20]);
        }
        d.bytes.extend_from_slice(&1.0f32.to_le_bytes());
        d.bytes
    }
}

/// A dump read back: the GS as it started, the privileged registers then,
/// and the packets.
pub type Loaded = (Gs, Box<[u8; PRIV_SIZE]>, Vec<Packet>);

/// A packet of a dump.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Packet {
    /// GIF data on a path (1 to 3).
    Transfer(usize, Vec<u128>),
    /// The privileged registers' block.
    Registers(Box<[u8; PRIV_SIZE]>),
    /// A V-sync of a field.
    VSync(u8),
}

/// Reading a dump.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self
            .bytes
            .get(self.at..self.at + n)
            .ok_or("the dump ends early")?;
        self.at += n;
        Ok(s)
    }
}

/// Reads back a dump this module wrote: the GS as it started, the
/// privileged registers then, and the packets.
///
/// # Errors
///
/// If the bytes are not such a dump.
pub fn read(bytes: &[u8]) -> Result<Loaded, String> {
    let mut cursor = Cursor { bytes, at: 0 };
    let u32_ = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let u64_ = |b: &[u8]| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
    if u32_(cursor.take(4)?) != 0xFFFF_FFFF {
        return Err("not a new-style GS dump".to_owned());
    }
    let header_size = u32_(cursor.take(4)?) as usize;
    let header = cursor.take(header_size)?;
    if u32_(&header[0..4]) != STATE_VERSION {
        return Err("not state version 9".to_owned());
    }
    let state_size = u32_(&header[4..8]) as usize;
    let state = cursor.take(state_size)?.to_vec();
    let mut privileged = Box::new([0u8; PRIV_SIZE]);
    privileged.copy_from_slice(cursor.take(PRIV_SIZE)?);

    // The state, in the order `freeze` writes it.
    let mut gs = Gs::default();
    let mut s = 4usize;
    let mut next64 = || {
        let v = u64_(&state[s..s + 8]);
        s += 8;
        v
    };
    let regs: Vec<u64> = (0..15).map(|_| next64()).collect();
    [
        gs.prim,
        gs.prmodecont,
        gs.texclut,
        gs.scanmsk,
        gs.texa,
        gs.fogcol,
        gs.dimx,
        gs.dthe,
        gs.colclamp,
        gs.pabe,
        gs.bitbltbuf,
    ] = [
        regs[0], regs[1], regs[2], regs[3], regs[4], regs[5], regs[6], regs[7], regs[8], regs[9],
        regs[10],
    ];
    (gs.trxpos, gs.trxreg) = (regs[12], regs[13]);
    for c in &mut gs.ctx {
        c.xyoffset = next64();
        c.tex0 = next64();
        c.tex1 = next64();
        c.clamp = next64();
        c.miptbp1 = next64();
        c.miptbp2 = next64();
        c.scissor = next64();
        c.alpha = next64();
        c.test = next64();
        c.fba = next64();
        c.frame = next64();
        c.zbuf = next64();
    }
    gs.rgbaq = next64();
    gs.st = next64();
    let uv_fog = next64();
    gs.uv = uv_fog & 0xFFFF_FFFF;
    gs.fog = (uv_fog >> 32) << 32;
    // XYZ, the obsolete register, the transfer state.
    let memory_at = s + 16 + 16 + 24 + 16 + 12 + 1;
    let memory = state
        .get(memory_at..memory_at + mem::SIZE)
        .ok_or("the state is too short")?;
    gs.mem.load_bytes(memory);

    let mut packets = Vec::new();
    while cursor.at < bytes.len() {
        match cursor.take(1)?[0] {
            packet::TRANSFER => {
                let path = cursor.take(1)?[0] as usize;
                let n = u32_(cursor.take(4)?) as usize;
                let data = cursor.take(n)?;
                let qwords = data
                    .as_chunks::<16>()
                    .0
                    .iter()
                    .map(|c| u128::from_le_bytes(*c))
                    .collect();
                packets.push(Packet::Transfer(path, qwords));
            }
            packet::VSYNC => packets.push(Packet::VSync(cursor.take(1)?[0])),
            packet::REGISTERS => {
                let mut block = Box::new([0u8; PRIV_SIZE]);
                block.copy_from_slice(cursor.take(PRIV_SIZE)?);
                packets.push(Packet::Registers(block));
            }
            2 => {
                cursor.take(4)?;
            }
            other => return Err(format!("unknown packet {other}")),
        }
    }
    Ok((gs, privileged, packets))
}

#[cfg(test)]
mod tests {
    use crate::Gs;

    #[test]
    fn a_dump_reads_back() {
        let mut gs = Gs::default();
        gs.mem.write(crate::Psm::Ct32, 0, 1, 5, 6, 0x1234_5678);
        gs.write(crate::regs::addr::TEXA, 0x80_0000_0080);
        gs.start_dump(&[7; super::PRIV_SIZE]);
        gs.gif(3, [1u128 | 1 << 15]);
        gs.dump_vsync(1, &[0; super::PRIV_SIZE]);
        let bytes = gs.finish_dump().unwrap_or_default();
        let (back, privileged, packets) = super::read(&bytes).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.mem.read(crate::Psm::Ct32, 0, 1, 5, 6), 0x1234_5678);
        assert_eq!(back.texa, 0x80_0000_0080);
        assert_eq!(privileged[0], 7);
        assert_eq!(packets.len(), 3);
        assert_eq!(packets[0], super::Packet::Transfer(3, vec![1 | 1 << 15]));
        assert_eq!(packets[2], super::Packet::VSync(1));
    }

    #[test]
    fn a_dump_has_pcsx2s_layout() {
        let mut gs = Gs::default();
        gs.start_dump(&[0; super::PRIV_SIZE]);
        gs.gif(3, [1u128 | 1 << 15]);
        gs.dump_vsync(1, &[0; super::PRIV_SIZE]);
        let bytes = gs.finish_dump().unwrap_or_default();
        let word = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        assert_eq!(word(0), 0xFFFF_FFFF);
        assert_eq!(word(4), 36);
        assert_eq!(word(8), 9);
        let state = word(12) as usize;
        // The state's size, as PCSX2's GetSaveStateSize gives it for
        // version 9.
        assert_eq!(
            state,
            4 + 15 * 8 + 24 * 8 + 4 * 8 + 8 + 16 + 24 + 16 + 12 + 1 + (4 << 20) + 80 + 4
        );
        let packets = 8 + 36 + state + super::PRIV_SIZE;
        assert_eq!(&bytes[packets..packets + 6], &[0, 3, 16, 0, 0, 0]);
        let vsync = packets + 6 + 16;
        assert_eq!(bytes[vsync], 3);
        assert_eq!(&bytes[vsync + 1 + super::PRIV_SIZE..], &[1, 1]);
    }
}
