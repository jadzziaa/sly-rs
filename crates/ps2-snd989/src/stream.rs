//! VAG streams: sound read from the disc as it plays (voice lines, and
//! music in some games), through a ring of two halves in sound memory.
//!
//! As Sly 1's driver does it (seen in its register traffic): the stream's
//! voice starts on the first half, past the file's 0x30-byte VAG header;
//! each half's last block carries the loop-end and repeat flags, and the
//! voice's loop address (LSAX) points at the other half, so at the end of
//! a half the voice jumps to the other. The driver polls the voice's next
//! address (NAX) every tick: when the voice has moved into the other half,
//! the half it left is refilled from the disc and LSAX is pointed at it.
//! The last data block ends the voice; once its envelope is silent the
//! stream is over.
//!
//! The volume, pitch and envelope follow rac-rs's port of the R&C driver
//! (`snd989/streams.rs`): ADSR 0x000F / 0x1FC0, the pitch from the VAG's
//! sample rate, the volume through the stream pan law. A held stream (flag
//! 1) keys on silent, at pitch 0, until `snd_ContinueVAGStream`.

use crate::Bus;
use crate::tables::stream_volume;
use crate::voices::{Voices, reg};

/// The type in a stream's handle.
pub(crate) const HANDLE_TYPE: u32 = 0x0400_0000;
/// Bytes of a VAG header.
const HEADER: u32 = 0x30;
/// Bytes of a disc sector.
const SECTOR: u32 = 0x800;
/// Stream slots.
const SLOTS: usize = 8;

/// `snd_PlayVAGStreamByLoc`'s arguments, as the EE packs them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PlayVag {
    pub(crate) lba: u32,
    pub(crate) lba_right: u32,
    /// Where the VAG header starts in the first sector.
    pub(crate) offset: u32,
    /// Volume, 0..0x400.
    pub(crate) volume: u32,
    pub(crate) pan: i16,
    pub(crate) group: i8,
    /// Bit 0: start held.
    pub(crate) flags: u32,
}

impl PlayVag {
    pub(crate) fn from_words(d: &[u32; 7]) -> Self {
        Self {
            lba: d[0],
            lba_right: d[1],
            offset: d[2] & 0xFFFF,
            volume: d[2] >> 16,
            pan: (d[3] >> 16) as u16 as i16,
            group: d[4] as i8,
            flags: d[6],
        }
    }
}

/// A stream playing.
#[derive(Clone, Debug)]
struct Stream {
    /// Where the file starts on the disc, in bytes.
    file: u64,
    /// The file's length: header and data.
    length: u32,
    /// Bytes of the file in the ring so far.
    fed: u32,
    /// The ring: its address and the bytes of one half.
    ring: u32,
    half: u32,
    voice: usize,
    group: i8,
    volume: i32,
    pan: i32,
    pitch: u16,
    held: bool,
    /// The half the voice is in.
    playing: u32,
    /// Ticks since the key-on (the first poll comes a tick after it).
    age: u32,
    /// The last of the data is in the ring.
    last_queued: bool,
    /// The half the voice is not in holds data to play next.
    other_ready: bool,
}

/// The streams.
pub(crate) struct Streams {
    /// The rings `snd_InitVAGStreamingEx` set up, and the bytes of each.
    rings: Vec<u32>,
    ring_bytes: u32,
    slots: Vec<Option<Stream>>,
    words: [u32; SLOTS],
}

/// The VAG header of a file: its data bytes and sample rate (big endian).
fn vag_header(sector: &[u8], offset: usize) -> Option<(u32, u32)> {
    let h = sector.get(offset..offset + HEADER as usize)?;
    if &h[..4] != b"VAGp" {
        return None;
    }
    let be = |at: usize| u32::from_be_bytes([h[at], h[at + 1], h[at + 2], h[at + 3]]);
    Some((be(0xC), be(0x10)))
}

impl Streams {
    pub(crate) fn new() -> Self {
        Self {
            rings: Vec::new(),
            ring_bytes: 0,
            slots: (0..SLOTS).map(|_| None).collect(),
            words: [1; SLOTS],
        }
    }

    /// `snd_InitVAGStreamingEx(count, bytes, ...)`: the ring of each
    /// stream, `bytes` rounded up to 4 KiB (at least 8 KiB), taken from
    /// sound memory in one block.
    pub(crate) fn init(&mut self, memory: &mut crate::bank::SpuMemory, count: u32, bytes: u32) {
        if !self.rings.is_empty() {
            return;
        }
        let fp = bytes.div_ceil(0x1000).max(2) * 0x1000;
        if let Some(base) = memory.alloc(fp * count.max(1)) {
            self.ring_bytes = fp;
            self.rings = (0..count.max(1)).map(|i| base + i * fp).collect();
        }
    }

    fn find(&self, handle: u32) -> Option<usize> {
        if (handle >> 24) & 0x1F != 4 {
            return None;
        }
        let slot = ((handle >> 16) & 0xFF) as usize;
        (slot < SLOTS && self.words[slot] == handle && self.slots[slot].is_some()).then_some(slot)
    }

    /// The handle while the stream plays, else 0.
    pub(crate) fn playing(&self, handle: u32) -> u32 {
        if self.find(handle).is_some() {
            handle
        } else {
            0
        }
    }

    /// Whether the stream has its first data in sound memory (it does as
    /// soon as it starts: the disc is read at once).
    pub(crate) fn buffered(&self, handle: u32) -> u32 {
        u32::from(self.find(handle).is_some())
    }

    /// Reads `bytes` of the file from `at` into the ring at `to`, the last
    /// block flagged to repeat (the voice jumps to its loop address) or,
    /// at the end of the data, to end.
    fn fill(bus: &mut dyn Bus, s: &mut Stream, to: u32) {
        let at = u64::from(s.fed);
        let want = s.half.min(s.length.saturating_sub(s.fed));
        let start = s.file + at;
        let first = (start / u64::from(SECTOR)) as u32;
        let skip = (start % u64::from(SECTOR)) as usize;
        let sectors = (skip as u32 + want).div_ceil(SECTOR);
        let mut data = bus
            .sectors(first, sectors)
            .map(|d| {
                d.get(skip..skip + want as usize)
                    .map(<[u8]>::to_vec)
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        data.resize(s.half as usize, 0);
        s.fed += want;
        let blocks = s.half as usize / 16;
        if s.fed >= s.length {
            s.last_queued = true;
            // The block holding the last data byte ends the voice; the rest
            // is silence.
            let end_block = (want as usize).div_ceil(16).clamp(1, blocks) - 1;
            data[end_block * 16 + 1] = 0x01;
            for b in data[(end_block + 1) * 16..].chunks_mut(16) {
                b.fill(0);
                b[1] = 0x01;
            }
        } else {
            data[(blocks - 1) * 16 + 1] = 0x03;
        }
        bus.upload(to, &data);
    }

    /// `snd_PlayVAGStreamByLoc`: the stream's handle, or 0.
    pub(crate) fn play(&mut self, bus: &mut dyn Bus, voices: &mut Voices, a: &PlayVag) -> u32 {
        if a.lba_right != 0 {
            tracing::warn!(target: "snd989", "stereo streams are not supported yet (lba {:#x}, {:#x})", a.lba, a.lba_right);
        }
        let Some(slot) = self.slots.iter().position(Option::is_none) else {
            return 0;
        };
        let Some(&ring) = self.rings.get(slot.min(self.rings.len().saturating_sub(1))) else {
            return 0;
        };
        if self.slots.iter().flatten().any(|s| s.ring == ring) {
            return 0;
        }
        let Some(header) = bus.sectors(a.lba, 1) else {
            return 0;
        };
        let Some((data_bytes, rate)) = vag_header(&header, a.offset as usize) else {
            tracing::warn!(target: "snd989", "stream at lba {:#x}: no VAG header", a.lba);
            return 0;
        };
        let Some(granted) = voices.alloc_stream(a.group) else {
            return 0;
        };
        let pitch = ((u64::from(rate) * 4096 / 48000) & 0x3FFF) as u16;
        let mut s = Stream {
            file: u64::from(a.lba) * u64::from(SECTOR) + u64::from(a.offset),
            length: HEADER + data_bytes,
            fed: 0,
            ring,
            half: self.ring_bytes / 2,
            voice: granted.voice,
            group: a.group,
            volume: ((a.volume.min(0x400) * 127) >> 10) as i32,
            pan: i32::from(a.pan),
            pitch,
            held: a.flags & 1 != 0,
            playing: 0,
            age: 0,
            last_queued: false,
            other_ready: false,
        };
        Self::fill(bus, &mut s, ring);
        if !s.last_queued {
            let second = ring + s.half;
            Self::fill(bus, &mut s, second);
            s.other_ready = true;
        }
        let (lr, p) = if s.held {
            ([0, 0], 0)
        } else {
            (Self::lr(voices, &s), pitch)
        };
        voices.key_on_stream(s.voice, lr, p, ring + HEADER, s.group);
        self.words[slot] = ((self.words[slot] & 0xFFFF).wrapping_add(1) & 0xFFFF)
            | ((slot as u32) << 16)
            | HANDLE_TYPE
            | 0x8000_0000;
        self.slots[slot] = Some(s);
        self.words[slot]
    }

    fn lr(voices: &Voices, s: &Stream) -> [i16; 2] {
        let mut lr = [0; 2];
        stream_volume(
            &voices.pan,
            voices.out_mode,
            s.volume,
            s.pan,
            0x7F,
            0x7F,
            0,
            &mut lr,
        );
        lr
    }

    /// `snd_ContinueVAGStream`: a held stream sounds.
    pub(crate) fn resume(&mut self, voices: &mut Voices, handle: u32) {
        let Some(slot) = self.find(handle) else {
            return;
        };
        let lr = self.slots[slot].as_ref().map(|s| Self::lr(voices, s));
        let Some(s) = self.slots[slot].as_mut() else {
            return;
        };
        if !s.held {
            return;
        }
        s.held = false;
        voices.write(reg::voice(s.voice, reg::PITCH), s.pitch);
        voices.stream_volume(s.voice, lr.unwrap_or([0, 0]));
    }

    /// `snd_SetSoundVolPan` on a stream.
    pub(crate) fn set_vol_pan(&mut self, voices: &mut Voices, handle: u32, vol: i32, pan: i32) {
        let Some(slot) = self.find(handle) else {
            return;
        };
        let Some(s) = self.slots[slot].as_mut() else {
            return;
        };
        if vol >= 0 && vol != 0x7FFF_FFFF {
            s.volume = (vol.min(0x400) * 127) >> 10;
        }
        if pan >= 0 {
            s.pan = pan;
        }
        if !s.held {
            let s = s.clone();
            let lr = Self::lr(voices, &s);
            voices.stream_volume(s.voice, lr);
        }
    }

    /// `snd_StopSound` on a stream.
    pub(crate) fn stop(&mut self, voices: &mut Voices, handle: u32) {
        if let Some(slot) = self.find(handle) {
            self.end(voices, slot);
        }
    }

    /// `snd_StopAllStreams`.
    pub(crate) fn stop_all(&mut self, voices: &mut Voices) {
        for slot in 0..SLOTS {
            self.end(voices, slot);
        }
    }

    fn end(&mut self, voices: &mut Voices, slot: usize) {
        if let Some(s) = self.slots[slot].take() {
            voices.stream_release(s.voice);
            self.words[slot] &= !0x8000_0000;
        }
    }

    /// The stream pass of a tick: where each voice is, refills, and ends.
    pub(crate) fn tick(&mut self, bus: &mut dyn Bus, voices: &mut Voices) {
        for slot in 0..SLOTS {
            let Some(mut s) = self.slots[slot].take() else {
                continue;
            };
            s.age += 1;
            if s.age < 2 {
                self.slots[slot] = Some(s);
                continue;
            }
            if s.last_queued && bus.read(reg::voice(s.voice, reg::ENVX), 2) == 0 {
                voices.stream_release(s.voice);
                self.words[slot] &= !0x8000_0000;
                continue;
            }
            let hi = bus.read(reg::address(s.voice, reg::NAX), 2);
            let lo = bus.read(reg::address(s.voice, reg::NAX) + 2, 2);
            let nax = ((hi & 0xF) << 16 | lo) * 2;
            let half = u32::from(nax >= s.ring + s.half);
            if half != s.playing {
                // The voice left the other half: refill it, and send the
                // voice there next.
                s.playing = half;
                s.other_ready = false;
                if !s.last_queued {
                    let other = s.ring + (1 - half) * s.half;
                    Self::fill(bus, &mut s, other);
                    s.other_ready = true;
                    voices.write_address(s.voice, reg::LSAX, other);
                }
            } else if s.age == 2 && s.other_ready {
                voices.write_address(s.voice, reg::LSAX, s.ring + (1 - half) * s.half);
            }
            self.slots[slot] = Some(s);
        }
    }
}
