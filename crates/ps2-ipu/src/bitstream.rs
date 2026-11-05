//! The bitstream reader: the two-quadword buffer the IPU reads bits from,
//! refilled from the input FIFO (`IPU.h`'s `tIPU_BP`, and `UBITS`, `SBITS`,
//! `DUMPBITS`, `GETBITS` and `getBits8/32/64` in `IPU_MultiISA.cpp`).
//!
//! Bits are read most significant first, from the buffer's bytes in
//! memory order (a quadword's least significant byte first).

use crate::fifo::InFifo;

/// The bit pointer and the internal buffer (PCSX2's `g_BP`), with the input
/// FIFO it pulls from.
#[derive(Clone, Debug, Default)]
pub(crate) struct Bitstream {
    /// The two buffered quadwords, as bytes.
    buf: [u8; 32],
    /// Bit position in the buffer (BP), below 128 between reads.
    pub(crate) bp: u32,
    /// Buffered quadwords (FP), 0 to 2.
    pub(crate) fp: u32,
    pub(crate) fifo: InFifo,
    /// A read found the FIFO empty (`IPUCoreStatus.WaitingOnIPUTo`).
    pub(crate) waiting: bool,
}

impl Bitstream {
    /// Clears the bit pointer and the buffer, not the FIFO (PCSX2's
    /// `memset(&g_BP, 0)`, after which its FIFO count is cleared too).
    pub(crate) fn reset_pointer(&mut self) {
        self.buf = [0; 32];
        self.bp = 0;
        self.fp = 0;
    }

    /// Loads quadwords until `bits` bits from BP are buffered; false if
    /// the FIFO runs dry first (`FillBuffer`).
    pub(crate) fn fill(&mut self, bits: u32) -> bool {
        while self.fp * 128 < self.bp + bits {
            let Some(qw) = self.fifo.read() else {
                self.waiting = true;
                return false;
            };
            let at = self.fp as usize * 16;
            self.buf[at..at + 16].copy_from_slice(&qw.to_le_bytes());
            self.fp += 1;
        }
        true
    }

    /// Moves BP on by `bits`, whether or not they were there (`Advance`).
    pub(crate) fn advance(&mut self, bits: u32) {
        self.fill(bits);
        self.bp += bits;
        if self.bp >= 128 {
            self.bp -= 128;
            if self.fp == 2 {
                // The second quadword moves to the front; the old copy stays.
                self.buf.copy_within(16..32, 0);
                self.fp = 1;
            } else if let Some(qw) = self.fifo.read() {
                self.buf[..16].copy_from_slice(&qw.to_le_bytes());
                self.fp = 1;
            } else {
                self.fp = 0;
            }
        }
    }

    /// Moves BP to the next byte boundary (`Align`).
    pub(crate) fn align(&mut self) {
        self.bp = (self.bp + 7) & !7;
        self.advance(0);
    }

    /// The 32 bits at BP's byte, shifted by BP's bit offset.
    fn word(&self) -> u32 {
        let at = (self.bp / 8) as usize;
        let w = u32::from_be_bytes([
            self.buf[at],
            self.buf[at + 1],
            self.buf[at + 2],
            self.buf[at + 3],
        ]);
        w << (self.bp & 7)
    }

    /// The next `bits` bits (1 to 32), not removed (`UBITS`). Reads whatever
    /// the buffer holds, buffered or not.
    pub(crate) fn ubits(&self, bits: u32) -> u32 {
        self.word() >> (32 - bits)
    }

    /// The next `bits` bits, sign-extended (`SBITS`).
    pub(crate) fn sbits(&self, bits: u32) -> i32 {
        (self.word() as i32) >> (32 - bits)
    }

    /// The next `bits` bits, removed (`GETBITS`).
    pub(crate) fn getbits(&mut self, bits: u32) -> u32 {
        let v = self.ubits(bits);
        self.advance(bits);
        v
    }

    /// The next `N` bytes' worth of bits in stream order, if buffered;
    /// removed if `advance` (`getBits8`, `getBits32`, `getBits64`).
    pub(crate) fn bytes<const N: usize>(&mut self, advance: bool) -> Option<[u8; N]> {
        if !self.fill(8 * N as u32) {
            return None;
        }
        let at = (self.bp / 8) as usize;
        let shift = self.bp & 7;
        let mut out = [0; N];
        for (k, b) in out.iter_mut().enumerate() {
            *b = if shift == 0 {
                self.buf[at + k]
            } else {
                (self.buf[at + k] << shift) | (self.buf[at + k + 1] >> (8 - shift))
            };
        }
        if advance {
            self.advance(8 * N as u32);
        }
        Some(out)
    }

    /// The next 32 bits as a big-endian word, not removed (`getBits32`
    /// then `BigEndian`), if buffered.
    pub(crate) fn peek32(&mut self) -> Option<u32> {
        self.bytes::<4>(false).map(u32::from_be_bytes)
    }
}
