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

/// One channel of a stream: a file, its ring and its voice.
#[derive(Clone, Debug)]
struct Channel {
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
    /// The channel's pan: 270 degrees for the left of a stereo stream, 90
    /// for its right, 0 for a mono one.
    pan: i32,
    pitch: u16,
    /// The half the voice is in.
    playing: u32,
    /// The last of the data is in the ring.
    last_queued: bool,
    /// The half the voice is not in holds data to play next.
    other_ready: bool,
    /// Its voice has stopped after the last data.
    ended: bool,
}

/// A stream playing: one channel, or two for a stereo stream.
#[derive(Clone, Debug)]
struct Stream {
    channels: Vec<Channel>,
    group: i8,
    volume: i32,
    pan: i32,
    held: bool,
    /// Ticks since the key-on (the first poll comes a tick after it).
    age: u32,
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

    /// Whether the stream's first data is in sound memory. The driver
    /// reads the disc in the background, so right after the stream starts
    /// it is not yet (Sly 1 asks once, at once, and the disc's driver
    /// answers 0); a tick later it is.
    pub(crate) fn buffered(&self, handle: u32) -> u32 {
        self.find(handle)
            .and_then(|slot| self.slots[slot].as_ref())
            .map_or(0, |s| u32::from(s.age >= 1))
    }

    /// Reads `bytes` of the file from `at` into the ring at `to`, the last
    /// block flagged to repeat (the voice jumps to its loop address) or,
    /// at the end of the data, to end.
    fn fill(bus: &mut dyn Bus, s: &mut Channel, to: u32) {
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

    /// `snd_PlayVAGStreamByLoc`: the stream's handle, or 0. A stereo
    /// stream (a second file for the right) takes two rings, a mono one
    /// one; without as many free, the stream does not start (Sly 1 sets up
    /// one ring, so its driver refuses stereo, and so does this one).
    pub(crate) fn play(&mut self, bus: &mut dyn Bus, voices: &mut Voices, a: &PlayVag) -> u32 {
        let Some(slot) = self.slots.iter().position(Option::is_none) else {
            return 0;
        };
        let files: Vec<(u32, i32)> = if a.lba_right == 0 {
            vec![(a.lba, 0)]
        } else {
            vec![(a.lba, 0x10E), (a.lba_right, 0x5A)]
        };
        let busy: Vec<u32> = self
            .slots
            .iter()
            .flatten()
            .flat_map(|s| s.channels.iter().map(|c| c.ring))
            .collect();
        let free: Vec<u32> = self
            .rings
            .iter()
            .copied()
            .filter(|r| !busy.contains(r))
            .take(files.len())
            .collect();
        if free.len() < files.len() {
            return 0;
        }
        let mut channels = Vec::new();
        for (&(lba, pan), &ring) in files.iter().zip(&free) {
            let Some((data_bytes, rate)) = bus
                .sectors(lba, 1)
                .and_then(|h| vag_header(&h, a.offset as usize))
            else {
                tracing::warn!(target: "snd989", "stream at lba {lba:#x}: no VAG header");
                return 0;
            };
            channels.push(Channel {
                file: u64::from(lba) * u64::from(SECTOR) + u64::from(a.offset),
                length: HEADER + data_bytes,
                fed: 0,
                ring,
                half: self.ring_bytes / 2,
                voice: 0,
                pan,
                pitch: ((u64::from(rate) * 4096 / 48000) & 0x3FFF) as u16,
                playing: 0,
                last_queued: false,
                other_ready: false,
                ended: false,
            });
        }
        let mut s = Stream {
            channels,
            group: a.group,
            volume: ((a.volume.min(0x400) * 127) >> 10) as i32,
            pan: i32::from(a.pan),
            held: a.flags & 1 != 0,
            age: 0,
        };
        let mut granted = Vec::new();
        for _ in 0..s.channels.len() {
            if let Some(g) = voices.alloc_stream(a.group) {
                granted.push(g.voice);
            } else {
                for &v in &granted {
                    voices.stream_release(v);
                }
                return 0;
            }
        }
        for (k, voice) in granted.into_iter().enumerate() {
            let held = s.held;
            let lr = if held {
                [0, 0]
            } else {
                Self::lr(voices, &s, k)
            };
            let c = &mut s.channels[k];
            c.voice = voice;
            let ring = c.ring;
            Self::fill(bus, c, ring);
            if !c.last_queued {
                let second = ring + c.half;
                Self::fill(bus, c, second);
                c.other_ready = true;
            }
            let pitch = if held { 0 } else { c.pitch };
            voices.key_on_stream(voice, lr, pitch, ring + HEADER, s.group);
        }
        self.words[slot] = ((self.words[slot] & 0xFFFF).wrapping_add(1) & 0xFFFF)
            | ((slot as u32) << 16)
            | HANDLE_TYPE
            | 0x8000_0000;
        self.slots[slot] = Some(s);
        self.words[slot]
    }

    /// Channel `k`'s left and right.
    fn lr(voices: &Voices, s: &Stream, k: usize) -> [i16; 2] {
        let mut lr = [0; 2];
        stream_volume(
            &voices.pan,
            voices.out_mode,
            s.volume,
            s.pan,
            0x7F,
            0x7F,
            s.channels[k].pan,
            &mut lr,
        );
        lr
    }

    /// `snd_ContinueVAGStream`: a held stream sounds.
    pub(crate) fn resume(&mut self, voices: &mut Voices, handle: u32) {
        let Some(slot) = self.find(handle) else {
            return;
        };
        let Some(s) = self.slots[slot].as_mut() else {
            return;
        };
        if !s.held {
            return;
        }
        s.held = false;
        let s = s.clone();
        for (k, c) in s.channels.iter().enumerate() {
            voices.write(reg::voice(c.voice, reg::PITCH), c.pitch);
            voices.stream_volume(c.voice, Self::lr(voices, &s, k));
        }
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
            for (k, c) in s.channels.iter().enumerate() {
                voices.stream_volume(c.voice, Self::lr(voices, &s, k));
            }
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
            for c in &s.channels {
                voices.stream_release(c.voice);
            }
            self.words[slot] &= !0x8000_0000;
        }
    }

    /// The stream pass of a tick: where each voice is, refills, and ends.
    /// A stream is over when every channel's voice has stopped after its
    /// last data.
    pub(crate) fn tick(&mut self, bus: &mut dyn Bus, voices: &mut Voices) {
        for slot in 0..SLOTS {
            let Some(mut s) = self.slots[slot].take() else {
                continue;
            };
            s.age += 1;
            if s.age >= 2 {
                for c in &mut s.channels {
                    Self::poll(bus, voices, c, s.age);
                }
            }
            if s.channels.iter().all(|c| c.ended) {
                for c in &s.channels {
                    voices.stream_release(c.voice);
                }
                self.words[slot] &= !0x8000_0000;
                continue;
            }
            self.slots[slot] = Some(s);
        }
    }

    /// One channel's poll.
    fn poll(bus: &mut dyn Bus, voices: &mut Voices, c: &mut Channel, age: u32) {
        if c.ended {
            return;
        }
        if c.last_queued && bus.read(reg::voice(c.voice, reg::ENVX), 2) == 0 {
            c.ended = true;
            return;
        }
        let hi = bus.read(reg::address(c.voice, reg::NAX), 2);
        let lo = bus.read(reg::address(c.voice, reg::NAX) + 2, 2);
        let nax = ((hi & 0xF) << 16 | lo) * 2;
        let half = u32::from(nax >= c.ring + c.half);
        if half != c.playing {
            // The voice left the other half: refill it, and send the voice
            // there next.
            c.playing = half;
            c.other_ready = false;
            if !c.last_queued {
                let other = c.ring + (1 - half) * c.half;
                Self::fill(bus, c, other);
                c.other_ready = true;
                voices.write_address(c.voice, reg::LSAX, other);
            }
        } else if age == 2 && c.other_ready {
            voices.write_address(c.voice, reg::LSAX, c.ring + (1 - half) * c.half);
        }
    }
}
