//! Sony's 989snd sound driver, in Rust: what the disc's `989SND.IRX` does
//! on the IOP for the game's `snd_*` calls, without running it.
//!
//! The EE's sound library sends its calls over SIF RPC, one at a time or
//! in batches (function 0x4D: a count, then commands of `{id: u16, bytes:
//! u16, data}`); [`Driver::call`] answers them. The driver keeps the
//! loaded banks and their sample data in SPU2 memory, plays sound effects
//! (grain programs), music (MIDI and AME sequences), VAG streams from the
//! disc and movie sound (the SPU2's auto-DMA input), and drives the SPU2
//! through its registers, as the real driver does through `libsd`. It
//! runs on emulated time: [`Driver::tick`] is the driver's 240 Hz timer,
//! and register writes reach the chip ([`Bus`]) at the moment they are
//! made.
//!
//! Sly 1's driver (989snd built 21 August 2002) is the reference: its call
//! set and register traffic were traced under the classic path (the disc's
//! driver on the emulated IOP) and are reproduced here. The sound-effect
//! interpreter and voice layer are ported from rac-rs (GPL-3.0, an exact
//! port of the driver in Ratchet & Clank, whose build is close to Sly's);
//! the music sequencers follow OpenGOAL's 989snd (ISC).
//!
//! Not covered: the driver's own disc reads for the game
//! (`snd_StreamSafeCdRead`, which Sly 1 does not use), and the time the
//! disc takes (a bank's or a stream's data arrive at once). `snd_AutoVol`
//! is checked against the one call Sly 1 makes; stereo streams only
//! against the disc driver's refusal (Sly 1 sets up one stream ring).

// The driver works in the IOP's 16- and 32-bit words, as the module does;
// the casts are its truncations.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::cast_precision_loss,
    reason = "the driver's 16- and 32-bit arithmetic"
)]
#![allow(
    clippy::many_single_char_names,
    reason = "the driver's own short names (slot, grain, voice), as in the ports it follows"
)]
#![allow(clippy::doc_markdown, reason = "project names (OpenGOAL, rac-rs)")]

use std::collections::BTreeMap;
use std::rc::Rc;

mod bank;
mod effects;
mod music;
mod reverb;
mod stream;
mod tables;
mod voices;

use bank::{Bank, Content, SpuMemory};
use tables::{R250, R250_LEN};
use voices::{Owner, VoiceEnd, Voices, reg};

/// The driver's command server.
pub const SERVER: u32 = 0x0012_3456;
/// The driver's loading server (banks).
pub const LOADER: u32 = 0x0012_3457;
/// IOP cycles per driver tick: the timer runs at 240 Hz.
pub const TICK_CYCLES: u64 = 36_864_000 / 240;

/// The batch function: several commands in one call.
const BATCH: u32 = 0x4D;

/// What the driver reaches: the IOP's hardware registers (the SPU2's and
/// the DMA channels'), sound memory, the disc and IOP memory.
pub trait Bus {
    /// A register read (`bytes` 2 or 4).
    fn read(&mut self, address: u32, bytes: u32) -> u32;
    /// A register write.
    fn write(&mut self, address: u32, bytes: u32, value: u32);
    /// Puts `data` into sound memory at `address` (bytes), as a DMA
    /// transfer would.
    fn upload(&mut self, address: u32, data: &[u8]);
    /// Reads `count` disc sectors from `lba`.
    fn sectors(&mut self, lba: u32, count: u32) -> Option<Vec<u8>>;
    /// A block of IOP memory for the EE to send movie sound to.
    fn alloc_iop(&mut self, bytes: u32) -> Option<u32>;
}

/// Movie sound: the EE streams it into a buffer in IOP memory, which the
/// driver feeds to core 0's auto-DMA input, half a buffer at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MovieState {
    Idle,
    /// The first kilobyte, sent once when the movie sound is set up.
    Priming,
    Playing,
}

#[derive(Clone, Copy, Debug)]
struct Movie {
    buffer: u32,
    size: u32,
    volume: i32,
    group: i32,
    state: MovieState,
    /// The half to send next.
    next: u32,
}

/// The driver.
pub struct Driver {
    voices: Voices,
    effects: effects::Effects,
    music: music::Music,
    streams: stream::Streams,
    rng: R250,
    banks: BTreeMap<u32, Rc<Bank>>,
    /// Sound memory, and where each bank's samples are.
    memory: SpuMemory,
    placed: BTreeMap<u32, (u32, u32)>,
    next_handle: u32,
    /// The reply buffer: results stay in it from call to call.
    reply: Vec<u32>,
    /// Each core's MMIX, as last written.
    mmix: [u16; 2],
    /// Each core's ATTR, as last written.
    attr: [u16; 2],
    reverb: reverb::Reverb,
    movie: Option<Movie>,
    ticks: u64,
}

impl Default for Driver {
    fn default() -> Self {
        Self::new(None, None)
    }
}

/// The R250 generator's table and index in a 989snd module image: found
/// through the generator's code (`slti $2, $3, 0x93` after loading the
/// index, `lhu ... (table)` after it).
fn r250_of(image: &[u8]) -> Option<([u16; R250_LEN], usize)> {
    let word = |at: usize| {
        image
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let at = (0..image.len().saturating_sub(4))
        .step_by(4)
        .find(|&a| word(a) == Some(0x2862_0093))?;
    let (lui, lh) = (word(at.checked_sub(12)?)?, word(at.checked_sub(8)?)?);
    if lui >> 16 != 0x3C03 || lh >> 16 != 0x8463 {
        return None;
    }
    let high = (lui & 0xFFFF) << 16;
    let index_at = high.wrapping_add(lh & 0xFFFF ^ 0x8000).wrapping_sub(0x8000) as usize;
    let table_lo = (at..at + 0x40)
        .step_by(4)
        .filter_map(word)
        .find(|w| w >> 16 == 0x9423)?;
    let table_at = high
        .wrapping_add(table_lo & 0xFFFF ^ 0x8000)
        .wrapping_sub(0x8000) as usize;
    let index = image.get(index_at..index_at + 2)?;
    let index = i16::from_le_bytes([index[0], index[1]]) as usize;
    let raw = image.get(table_at..table_at + 2 * R250_LEN)?;
    let mut table = [0; R250_LEN];
    for (k, t) in table.iter_mut().enumerate() {
        *t = u16::from_le_bytes([raw[2 * k], raw[2 * k + 1]]);
    }
    Some((table, index))
}

impl Driver {
    /// A driver, its random numbers seeded from the disc's module image
    /// (`989SND.IRX` as loaded, at address 0), its reverb presets from
    /// libsd's (`LIBSD.IRX`), if given.
    #[must_use]
    pub fn new(module: Option<&[u8]>, libsd: Option<&[u8]>) -> Self {
        let presets = libsd.and_then(reverb::Presets::from_libsd).unwrap_or_else(|| {
            tracing::warn!(target: "snd989", "no libsd image: reverb types other than off are silent");
            reverb::Presets::off_only()
        });
        let found = module.and_then(r250_of);
        if let Some(([first, ..], index)) = &found {
            tracing::debug!(target: "snd989", "random numbers from the module: index {index}, first {first:#x}");
        }
        let (table, index) = found.unwrap_or_else(|| {
            tracing::warn!(target: "snd989", "no 989snd module image: the driver's random numbers start from zeros");
            ([0; R250_LEN], 0)
        });
        let mut voices = Voices::new();
        voices.set_voice_mode(0);
        Self {
            voices,
            effects: effects::Effects::new(),
            music: music::Music::new(),
            streams: stream::Streams::new(),
            rng: R250::new(&table, index),
            banks: BTreeMap::new(),
            memory: SpuMemory::default(),
            placed: BTreeMap::new(),
            next_handle: 0x001F_0000,
            reply: vec![0; 0x400],
            mmix: [0xF00, 0xF0C],
            attr: [0xC000, 0xC001],
            reverb: reverb::Reverb::new(presets),
            movie: None,
            ticks: 0,
        }
    }

    /// Ticks run so far.
    #[must_use]
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// Hands the queued register writes to the chip.
    fn flush(&mut self, bus: &mut dyn Bus) {
        for (address, value) in self.voices.writes.drain(..) {
            bus.write(address, 2, u32::from(value));
        }
    }

    /// The driver's timer: one 240 Hz tick. The end-of-voice poll, then
    /// every sound effect, song and stream, then the key-offs and key-ons,
    /// and every other tick the LFOs.
    pub fn tick(&mut self, bus: &mut dyn Bus) {
        self.ticks += 1;
        let polled: Vec<(usize, u16)> = self
            .voices
            .polled()
            .into_iter()
            .map(|v| (v, bus.read(reg::voice(v, reg::ENVX), 2) as u16))
            .collect();
        let ends = self.voices.poll_end(&polled);
        self.dispatch(ends);
        self.voices.tick = self.voices.tick.wrapping_add(1);
        self.effects.tick(&mut self.voices, &mut self.rng);
        let stolen = self.music.tick(&mut self.voices);
        self.dispatch(stolen);
        self.streams.tick(bus, &mut self.voices);
        self.voices.flush_key_offs();
        self.voices.flush_key_ons();
        if self.voices.tick.is_multiple_of(2) {
            self.effects.tick_lfos(&mut self.voices, &mut self.rng);
        }
        let mut ops = Vec::new();
        self.reverb.tick(&mut ops);
        self.apply_reverb(bus, ops);
        self.flush(bus);
    }

    /// Voices that ended, to their owners.
    fn dispatch(&mut self, ends: Vec<VoiceEnd>) {
        for e in ends {
            match e.owner {
                Owner::Effect(_) => self.effects.voice_end(e),
                Owner::Music(_) => self.music.voice_end(e),
            }
        }
    }

    /// An IOP DMA channel feeding the SPU2 finished a transfer (`channel`
    /// 4 or 7): the next half of the movie sound goes.
    pub fn dma_done(&mut self, channel: u32, bus: &mut dyn Bus) {
        if channel != 4 {
            return;
        }
        let Some(m) = self.movie.as_mut() else { return };
        match m.state {
            MovieState::Priming => {
                bus.write(0x1F90_019A, 2, 0xC000);
                bus.write(0x1F90_01B0, 2, 0);
                m.state = MovieState::Idle;
            }
            MovieState::Playing => {
                let half = m.size / 2;
                bus.write(0x1F80_10C6, 2, half / 64);
                bus.write(0x1F80_10C0, 4, m.next);
                bus.write(0x1F80_10C8, 4, 0x0100_0201);
                m.next = if m.next == m.buffer {
                    m.buffer + half
                } else {
                    m.buffer
                };
            }
            MovieState::Idle => {}
        }
    }

    /// Answers an RPC call to the driver's servers; the reply's bytes.
    pub fn call(
        &mut self,
        sid: u32,
        function: u32,
        data: &[u8],
        reply_size: u32,
        bus: &mut dyn Bus,
    ) -> Vec<u8> {
        let words: Vec<u32> = data
            .chunks(4)
            .map(|c| {
                let mut w = [0; 4];
                w[..c.len()].copy_from_slice(c);
                u32::from_le_bytes(w)
            })
            .collect();
        if sid == LOADER {
            let handle = if function == 3 {
                self.load_bank(bus, words.first().copied().unwrap_or(0))
            } else {
                tracing::warn!(target: "snd989", "loader function {function} not supported");
                0
            };
            self.flush(bus);
            return handle.to_le_bytes().to_vec();
        }
        self.reply[0] = u32::MAX;
        if function == BATCH {
            let count = words.first().copied().unwrap_or(0) as usize;
            let mut at = 4;
            for k in 0..count.min(self.reply.len() - 2) {
                let Some(head) = data.get(at..at + 4) else {
                    break;
                };
                let id = u32::from(u16::from_le_bytes([head[0], head[1]]));
                let len = i16::from_le_bytes([head[2], head[3]]).max(0) as usize;
                let mut d = [0u32; 8];
                for (j, w) in d.iter_mut().enumerate() {
                    let o = at + 4 + 4 * j;
                    if 4 * j < len
                        && let Some(b) = data.get(o..o + 4)
                    {
                        *w = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                    }
                }
                if let Some(r) = self.command(bus, id, &d) {
                    self.reply[1 + k] = r;
                }
                at += 4 + len.div_ceil(4) * 4;
            }
            let end = (count + 1).min(self.reply.len() - 1);
            self.reply[end] = u32::MAX;
        } else {
            let mut d = [0u32; 8];
            for (w, &v) in d.iter_mut().zip(&words) {
                *w = v;
            }
            if let Some(r) = self.command(bus, function, &d) {
                self.reply[1] = r;
            }
            self.reply[2] = u32::MAX;
        }
        self.flush(bus);
        self.reply
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .take(reply_size as usize)
            .collect()
    }

    /// One command; its result, for the commands that return one.
    #[allow(clippy::too_many_lines, reason = "one arm per command")]
    fn command(&mut self, bus: &mut dyn Bus, id: u32, d: &[u32; 8]) -> Option<u32> {
        let kind = |h: u32| (h >> 24) & 0x1F;
        tracing::debug!(target: "snd989", "command {id} {:x?}", &d[..4]);
        match id {
            0 => {
                self.init(bus);
                None
            }
            6 => {
                self.unload_bank(d[0]);
                None
            }
            9 => {
                self.voices.set_group_volume(d[0] as i32, d[1] as i32);
                if let Some(m) = self.movie
                    && m.state == MovieState::Playing
                    && m.group == d[0] as i32
                {
                    self.movie_volume(bus);
                }
                None
            }
            10 => Some(self.voices.masters.get(d[0] as usize).copied().unwrap_or(0) as u32),
            11 => {
                self.voices.out_mode = d[0] as i16;
                None
            }
            13 => {
                self.voices.set_voice_mode(d[0] as i32);
                None
            }
            14 => {
                self.reverb_type(bus, d[0], d[1] as i32);
                None
            }
            15 => {
                let mut ops = Vec::new();
                self.reverb
                    .set_depth(d[0], d[1] as u16, d[2] as u16, &mut ops);
                self.apply_reverb(bus, ops);
                None
            }
            17 => Some(self.play(d)),
            19 | 20 => {
                let pause = id == 19;
                match kind(d[0]) {
                    5 if pause => self.effects.pause(&mut self.voices, d[0]),
                    5 => self.effects.resume(&mut self.voices, d[0]),
                    1 | 2 => self.music.set_paused(&mut self.voices, d[0], pause),
                    _ => {}
                }
                None
            }
            21 => {
                match kind(d[0]) {
                    5 => self.effects.stop(&mut self.voices, &mut self.rng, d[0]),
                    4 => self.streams.stop(&mut self.voices, d[0]),
                    1 | 2 => self.music.stop(&mut self.voices, d[0]),
                    _ => {}
                }
                None
            }
            22 | 23 => {
                if id == 22 {
                    self.effects.pause_groups(&mut self.voices, d[0]);
                } else {
                    self.effects.resume_groups(&mut self.voices, d[0]);
                }
                self.music.pause_groups(&mut self.voices, d[0], id == 22);
                None
            }
            24 => {
                self.effects.stop_all(&mut self.voices, &mut self.rng);
                self.music.stop_all(&mut self.voices);
                None
            }
            25 => Some(match d[0] {
                0 => 0,
                u32::MAX => u32::MAX,
                h => match kind(h) {
                    5 => self.effects.playing(h),
                    4 => self.streams.playing(h),
                    1 | 2 => self.music.playing(h),
                    _ => 0,
                },
            }),
            26 => Some(u32::from(
                self.banks
                    .get(&d[0])
                    .is_some_and(|b| b.looper(d[1] as usize)),
            )),
            27 => {
                let (vol, pan) = (d[1] as i32, d[2] as i32);
                match kind(d[0]) {
                    5 => self.effects.set_vol_pan(&mut self.voices, d[0], vol, pan),
                    4 => self.streams.set_vol_pan(&mut self.voices, d[0], vol, pan),
                    1 | 2 => self.music.set_vol_pan(&mut self.voices, d[0], vol, pan),
                    _ => {}
                }
                None
            }
            28 => Some(self.original_pitch(d[0], d[1] as usize)),
            30 => {
                if kind(d[0]) == 5 {
                    self.effects.set_note(&mut self.voices, d[0], d[1]);
                }
                None
            }
            31 => {
                if kind(d[0]) == 5 {
                    self.effects
                        .set_bend_of(&mut self.voices, d[0], d[1] as i16);
                }
                None
            }
            34 => {
                if kind(d[0]) == 5 {
                    self.effects.auto_volume(d[0], d[1] as i32, d[2] as i32);
                }
                None
            }
            39 => {
                self.music.excite = d[0] as u8;
                None
            }
            40 => Some(self.music.register(d[0], d[1] as usize)),
            41 => {
                self.music.set_register(d[0], d[1] as usize, d[2] as u8);
                None
            }
            42 => {
                self.streams.init(&mut self.memory, d[0], d[1]);
                Some(1)
            }
            44 => {
                let a = stream::PlayVag::from_words(&[d[0], d[1], d[2], d[3], d[4], d[5], d[6]]);
                let h = self.streams.play(bus, &mut self.voices, &a);
                Some(h)
            }
            46 => {
                self.streams.resume(&mut self.voices, d[0]);
                None
            }
            52 => {
                self.streams.stop_all(&mut self.voices);
                None
            }
            54 => Some(1),
            59 => Some(self.movie_init(bus, d)),
            60 | 61 => {
                self.movie_stop(bus, id == 61);
                None
            }
            62 => {
                self.movie_start(bus, d);
                None
            }
            64 => {
                self.flush(bus);
                Some(bus.read(0x1F80_10C0, 4))
            }
            78 => {
                self.voices
                    .set_group_range(d[0] as usize, d[1] as i32, d[2] as i32);
                None
            }
            79 => Some(self.streams.buffered(d[0])),
            // ResolveBankXREFS, and PreAllocReverbWorkArea (the reverb
            // work area of the only type used, off, is the tiny one at
            // the top of sound memory).
            81 => {
                // The reserving core's MMIX effect bits go on first.
                for c in 0..2usize {
                    if d[0] & (1 << c) != 0 {
                        self.mmix[c] |= 3;
                        let mmix = self.mmix[c];
                        self.voices.write(0x1F90_0198 + 0x400 * c as u32, mmix);
                    }
                }
                let mut ops = Vec::new();
                self.reverb
                    .reserve(&mut self.memory, d[0], d[1] as i32, &mut ops);
                self.apply_reverb(bus, ops);
                None
            }
            8 => None,
            _ => {
                tracing::warn!(target: "snd989", "command {id} not supported ({:x?})", &d[..4]);
                None
            }
        }
    }

    /// `snd_GetSoundOriginalPitch(bank, sound)`: the note a sound effect
    /// plays its tones at, in 1/128ths of a semitone: 60 << 7 (what Sly 1's
    /// driver answers), the scale `snd_SetSoundPitch` works in.
    fn original_pitch(&self, bank: u32, sound: usize) -> u32 {
        match self.banks.get(&bank).map(|b| &b.content) {
            Some(Content::Effects(sounds)) if sound < sounds.len() => 60 << 7,
            _ => 0,
        }
    }

    /// `snd_PlaySoundVolPanPMPB(bank, sound, volume, pan, pitch mod,
    /// bend)`.
    fn play(&mut self, d: &[u32; 8]) -> u32 {
        let Some(bank) = self.banks.get(&d[0]).cloned() else {
            return 0;
        };
        match &bank.content {
            Content::Effects(_) => {
                let play = effects::Play {
                    scale: d[2] as i32,
                    pan: d[3] as i32,
                    pitch_mod: d[4] as i16,
                    bend: d[5] as i16,
                };
                self.effects.play(
                    &mut self.voices,
                    &mut self.rng,
                    d[0],
                    &bank,
                    d[1] as i32,
                    &play,
                    None,
                    [0; 4],
                )
            }
            Content::Music { .. } => self.music.play(
                d[0],
                &bank,
                d[1] as i32,
                d[2] as i32,
                d[3] as i32,
                d[4] as i16,
            ),
        }
    }

    // ------------------------------------------------ banks

    /// `snd_BankLoadByLoc(lba, 0)`: reads the bank from the disc, puts its
    /// samples in sound memory; its handle, or 0.
    fn load_bank(&mut self, bus: &mut dyn Bus, lba: u32) -> u32 {
        let parsed = bus
            .sectors(lba, 1)
            .ok_or_else(|| "disc read failed".to_owned())
            .and_then(|first| Bank::container_len(&first))
            .and_then(|len| {
                bus.sectors(lba, len.div_ceil(0x800) as u32)
                    .ok_or_else(|| "disc read failed".to_owned())
            })
            .and_then(|all| Bank::parse(&all));
        let mut bank = match parsed {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(target: "snd989", "bank at lba {lba:#x}: {e}");
                return 0;
            }
        };
        let size = bank.samples.len() as u32;
        let Some(base) = self.memory.alloc(size) else {
            tracing::warn!(target: "snd989", "bank at lba {lba:#x}: sound memory is full");
            return 0;
        };
        bank.base = base;
        bus.upload(base, &bank.samples);
        let handle = self.next_handle;
        self.next_handle += 0x100;
        self.placed.insert(handle, (base, size));
        self.banks.insert(handle, Rc::new(bank));
        tracing::debug!(target: "snd989", "bank at lba {lba:#x}: handle {handle:#x}, samples at {base:#x} ({size:#x} bytes)");
        handle
    }

    fn unload_bank(&mut self, handle: u32) {
        if self.banks.remove(&handle).is_none() {
            return;
        }
        self.effects
            .stop_bank(&mut self.voices, &mut self.rng, handle);
        self.music.stop_bank(&mut self.voices, handle);
        self.effects.forget_bank(handle);
        if let Some((base, size)) = self.placed.remove(&handle) {
            self.memory.release(base, size);
        }
    }

    // ------------------------------------------------ setup

    /// The driver's start: `sceSdInit` (libsd), then 989snd's own setup of
    /// the cores, as Sly 1's driver writes them.
    #[allow(clippy::too_many_lines, reason = "the register writes, in order")]
    fn init(&mut self, bus: &mut dyn Bus) {
        // libsd: the SPU2's bus and DMA set-up.
        for (a, v) in [
            (0x1F80_1404, 0xBF90_0000),
            (0x1F80_140C, 0xBF90_0800),
            (0x1F80_10F0, 0x0008_0000),
            (0x1F80_1570, 0x8),
            (0x1F80_1014, 0x200B_31E1),
            (0x1F80_1414, 0x200B_31E1),
        ] {
            bus.write(a, 4, v);
        }
        let mut w = |address: u32, value: u32| bus.write(address, 2, value);
        for (a, v) in [
            (0x7C6, 0x900),
            (0x7C8, 0x200),
            (0x7CA, 0x8),
            (0x7C0, 0),
            (0x7C0, 0x8000),
            (0x760, 0),
            (0x762, 0),
            (0x788, 0),
            (0x78A, 0),
            (0x1B0, 0),
            (0x5B0, 0),
            (0x19A, 0),
            (0x59A, 0),
            (0x19A, 0x8000),
            (0x59A, 0x8000),
            (0x760, 0),
            (0x762, 0),
            (0x788, 0),
            (0x78A, 0),
            (0x1A4, 0xFFFF),
            (0x1A6, 0xFF),
            (0x5A4, 0xFFFF),
            (0x5A6, 0xFF),
            (0x180, 0),
            (0x182, 0),
            (0x580, 0),
            (0x582, 0),
            (0x184, 0),
            (0x186, 0),
            (0x584, 0),
            (0x586, 0),
            (0x2E0, 0xE),
            (0x2E2, 0xFFF8),
            (0x6E0, 0xF),
            (0x6E2, 0xFFF8),
            (0x1A8, 0),
            (0x1AA, 0x2800),
        ] {
            w(0x1F90_0000 + a, v);
        }
        // A looping block of silence at 0x5000, every voice's start.
        for _ in 0..8 {
            w(0x1F90_01AC, 0x0707);
        }
        w(0x1F90_019A, 0x8010);
        w(0x1F90_019A, 0x8000);
        for v in 0..24u32 {
            let (c1, c0) = (0x1F90_0400 + 0x10 * v, 0x1F90_0000 + 0x10 * v);
            for (o, value) in [(0, 0), (2, 0), (4, 0x3FFF), (6, 0), (8, 0)] {
                w(c1 + o, value);
                w(c0 + o, value);
            }
            let (a1, a0) = (0x1F90_05C0 + 12 * v, 0x1F90_01C0 + 12 * v);
            w(a1, 0);
            w(a0, 0);
            w(a1 + 2, 0x2800);
            w(a0 + 2, 0x2800);
        }
        for (a, v) in [
            (0x5A0, 0xFFFF),
            (0x1A0, 0xFFFF),
            (0x5A2, 0xFF),
            (0x1A2, 0xFF),
        ] {
            w(0x1F90_0000 + a, v);
        }
        // 989snd: every voice off, both cores on, every voice in the dry
        // and reverb mixes, the volumes.
        let mut setup: Vec<(u32, u32)> = vec![
            (0x5A4, 0xFFFF),
            (0x1A4, 0xFFFF),
            (0x5A6, 0xFF),
            (0x1A6, 0xFF),
            (0x342, 0),
            (0x340, 0),
            (0x7C0, 0xC032),
            (0x19A, 0xC000),
            (0x59A, 0xC001),
        ];
        for c in [0, 0x400] {
            for o in [0x188, 0x18A, 0x190, 0x192, 0x18C, 0x18E, 0x194, 0x196] {
                setup.push((c + o, if o & 2 == 0 { 0xFFFF } else { 0xFF }));
            }
        }
        setup.extend([
            (0x198, 0xFF0),
            (0x598, 0xFFC),
            (0x760, 0),
            (0x762, 0),
            (0x788, 0),
            (0x78A, 0),
            (0x764, 0),
            (0x766, 0),
            (0x78C, 0),
            (0x78E, 0),
            (0x33C, 0xE),
            (0x73C, 0xF),
            (0x768, 0),
            (0x76A, 0),
            (0x790, 0x7FFF),
            (0x792, 0x7FFF),
            (0x76C, 0),
            (0x76E, 0),
            (0x794, 0),
            (0x796, 0),
            (0x198, 0xF00),
            (0x598, 0xF0C),
            (0x760, 0x3FFF),
            (0x762, 0x3FFF),
            (0x764, 0),
            (0x766, 0),
            (0x78C, 0),
            (0x78E, 0),
            (0x790, 0x7FFF),
            (0x792, 0x7FFF),
            (0x7C8, 0x200),
            (0x7C0, 0xC032),
            (0x7C6, 0x900),
            (0x19A, 0xC000),
            (0x788, 0x3FFF),
            (0x78A, 0x3FFF),
        ]);
        for (a, v) in setup {
            w(0x1F90_0000 + a, v);
        }
        self.mmix = [0xF00, 0xF0C];
    }

    /// `snd_SetReverbType(cores, type)`.
    fn reverb_type(&mut self, bus: &mut dyn Bus, cores: u32, mode: i32) {
        let mut ops = Vec::new();
        self.reverb
            .set_type(&mut self.memory, cores, mode, &mut ops);
        self.apply_reverb(bus, ops);
    }

    /// The reverb's register writes, in order with the voices'.
    fn apply_reverb(&mut self, bus: &mut dyn Bus, ops: Vec<reverb::Op>) {
        for op in ops {
            match op {
                reverb::Op::Write(address, value) => self.voices.write(address, value),
                reverb::Op::Enable(c, on) => {
                    let attr = if on {
                        self.attr[c] | 0x80
                    } else {
                        self.attr[c] & !0x80
                    };
                    self.attr[c] = attr;
                    self.voices.write(0x1F90_019A + 0x400 * c as u32, attr);
                }
                reverb::Op::Clear(address, bytes) => {
                    self.flush(bus);
                    bus.upload(address, &vec![0; bytes as usize]);
                }
            }
        }
    }

    // ------------------------------------------------ movie sound

    /// `snd_InitMovieSound(bytes, volume, ?, group, ?)`: the IOP buffer the
    /// EE sends the sound to.
    fn movie_init(&mut self, bus: &mut dyn Bus, d: &[u32; 8]) -> u32 {
        let size = d[0];
        let buffer = match self.movie {
            Some(m) if m.size >= size => m.buffer,
            _ => bus.alloc_iop(size).unwrap_or(0),
        };
        self.movie = Some(Movie {
            buffer,
            size,
            volume: d[1] as i32,
            group: d[3] as i32,
            state: MovieState::Priming,
            next: buffer,
        });
        self.flush(bus);
        bus.write(0x1F90_019A, 2, 0xC000);
        bus.write(0x1F90_01A8, 2, 0);
        bus.write(0x1F90_01AA, 2, 0);
        bus.write(0x1F90_01B0, 2, 1);
        bus.write(0x1F80_10C0, 4, buffer);
        bus.write(0x1F80_10C4, 2, 0x10);
        bus.write(0x1F80_10C6, 2, 0x10);
        bus.write(0x1F80_10C8, 4, 0x0100_0201);
        buffer
    }

    /// `snd_StartMovieSound(buffer, bytes, ...)`: core 0's input plays the
    /// buffer, half after half.
    fn movie_start(&mut self, bus: &mut dyn Bus, d: &[u32; 8]) {
        let Some(m) = self.movie.as_mut() else { return };
        if d[0] != 0 {
            m.buffer = d[0];
        }
        if d[1] != 0 {
            m.size = d[1];
        }
        m.state = MovieState::Playing;
        let half = m.size / 2;
        m.next = m.buffer + half;
        let buffer = m.buffer;
        self.flush(bus);
        bus.write(0x1F90_019A, 2, 0xC000);
        bus.write(0x1F90_01A8, 2, 0);
        bus.write(0x1F90_01AA, 2, 0);
        bus.write(0x1F90_01B0, 2, 1);
        bus.write(0x1F80_10C0, 4, buffer);
        bus.write(0x1F80_10C4, 2, 0x10);
        bus.write(0x1F80_10C6, 2, half / 64);
        bus.write(0x1F80_10C8, 4, 0x0100_0201);
        self.movie_volume(bus);
        self.mmix[0] |= 0xC0;
        bus.write(0x1F90_0198, 2, u32::from(self.mmix[0]));
    }

    /// Core 0's input volume (BVOL) from the movie's volume and group.
    fn movie_volume(&mut self, bus: &mut dyn Bus) {
        let Some(m) = self.movie else { return };
        let v = (0x7FFE * m.volume.clamp(0, 0x400) / 0x400) as i16;
        let v = tables::group_master(v, m.group, &self.voices.masters) as u16;
        bus.write(0x1F90_076C, 2, u32::from(v));
        bus.write(0x1F90_076E, 2, u32::from(v));
    }

    /// `snd_ResetMovieSound` (`reset`) or `snd_CloseMovieSound`.
    fn movie_stop(&mut self, bus: &mut dyn Bus, reset: bool) {
        self.flush(bus);
        bus.write(0x1F90_076C, 2, 0);
        bus.write(0x1F90_076E, 2, 0);
        self.mmix[0] &= !0xC0;
        bus.write(0x1F90_0198, 2, u32::from(self.mmix[0]));
        if reset {
            bus.write(0x1F80_10C8, 4, 0x201);
            bus.write(0x1F90_01B0, 2, 0);
        }
        if let Some(m) = self.movie.as_mut() {
            m.state = MovieState::Idle;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bus with sound memory, a disc of one bank, and a log of writes.
    #[derive(Default)]
    struct TestBus {
        writes: Vec<(u32, u32)>,
        uploads: Vec<(u32, usize)>,
        disc: Vec<u8>,
    }

    impl Bus for TestBus {
        fn read(&mut self, _: u32, _: u32) -> u32 {
            0
        }
        fn write(&mut self, address: u32, _: u32, value: u32) {
            self.writes.push((address, value));
        }
        fn upload(&mut self, address: u32, data: &[u8]) {
            self.uploads.push((address, data.len()));
        }
        fn sectors(&mut self, lba: u32, count: u32) -> Option<Vec<u8>> {
            let at = lba as usize * 0x800;
            let mut out = self.disc.get(at..).unwrap_or(&[]).to_vec();
            out.resize(count as usize * 0x800, 0);
            Some(out)
        }
        fn alloc_iop(&mut self, _: u32) -> Option<u32> {
            Some(0xA_BB00)
        }
    }

    fn batch(commands: &[(u16, &[u32])]) -> Vec<u8> {
        let mut out = (commands.len() as u32).to_le_bytes().to_vec();
        for (id, words) in commands {
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&((4 * words.len()) as u16).to_le_bytes());
            for w in *words {
                out.extend_from_slice(&w.to_le_bytes());
            }
        }
        out
    }

    /// A bank loads into sound memory from 0x5040; a sound plays, keys on
    /// at the next tick, and its handle answers "still playing".
    #[test]
    fn loads_plays_and_keys_on() {
        let mut bus = TestBus {
            disc: bank::tests::effect_bank(),
            ..TestBus::default()
        };
        let mut driver = Driver::new(None, None);
        driver.call(SERVER, 0, &0x26_0DC0u32.to_le_bytes(), 12, &mut bus);
        let handle = driver.call(LOADER, 3, &[0; 8], 4, &mut bus);
        let handle = u32::from_le_bytes([handle[0], handle[1], handle[2], handle[3]]);
        assert_ne!(handle, 0);
        assert_eq!(bus.uploads, [(0x5040, 0x40)]);
        let mut play = handle.to_le_bytes().to_vec();
        for w in [0u32, 0x400, u32::MAX, 0, 0] {
            play.extend_from_slice(&w.to_le_bytes());
        }
        let reply = driver.call(SERVER, 17, &play, 12, &mut bus);
        let sound = u32::from_le_bytes([reply[4], reply[5], reply[6], reply[7]]);
        assert_eq!(sound, 0x8500_0002);
        bus.writes.clear();
        driver.tick(&mut bus);
        assert!(bus.writes.contains(&(0x1F90_01A0, 1)), "KON for voice 0");
        let reply = driver.call(
            SERVER,
            BATCH,
            &batch(&[(25, &[sound]), (21, &[0])]),
            16,
            &mut bus,
        );
        let words: Vec<u32> = reply
            .chunks(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(words[0], u32::MAX);
        assert_eq!(words[1], sound);
        assert_eq!(words[3], u32::MAX);
    }

    fn words(w: &[u32]) -> Vec<u8> {
        w.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    /// A disc with a VAG file of 0x3000 data bytes at 44.1 kHz at sectors
    /// 10 and 20.
    fn vag_disc() -> Vec<u8> {
        let mut disc = vec![0u8; 30 * 0x800];
        for lba in [10usize, 20] {
            let h = &mut disc[lba * 0x800..];
            h[..4].copy_from_slice(b"VAGp");
            h[0xC..0x10].copy_from_slice(&0x3000u32.to_be_bytes());
            h[0x10..0x14].copy_from_slice(&44100u32.to_be_bytes());
        }
        disc
    }

    /// A stereo stream takes two rings: refused with one (as Sly 1's
    /// driver refuses it), two voices panned left and right with two.
    #[test]
    fn stereo_streams() {
        for (rings, voices) in [(1u32, 0usize), (2, 2)] {
            let mut bus = TestBus {
                disc: vag_disc(),
                ..TestBus::default()
            };
            let mut driver = Driver::new(None, None);
            driver.call(SERVER, 0, &words(&[0]), 12, &mut bus);
            driver.call(SERVER, 42, &words(&[rings, 0x2000, 1, 1]), 12, &mut bus);
            bus.writes.clear();
            let reply = driver.call(
                SERVER,
                44,
                &words(&[10, 20, 0x0400_0000, 0, 2, 0, 0]),
                12,
                &mut bus,
            );
            let handle = u32::from_le_bytes([reply[4], reply[5], reply[6], reply[7]]);
            assert_eq!(handle != 0, voices == 2, "{rings} rings");
            driver.tick(&mut bus);
            let pitches: Vec<u32> = bus
                .writes
                .iter()
                .filter(|w| w.0 & 0xF == 4 && w.0 < 0x1F90_0180)
                .map(|w| w.1)
                .collect();
            assert_eq!(pitches.len(), voices);
            assert!(pitches.iter().all(|&p| p == 3763));
            if voices == 2 {
                // Left: all on the left, right: all on the right.
                let vol = |v: u32| {
                    bus.writes
                        .iter()
                        .find(|w| w.0 == v)
                        .map_or(u32::MAX, |w| w.1)
                };
                assert_eq!(vol(0x1F90_0002), 0);
                assert_eq!(vol(0x1F90_0010), 0);
                assert_ne!(vol(0x1F90_0000), 0);
                assert_ne!(vol(0x1F90_0012), 0);
            }
        }
    }
}
