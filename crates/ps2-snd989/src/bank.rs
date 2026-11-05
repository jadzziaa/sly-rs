//! Sound banks as 989snd loads them: a container of two or three chunks
//! (the bank's description, its SPU-ADPCM sample data, and for a music
//! bank its sequences).
//!
//! Two kinds of description:
//!
//! - `SBlk` (version 1), a block of sound effects: each sound is a short
//!   program of 0x28-byte grains (tones to key on, waits, loops, random
//!   choices, LFOs).
//! - `SBv2`, a music bank: sounds that start a sequence (a MIDI file, or a
//!   set of them driven by AME, Sony's interactive music scripting),
//!   programs (instruments) of tones mapped to key ranges, and the
//!   sequences themselves in the third chunk, a `MID ` or `MMID` block.
//!
//! The layouts follow rac-rs's reading of the R&C 989snd (`bank.rs`) and
//! OpenGOAL's loader (`game/sound/989snd/loader.cpp`), checked against Sly
//! 1's banks.

/// Bytes of a grain.
const GRAIN_BYTES: usize = 0x28;
/// Bytes of a sound effect's entry.
const SOUND_BYTES: usize = 12;
/// Bytes of a tone.
const TONE_BYTES: usize = 0x18;
/// Bytes of a music bank's sound entry.
const MUSIC_SOUND_BYTES: usize = 0x1C;

fn u16_at(b: &[u8], at: usize) -> Result<u16, String> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| format!("bank: read at {at:#x} past the end"))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| format!("bank: read at {at:#x} past the end"))
}

fn span(b: &[u8], at: usize, len: usize) -> Result<&[u8], String> {
    at.checked_add(len)
        .and_then(|end| b.get(at..end))
        .ok_or_else(|| format!("bank: {len:#x} bytes at {at:#x} past the end"))
}

/// One tone: a sample with its pitch centre, key range, volume, pan and
/// envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tone {
    /// Voice priority.
    pub(crate) priority: i8,
    /// Volume (negative in a sound effect: a register or a random value).
    pub(crate) volume: i8,
    /// The note the sample plays at its own rate; negative for a 48 kHz
    /// sample, positive for a 44.1 kHz one.
    pub(crate) centre_note: i8,
    /// Fine tuning, added, in 1/128 of a semitone.
    pub(crate) centre_fine: i8,
    /// Pan in degrees (negative in a sound effect: a register or random).
    pub(crate) pan: i16,
    /// The key range a program's tone covers.
    pub(crate) map_low: u8,
    pub(crate) map_high: u8,
    /// Pitch-bend range in semitones, below and above.
    pub(crate) bend_down: i8,
    pub(crate) bend_up: i8,
    /// The envelope, as the SPU2's ADSR registers take it.
    pub(crate) adsr1: u16,
    pub(crate) adsr2: u16,
    /// Bit 0: send to the reverb.
    pub(crate) flags: u16,
    /// Where the sample starts in the bank's sample data.
    pub(crate) sample: u32,
}

impl Tone {
    fn parse(b: &[u8]) -> Result<Self, String> {
        let b = span(b, 0, 0x14)?;
        Ok(Self {
            priority: b[0] as i8,
            volume: b[1] as i8,
            centre_note: b[2] as i8,
            centre_fine: b[3] as i8,
            pan: u16_at(b, 4)? as i16,
            map_low: b[6],
            map_high: b[7],
            bend_down: b[8] as i8,
            bend_up: b[9] as i8,
            adsr1: u16_at(b, 10)?,
            adsr2: u16_at(b, 12)?,
            flags: u16_at(b, 14)?,
            sample: u32_at(b, 16)?,
        })
    }

    /// Whether the tone is sent to the reverb.
    pub(crate) fn reverb(&self) -> bool {
        self.flags & 1 != 0
    }
}

/// One grain of a sound effect: its type, the ticks to wait after it, and
/// 32 bytes of operands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Grain {
    pub(crate) kind: u32,
    /// The driver reads the low half of the delay word.
    pub(crate) delay: u16,
    pub(crate) p: [u8; 32],
}

impl Grain {
    pub(crate) fn u8_at(&self, at: usize) -> u8 {
        self.p[at]
    }

    pub(crate) fn s16_at(&self, at: usize) -> i16 {
        i16::from_le_bytes([self.p[at], self.p[at + 1]])
    }

    pub(crate) fn u16_at(&self, at: usize) -> u16 {
        u16::from_le_bytes([self.p[at], self.p[at + 1]])
    }

    pub(crate) fn u32_at(&self, at: usize) -> u32 {
        u32::from_le_bytes([self.p[at], self.p[at + 1], self.p[at + 2], self.p[at + 3]])
    }

    /// The tone of a tone grain (type 1).
    pub(crate) fn tone(&self) -> Option<Tone> {
        (self.kind == 1)
            .then(|| Tone::parse(&self.p).ok())
            .flatten()
    }
}

/// A sound effect: its volume, volume group, pan, flags and grains.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Effect {
    pub(crate) volume: i8,
    pub(crate) group: u8,
    pub(crate) pan: u16,
    /// Bit 1: one instance at a time; bit 2: pan scaled by the volume.
    pub(crate) flags: u16,
    pub(crate) grains: Vec<Grain>,
}

/// A music bank's sound: which sequence it plays and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Song {
    /// 4: a MIDI sequence, 5: an AME set of sequences.
    pub(crate) kind: i32,
    pub(crate) volume: i16,
    /// Times to play (0 loops for ever).
    pub(crate) repeats: i8,
    pub(crate) group: i8,
    pub(crate) pan: i16,
}

/// An instrument of a music bank: tones by key range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Program {
    pub(crate) volume: i8,
    pub(crate) pan: i16,
    pub(crate) tones: Vec<Tone>,
}

/// A MIDI sequence: its events, tempo and resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sequence {
    /// The bytes from its header on.
    pub(crate) data: Vec<u8>,
    /// Where the events start in `data`.
    pub(crate) start: usize,
    /// Microseconds per quarter note.
    pub(crate) tempo: u32,
    /// Ticks per quarter note.
    pub(crate) ppq: u32,
    /// In an AME set: the handle word naming the segment's handler type
    /// (bits 24..28: 1 or 3 are MIDI segments; others are not played).
    pub(crate) handle: u32,
}

impl Sequence {
    fn parse(b: &[u8], handle: bool) -> Result<Self, String> {
        if span(b, 0, 4)? != b"MID " {
            return Err("music bank: not a MID block".to_owned());
        }
        Ok(Self {
            start: u32_at(b, 0x18)? as usize,
            tempo: u32_at(b, 0x20)?,
            ppq: u32_at(b, 0x24)?,
            handle: if handle {
                u32_at(b, 0x28)?
            } else {
                0x0100_0000
            },
            data: b.to_vec(),
        })
    }
}

/// What a bank holds besides its samples.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Content {
    /// Sound effects.
    Effects(Vec<Effect>),
    /// Music: songs, programs, and the sequences (one `MID `, or the
    /// segments of an `MMID`).
    Music {
        songs: Vec<Song>,
        programs: Vec<Program>,
        sequences: Vec<Sequence>,
        multi: bool,
    },
}

/// A parsed bank.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Bank {
    pub(crate) content: Content,
    /// The SPU-ADPCM sample data.
    pub(crate) samples: Vec<u8>,
    /// Where the samples sit in sound memory (bytes), once uploaded.
    pub(crate) base: u32,
}

impl Bank {
    /// The total length of a container, from its first sectors.
    pub(crate) fn container_len(header: &[u8]) -> Result<usize, String> {
        let chunks = u32_at(header, 4)? as usize;
        if !(2..=3).contains(&chunks) {
            return Err(format!("sound bank: {chunks} chunks"));
        }
        let mut end = 0;
        for c in 0..chunks {
            let (at, len) = (u32_at(header, 8 + 8 * c)?, u32_at(header, 12 + 8 * c)?);
            end = end.max(at as usize + len as usize);
        }
        Ok(end)
    }

    /// Parses a container (trailing sector padding is ignored).
    pub(crate) fn parse(container: &[u8]) -> Result<Self, String> {
        let chunks = u32_at(container, 4)? as usize;
        let chunk = |c: usize| -> Result<&[u8], String> {
            let (at, len) = (
                u32_at(container, 8 + 8 * c)?,
                u32_at(container, 12 + 8 * c)?,
            );
            span(container, at as usize, len as usize)
        };
        let meta = chunk(0)?;
        let samples = chunk(1)?.to_vec();
        let content = match span(meta, 0, 4)? {
            b"SBlk" => Content::Effects(Self::effects(meta)?),
            b"SBv2" if chunks == 3 => Self::music(meta, chunk(2)?)?,
            other => {
                return Err(format!(
                    "sound bank: unknown block {:?}",
                    String::from_utf8_lossy(other)
                ));
            }
        };
        Ok(Self {
            content,
            samples,
            base: 0,
        })
    }

    fn effects(meta: &[u8]) -> Result<Vec<Effect>, String> {
        let sounds = u16_at(meta, 0x16)? as usize;
        let sound_table = u32_at(meta, 0x1C)? as usize;
        let grain_table = u32_at(meta, 0x20)? as usize;
        let mut out = Vec::with_capacity(sounds);
        for i in 0..sounds {
            let e = span(meta, sound_table + i * SOUND_BYTES, SOUND_BYTES)?;
            let (count, first) = (e[4] as usize, u32_at(e, 8)? as usize);
            let mut grains = Vec::with_capacity(count);
            for g in 0..count {
                let b = span(meta, grain_table + first + g * GRAIN_BYTES, GRAIN_BYTES)?;
                let mut p = [0; 32];
                p.copy_from_slice(&b[8..]);
                grains.push(Grain {
                    kind: u32_at(b, 0)?,
                    delay: u16_at(b, 4)?,
                    p,
                });
            }
            out.push(Effect {
                volume: e[0] as i8,
                group: e[1],
                pan: u16_at(e, 2)?,
                flags: u16_at(e, 6)?,
                grains,
            });
        }
        Ok(out)
    }

    fn music(meta: &[u8], midi: &[u8]) -> Result<Content, String> {
        let sounds = u16_at(meta, 0x14)? as usize;
        let programs = u16_at(meta, 0x16)? as usize;
        let first_sound = u32_at(meta, 0x1C)? as usize;
        let first_program = u32_at(meta, 0x20)? as usize;
        let mut songs = Vec::with_capacity(sounds);
        for i in 0..sounds {
            let e = span(meta, first_sound + i * MUSIC_SOUND_BYTES, MUSIC_SOUND_BYTES)?;
            songs.push(Song {
                kind: u32_at(e, 0)? as i32,
                volume: u16_at(e, 0x10)? as i16,
                repeats: e[0x12] as i8,
                group: e[0x13] as i8,
                pan: u16_at(e, 0x14)? as i16,
            });
        }
        let mut progs = Vec::with_capacity(programs);
        for i in 0..programs {
            let e = span(meta, first_program + i * 8, 8)?;
            let (count, first) = (e[0] as usize, u32_at(e, 4)? as usize);
            let tones = (0..count)
                .map(|t| Tone::parse(span(meta, first + t * TONE_BYTES, TONE_BYTES)?))
                .collect::<Result<Vec<_>, _>>()?;
            progs.push(Program {
                volume: e[1] as i8,
                pan: u16_at(e, 2)? as i16,
                tones,
            });
        }
        // The sequence chunk is a container of its own: its first chunk
        // is the `MID ` or `MMID` block.
        let at = u32_at(midi, 8)? as usize;
        let block = span(midi, at, midi.len().saturating_sub(at))?;
        let (sequences, multi) = match span(block, 0, 4)? {
            b"MID " => (vec![Sequence::parse(block, false)?], false),
            b"MMID" => {
                let count = block.get(7).copied().unwrap_or(0) as usize;
                let mut list = Vec::with_capacity(count);
                for s in 0..count {
                    let offset = u32_at(block, 0x10 + 4 * s)? as usize;
                    list.push(Sequence::parse(
                        span(block, offset, block.len() - offset.min(block.len()))?,
                        true,
                    )?);
                }
                (list, true)
            }
            _ => return Err("music bank: no MID or MMID block".to_owned()),
        };
        Ok(Content::Music {
            songs,
            programs: progs,
            sequences,
            multi,
        })
    }

    /// Whether sound `index` loops (`snd_IsSoundALooper`): a sound effect
    /// with a loop end in its program, or a song that repeats for ever.
    pub(crate) fn looper(&self, index: usize) -> bool {
        match &self.content {
            Content::Effects(sounds) => sounds
                .get(index)
                .is_some_and(|s| s.grains.iter().any(|g| g.kind == 22)),
            Content::Music { songs, .. } => songs.get(index).is_some_and(|s| s.repeats == 0),
        }
    }
}

/// The SPU2 memory 989snd hands out, as its allocator does: one free list
/// from 0x5040 to the end, sizes rounded up to 0x40, first fit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpuMemory {
    /// Free regions (start, end), sorted.
    free: Vec<(u32, u32)>,
}

impl Default for SpuMemory {
    fn default() -> Self {
        Self {
            free: vec![(0x5040, 0x20_0000)],
        }
    }
}

impl SpuMemory {
    fn round(size: u32) -> u32 {
        (size.wrapping_add(0x3F) & !0x3F).max(0x40)
    }

    /// A block of `size` bytes, first fit.
    pub(crate) fn alloc(&mut self, size: u32) -> Option<u32> {
        let n = Self::round(size);
        let i = self.free.iter().position(|(a, b)| b - a >= n)?;
        let a = self.free[i].0;
        if self.free[i].1 - a == n {
            self.free.remove(i);
        } else {
            self.free[i].0 += n;
        }
        Some(a)
    }

    /// Gives a block back, merging it with its neighbours.
    pub(crate) fn release(&mut self, address: u32, size: u32) {
        let n = Self::round(size);
        let at = self.free.partition_point(|r| r.0 < address);
        self.free.insert(at, (address, address + n));
        let mut k = 0;
        while k + 1 < self.free.len() {
            if self.free[k].1 == self.free[k + 1].0 {
                self.free[k].1 = self.free[k + 1].1;
                self.free.remove(k + 1);
            } else {
                k += 1;
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A container with one sound effect of three grains: a loop start, a
    /// tone, and a loop end that waits 100 ticks.
    pub(crate) fn effect_bank() -> Vec<u8> {
        let mut meta = vec![0u8; 0x40];
        meta[..4].copy_from_slice(b"SBlk");
        meta[4] = 1;
        meta[0x16] = 1;
        meta[0x18] = 3;
        meta[0x1C] = 0x40;
        meta[0x20] = 0x4C;
        // The sound: volume 100, group 0, pan 0, three grains from 0.
        meta.extend_from_slice(&[100, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0]);
        let mut start = vec![0u8; GRAIN_BYTES];
        start[0] = 21;
        meta.extend_from_slice(&start);
        let mut tone = vec![0u8; GRAIN_BYTES];
        tone[0] = 1;
        tone[8] = 10; // priority
        tone[9] = 127; // volume
        tone[10] = 0xC4; // centre note -60
        tone[18] = 0xFF; // adsr1
        tone[24] = 0x20; // sample at 0x20
        meta.extend_from_slice(&tone);
        let mut end = vec![0u8; GRAIN_BYTES];
        end[0] = 22;
        end[4] = 100;
        meta.extend_from_slice(&end);
        let samples = vec![0u8; 0x40];
        let mut out = vec![0u8; 0x18];
        out[0] = 3;
        out[4] = 2;
        out[8] = 0x18;
        out[12..16].copy_from_slice(&(meta.len() as u32).to_le_bytes());
        let data_at = 0x18 + meta.len() as u32;
        out[16..20].copy_from_slice(&data_at.to_le_bytes());
        out[20..24].copy_from_slice(&(samples.len() as u32).to_le_bytes());
        out.extend_from_slice(&meta);
        out.extend_from_slice(&samples);
        out
    }

    #[test]
    fn parses_an_effect_bank() {
        let raw = effect_bank();
        assert_eq!(Bank::container_len(&raw), Ok(raw.len()));
        let bank = Bank::parse(&raw).unwrap_or_else(|e| panic!("{e}"));
        let Content::Effects(sounds) = &bank.content else {
            panic!("not an effect bank");
        };
        assert_eq!(sounds.len(), 1);
        assert_eq!(sounds[0].volume, 100);
        let tone = sounds[0].grains[1].tone();
        assert_eq!(tone.as_ref().map(|t| t.centre_note), Some(-60));
        assert_eq!(tone.map(|t| t.sample), Some(0x20));
        assert!(bank.looper(0));
        assert!(!bank.looper(1));
    }

    #[test]
    fn allocates_first_fit_and_merges() {
        let mut m = SpuMemory::default();
        assert_eq!(m.alloc(749_312), Some(0x5040));
        assert_eq!(m.alloc(0x2000), Some(0xB_BF40));
        assert_eq!(m.alloc(588_096), Some(0xB_DF40));
        assert_eq!(m.alloc(451_184), Some(0x14_D880));
        m.release(0xB_BF40, 0x2000);
        assert_eq!(m.alloc(0x1000), Some(0xB_BF40));
    }
}
