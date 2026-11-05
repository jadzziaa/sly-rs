//! Music: a music bank's sounds play MIDI sequences on the bank's
//! programs. A MIDI song (sound type 4) plays one sequence; an AME song
//! (type 5, Sony's interactive music) plays segments of an `MMID` set,
//! whose sequences carry AME scripts (system-exclusive events `F0 75 ...
//! F7`) that start and stop segments, test registers and the global
//! excitement level, and mute channel groups by it.
//!
//! Ported from OpenGOAL (`game/sound/989snd/midi_handler.cpp` and
//! `ame_handler.cpp`, copyright 2021 - 2024 Ziemas, ISC licence; the notice
//! is in NOTICE.md), on 989snd's voice layer: a note takes
//! a voice of the song's group at its tone's priority, as sound effects
//! do. The sequencer ticks at 240 Hz with the driver.
//!
//! Handles: `0x82000000 | slot << 16 | serial` for AME songs, `0x81...`
//! for MIDI ones.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::bank::{Bank, Content, Program, Sequence, Song};
use crate::tables::music_volume;
use crate::voices::{KeyOn, Owner, VoiceEnd, Voices, mask_set};

/// Music handler slots.
const SLOTS: usize = 16;
/// Microseconds per 240 Hz tick.
const MICROS_PER_TICK: i64 = 1_000_000 / 240;

/// A note sounding.
#[derive(Clone, Copy, Debug)]
struct Note {
    voice: usize,
    /// The voice's claim serial when the note took it: once the voice is
    /// claimed again (stolen, or freed and reused) the note is gone.
    serial: u32,
    key: u8,
    channel: u8,
    velocity: u8,
    program: usize,
    tone: usize,
}

/// One sequence playing.
#[derive(Clone, Debug)]
struct Track {
    /// The sequence's index in the bank.
    sequence: usize,
    pos: usize,
    status: u8,
    tempo: u32,
    ppq: u32,
    tick_error: i64,
    countdown: i64,
    get_delta: bool,
    complete: bool,
    repeats: i8,
    programs: [u8; 16],
    channel_volume: [u8; 16],
    channel_pan: [i16; 16],
    bend: [i16; 16],
    mute: [bool; 16],
    notes: Vec<Note>,
}

impl Track {
    fn new(sequence: usize, seq: &Sequence, repeats: i8) -> Self {
        Self {
            sequence,
            pos: seq.start,
            status: 0,
            tempo: seq.tempo,
            ppq: seq.ppq.max(1),
            tick_error: 0,
            countdown: 0,
            get_delta: true,
            complete: false,
            repeats,
            programs: [0; 16],
            channel_volume: [0x7F; 16],
            channel_pan: [0; 16],
            bend: [0; 16],
            mute: [false; 16],
            notes: Vec::new(),
        }
    }

    /// Forgets the notes whose voices have since gone to someone else. A
    /// note keyed off by its note-off stays (its release still follows the
    /// song's volume) until its voice is taken again.
    fn prune(&mut self, voices: &Voices) {
        self.notes.retain(|n| voices.serial(n.voice) == n.serial);
    }
}

/// An AME channel group: the channels muted outside an excitement range.
#[derive(Clone, Copy, Debug, Default)]
struct Group {
    basis: u8,
    channels: [(u8, u8, u8); 16],
    count: usize,
}

/// A song playing.
#[derive(Clone, Debug)]
struct Player {
    bank: Rc<Bank>,
    bank_handle: u32,
    song: Song,
    ame: bool,
    vol: i32,
    pan: i32,
    pitch_mod: i16,
    paused: bool,
    /// Segments playing, by index.
    tracks: BTreeMap<usize, Track>,
    registers: [u8; 16],
    groups: [Group; 16],
    /// AME macros: where each starts, in which sequence.
    macros: [Option<(usize, usize)>; 16],
    /// Segments an AME script started during this tick.
    starts: Vec<usize>,
    /// Voices of other owners its notes took over.
    stolen: Vec<VoiceEnd>,
}

/// The music handlers.
pub(crate) struct Music {
    slots: Vec<Option<Player>>,
    words: [u32; SLOTS],
    /// The AME global excitement (`snd_SetGlobalExcite`).
    pub(crate) excite: u8,
}

/// What a program's notes need from the bank.
struct Programs<'a> {
    programs: &'a [Program],
    sequences: &'a [Sequence],
}

impl<'a> Programs<'a> {
    fn of(bank: &'a Bank) -> Option<Self> {
        match &bank.content {
            Content::Music {
                programs,
                sequences,
                ..
            } => Some(Self {
                programs,
                sequences,
            }),
            Content::Effects(_) => None,
        }
    }
}

fn read_vlq(data: &[u8], pos: &mut usize) -> u32 {
    let mut out = 0u32;
    for _ in 0..4 {
        let b = data.get(*pos).copied().unwrap_or(0);
        *pos += 1;
        out = (out << 7) | u32::from(b & 0x7F);
        if b & 0x80 == 0 {
            break;
        }
    }
    out
}

fn midi_pan(pan: u8) -> i16 {
    if pan >= 64 {
        (90 * (i32::from(pan) - 64) / 63) as i16
    } else {
        (90 * i32::from(pan) / 64 + 270) as i16
    }
}

impl Music {
    pub(crate) fn new() -> Self {
        Self {
            slots: (0..SLOTS).map(|_| None).collect(),
            words: [1; SLOTS],
            excite: 0,
        }
    }

    fn find(&self, handle: u32) -> Option<usize> {
        let kind = (handle >> 24) & 0x1F;
        if kind != 1 && kind != 2 {
            return None;
        }
        let slot = ((handle >> 16) & 0xFF) as usize;
        (slot < SLOTS && self.words[slot] == handle && self.slots[slot].is_some()).then_some(slot)
    }

    /// The handle while the song plays, else 0.
    pub(crate) fn playing(&self, handle: u32) -> u32 {
        if self.find(handle).is_some() {
            handle
        } else {
            0
        }
    }

    /// Starts song `sound` of a music bank; its handle, or 0.
    pub(crate) fn play(
        &mut self,
        bank_handle: u32,
        bank: &Rc<Bank>,
        sound: i32,
        scale: i32,
        pan: i32,
        pitch_mod: i16,
    ) -> u32 {
        let Content::Music {
            songs, sequences, ..
        } = &bank.content
        else {
            return 0;
        };
        let Some(song) = usize::try_from(sound).ok().and_then(|i| songs.get(i)) else {
            return 0;
        };
        let ame = song.kind == 5;
        if !ame && song.kind != 4 {
            return 0;
        }
        let Some(s) = self.slots.iter().position(Option::is_none) else {
            return 0;
        };
        let scale = if scale == 0x7FFF_FFFF { 0x400 } else { scale };
        let vol = ((scale * i32::from(song.volume)) >> 10).min(127);
        let pan = if (pan.wrapping_add(2) as u32) < 2 {
            i32::from(song.pan)
        } else {
            pan
        };
        let mut player = Player {
            bank: Rc::clone(bank),
            bank_handle,
            song: song.clone(),
            ame,
            vol,
            pan,
            pitch_mod,
            paused: false,
            tracks: BTreeMap::new(),
            registers: [0; 16],
            groups: [Group::default(); 16],
            macros: [None; 16],
            starts: Vec::new(),
            stolen: Vec::new(),
        };
        if let Some(seq) = sequences.first() {
            let start = !ame || matches!((seq.handle >> 24) & 0xF, 1 | 3);
            if start {
                player.tracks.insert(0, Track::new(0, seq, song.repeats));
            }
        }
        let kind = if ame { 0x0200_0000 } else { 0x0100_0000 };
        self.words[s] = ((self.words[s] & 0xFFFF).wrapping_add(1) & 0xFFFF)
            | ((s as u32) << 16)
            | kind
            | 0x8000_0000;
        self.slots[s] = Some(player);
        self.words[s]
    }

    /// A voice of a song ended.
    pub(crate) fn voice_end(&mut self, end: VoiceEnd) {
        let Owner::Music(s) = end.owner else { return };
        if let Some(Some(p)) = self.slots.get_mut(s) {
            for t in p.tracks.values_mut() {
                t.notes.retain(|n| n.voice != end.voice);
            }
        }
    }

    /// Every song, one tick; the voices of other owners its notes took
    /// over.
    pub(crate) fn tick(&mut self, voices: &mut Voices) -> Vec<VoiceEnd> {
        let excite = self.excite;
        let mut stolen = Vec::new();
        for s in 0..SLOTS {
            let Some(p) = self.slots[s].as_mut() else {
                continue;
            };
            if p.paused {
                continue;
            }
            let ids: Vec<usize> = p.tracks.keys().copied().collect();
            for id in ids {
                p.step(voices, s, id, excite);
            }
            // Finished tracks go (their notes were keyed off), then the
            // segments the scripts started begin.
            p.tracks.retain(|_, t| !t.complete);
            for id in std::mem::take(&mut p.starts) {
                p.start_segment(id);
            }
            stolen.append(&mut p.stolen);
            if p.tracks.is_empty() {
                self.slots[s] = None;
                self.words[s] &= !0x8000_0000;
            }
        }
        stolen
    }

    /// `snd_StopSound` on a song.
    pub(crate) fn stop(&mut self, voices: &mut Voices, handle: u32) {
        if let Some(s) = self.find(handle) {
            self.stop_slot(voices, s);
        }
    }

    fn stop_slot(&mut self, voices: &mut Voices, s: usize) {
        if let Some(p) = self.slots[s].take() {
            let mut mask = [0; 2];
            for t in p.tracks.values() {
                for n in t
                    .notes
                    .iter()
                    .filter(|n| voices.serial(n.voice) == n.serial)
                {
                    mask_set(&mut mask, n.voice);
                }
            }
            let _ = voices.key_off(mask, false);
            self.words[s] &= !0x8000_0000;
        }
    }

    /// Stops every song.
    pub(crate) fn stop_all(&mut self, voices: &mut Voices) {
        for s in 0..SLOTS {
            self.stop_slot(voices, s);
        }
    }

    /// Stops every song of `bank`.
    pub(crate) fn stop_bank(&mut self, voices: &mut Voices, bank: u32) {
        for s in 0..SLOTS {
            if self.slots[s]
                .as_ref()
                .is_some_and(|p| p.bank_handle == bank)
            {
                self.stop_slot(voices, s);
            }
        }
    }

    /// `snd_PauseSound` or `snd_ContinueSound` on a song.
    pub(crate) fn set_paused(&mut self, voices: &mut Voices, handle: u32, paused: bool) {
        if let Some(s) = self.find(handle) {
            self.pause_slot(voices, s, paused);
        }
    }

    fn pause_slot(&mut self, voices: &mut Voices, s: usize, paused: bool) {
        let Some(p) = self.slots[s].as_mut() else {
            return;
        };
        if p.paused == paused {
            return;
        }
        p.paused = paused;
        if paused {
            voices.pause(Owner::Music(s));
        } else {
            let mut mask = [0; 2];
            for t in p.tracks.values() {
                for n in t
                    .notes
                    .iter()
                    .filter(|n| voices.serial(n.voice) == n.serial)
                {
                    mask_set(&mut mask, n.voice);
                }
            }
            voices.resume(mask);
        }
    }

    /// Pauses or continues the songs of the groups in `mask`.
    pub(crate) fn pause_groups(&mut self, voices: &mut Voices, mask: u32, paused: bool) {
        for s in 0..SLOTS {
            if self.slots[s]
                .as_ref()
                .is_some_and(|p| mask & (1u32 << (i32::from(p.song.group) & 0x1F)) != 0)
            {
                self.pause_slot(voices, s, paused);
            }
        }
    }

    /// `snd_SetSoundVolPan` on a song.
    pub(crate) fn set_vol_pan(&mut self, voices: &mut Voices, handle: u32, vol: i32, pan: i32) {
        let Some(s) = self.find(handle) else { return };
        let Some(p) = self.slots[s].as_mut() else {
            return;
        };
        if vol >= 0 {
            if vol != 0x7FFF_FFFF {
                p.vol = (i32::from(p.song.volume) * vol) >> 10;
            }
        } else {
            p.vol = -vol;
        }
        p.vol = p.vol.min(127);
        if pan == -1 {
            p.pan = i32::from(p.song.pan);
        } else if pan != -2 {
            p.pan = pan;
        }
        let ids: Vec<usize> = p.tracks.keys().copied().collect();
        for id in ids {
            let notes = p
                .tracks
                .get_mut(&id)
                .map(|t| {
                    t.prune(voices);
                    t.notes.clone()
                })
                .unwrap_or_default();
            for n in notes {
                let lr = p.note_lr(voices, id, &n);
                voices.set_lr(n.voice, lr, p.paused);
            }
        }
    }

    /// `snd_SetMIDIRegister`.
    pub(crate) fn set_register(&mut self, handle: u32, reg: usize, value: u8) {
        if let Some(s) = self.find(handle)
            && let Some(p) = self.slots[s].as_mut()
            && let Some(r) = p.registers.get_mut(reg)
        {
            *r = value;
        }
    }

    /// `snd_GetMIDIRegister`.
    pub(crate) fn register(&self, handle: u32, reg: usize) -> u32 {
        self.find(handle)
            .and_then(|s| self.slots[s].as_ref())
            .and_then(|p| p.registers.get(reg))
            .map_or(0, |&r| u32::from(r))
    }
}

impl Player {
    fn start_segment(&mut self, id: usize) {
        if self.tracks.contains_key(&id) {
            return;
        }
        let Some(bank) = Programs::of(&self.bank) else {
            return;
        };
        let Some(seq) = bank.sequences.get(id) else {
            return;
        };
        if matches!((seq.handle >> 24) & 0xF, 1 | 3) {
            let t = Track::new(id, seq, self.song.repeats);
            self.tracks.insert(id, t);
        }
    }

    fn note_lr(&self, voices: &Voices, id: usize, n: &Note) -> [i16; 2] {
        let Some(t) = self.tracks.get(&id) else {
            return [0, 0];
        };
        let Some(bank) = Programs::of(&self.bank) else {
            return [0, 0];
        };
        let Some(prog) = bank.programs.get(n.program) else {
            return [0, 0];
        };
        let Some(tone) = prog.tones.get(n.tone) else {
            return [0, 0];
        };
        let ch = usize::from(n.channel);
        let mut pan = i32::from(t.channel_pan[ch]) + self.pan;
        if pan >= 360 {
            pan -= 360;
        }
        let mut lr = voices.lr(n.voice);
        music_volume(
            &voices.pan,
            voices.out_mode,
            [
                self.vol,
                i32::from(n.velocity) * i32::from(t.channel_volume[ch]) / 127,
                i32::from(prog.volume),
                i32::from(tone.volume),
            ],
            [pan, i32::from(prog.pan), i32::from(tone.pan)],
            &mut lr,
        );
        lr
    }

    /// Reads the next delta time and works out the ticks to wait.
    fn new_delta(t: &mut Track, data: &[u8]) {
        let delta = i64::from(read_vlq(data, &mut t.pos));
        let tempo = i64::from(t.tempo);
        let ppq = i64::from(t.ppq);
        let per_tick = (tempo / ppq).max(1);
        let ppt = 100 * MICROS_PER_TICK / per_tick;
        let mut tick_delta = 100 * delta + t.tick_error;
        if tick_delta < 0 || tick_delta < ppt / 2 {
            t.tick_error = tick_delta;
            tick_delta = 0;
        }
        if tick_delta != 0 {
            t.countdown = (tick_delta / 100 * tempo / ppq - 1 + MICROS_PER_TICK) / MICROS_PER_TICK;
            t.tick_error = tick_delta - ppt * t.countdown;
        } else {
            t.countdown = 0;
        }
    }

    /// One tick of track `id`.
    #[allow(clippy::too_many_lines, reason = "one arm per MIDI event")]
    fn step(&mut self, voices: &mut Voices, slot: usize, id: usize, excite: u8) {
        let bank = Rc::clone(&self.bank);
        let Some(progs) = Programs::of(&bank) else {
            return;
        };
        let Some(seq) = self
            .tracks
            .get(&id)
            .and_then(|t| progs.sequences.get(t.sequence))
        else {
            return;
        };
        let data = &seq.data;
        {
            let Some(t) = self.tracks.get_mut(&id) else {
                return;
            };
            if t.complete {
                return;
            }
            if t.get_delta {
                Self::new_delta(t, data);
                t.get_delta = false;
            } else {
                t.countdown -= 1;
            }
        }
        let mut guard = 0;
        loop {
            let Some(t) = self.tracks.get_mut(&id) else {
                return;
            };
            if t.countdown > 0 || t.complete {
                return;
            }
            guard += 1;
            if guard > 10_000 || t.pos >= data.len() {
                t.complete = true;
                return;
            }
            if data[t.pos] & 0x80 != 0 {
                t.status = data[t.pos];
                t.pos += 1;
            }
            let ch = usize::from(t.status & 0xF);
            let b0 = data.get(t.pos).copied().unwrap_or(0);
            let b1 = data.get(t.pos + 1).copied().unwrap_or(0);
            match t.status >> 4 {
                0x8 => {
                    t.pos += 2;
                    self.note_off(voices, id, ch, b0);
                }
                0x9 => {
                    t.pos += 2;
                    if b1 == 0 {
                        self.note_off(voices, id, ch, b0);
                    } else {
                        self.note_on(voices, &progs, slot, id, ch, b0, b1);
                    }
                }
                0xB => {
                    t.pos += 2;
                    match b0 {
                        7 => t.channel_volume[ch] = b1,
                        10 => t.channel_pan[ch] = midi_pan(b1),
                        _ => {}
                    }
                }
                0xC => {
                    t.pos += 1;
                    t.programs[ch] = b0;
                }
                0xD => {
                    // Channel pressure: 989snd keys off the note named.
                    t.pos += 1;
                    self.note_off(voices, id, ch, b0);
                }
                0xE => {
                    t.pos += 2;
                    let raw = i32::from(b0 & 0x7F) | (i32::from(b1 & 0x7F) << 7);
                    let bend = (0xFFFF * raw / 0x3FFF + 0x8000) as i16;
                    t.bend[ch] = bend;
                    t.prune(voices);
                    let notes: Vec<Note> = t
                        .notes
                        .iter()
                        .copied()
                        .filter(|n| usize::from(n.channel) == ch)
                        .collect();
                    let (pm, paused) = (self.pitch_mod, self.paused);
                    for n in notes {
                        voices.repitch(n.voice, bend, pm, paused);
                    }
                }
                0xF if t.status == 0xFF => {
                    let len = usize::from(b1);
                    if b0 == 0x2F {
                        t.pos = seq.start;
                        t.repeats = t.repeats.wrapping_sub(1);
                        if t.repeats == 0 {
                            t.complete = true;
                        }
                        t.repeats = t.repeats.max(0);
                    } else {
                        if b0 == 0x51 {
                            let at = t.pos + 2;
                            t.tempo = data.get(at..at + 3).map_or(t.tempo, |b| {
                                u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2])
                            });
                        }
                        t.pos += len + 2;
                    }
                }
                0xF if t.status == 0xF0 && b0 == 0x75 => {
                    t.pos += 1;
                    let pos = t.pos;
                    let (cont, pos) = if self.ame {
                        self.run_ame(data, id, pos, excite)
                    } else {
                        (false, pos)
                    };
                    if let Some(t) = self.tracks.get_mut(&id) {
                        t.pos = pos;
                        if !cont {
                            t.complete = true;
                        }
                    }
                }
                _ => {
                    tracing::debug!(target: "snd989", "music: unknown MIDI status {:#x}", t.status);
                    t.complete = true;
                }
            }
            let Some(t) = self.tracks.get_mut(&id) else {
                return;
            };
            if t.complete {
                t.prune(voices);
                let mut mask = [0; 2];
                for n in &t.notes {
                    mask_set(&mut mask, n.voice);
                }
                let _ = voices.key_off(mask, false);
                return;
            }
            Self::new_delta(t, data);
        }
    }

    #[allow(clippy::too_many_arguments, reason = "the note's whole context")]
    fn note_on(
        &mut self,
        voices: &mut Voices,
        progs: &Programs<'_>,
        slot: usize,
        id: usize,
        ch: usize,
        note: u8,
        velocity: u8,
    ) {
        let Some(t) = self.tracks.get(&id) else {
            return;
        };
        if t.mute[ch] {
            return;
        }
        let program = usize::from(t.programs[ch]);
        let Some(prog) = progs.programs.get(program) else {
            return;
        };
        let bend = t.bend[ch];
        for (k, tone) in prog.tones.iter().enumerate() {
            if note < tone.map_low || note > tone.map_high {
                continue;
            }
            let Some(granted) = voices.alloc(self.song.group, tone.priority) else {
                continue;
            };
            if let Some(stolen) = granted.stolen {
                // Another owner's voice was taken: the driver tells it.
                self.stolen.push(stolen);
            }
            let n = Note {
                voice: granted.voice,
                serial: voices.serial(granted.voice),
                key: note,
                channel: ch as u8,
                velocity,
                program,
                tone: k,
            };
            if let Some(t) = self.tracks.get_mut(&id) {
                t.prune(voices);
                t.notes.push(n);
            }
            let lr = {
                let mut zero = [0i16; 2];
                let t = &self.tracks[&id];
                let mut pan = i32::from(t.channel_pan[ch]) + self.pan;
                if pan >= 360 {
                    pan -= 360;
                }
                music_volume(
                    &voices.pan,
                    voices.out_mode,
                    [
                        self.vol,
                        i32::from(velocity) * i32::from(t.channel_volume[ch]) / 127,
                        i32::from(prog.volume),
                        i32::from(tone.volume),
                    ],
                    [pan, i32::from(prog.pan), i32::from(tone.pan)],
                    &mut zero,
                );
                zero
            };
            voices.key_on(
                granted.voice,
                &KeyOn {
                    owner: Owner::Music(slot),
                    tone: tone.clone(),
                    base: self.bank.base,
                    group: self.song.group,
                    lr,
                    tone_volume: tone.volume,
                    tone_pan: tone.pan,
                    centre: note,
                    centre_fine: 0,
                    bend,
                    pitch_mod: self.pitch_mod,
                },
            );
            if self.paused {
                voices.pause(Owner::Music(slot));
            }
        }
    }

    fn note_off(&mut self, voices: &mut Voices, id: usize, ch: usize, note: u8) {
        let Some(t) = self.tracks.get_mut(&id) else {
            return;
        };
        t.prune(voices);
        let mut mask = [0; 2];
        for n in &t.notes {
            if usize::from(n.channel) == ch && n.key == note {
                mask_set(&mut mask, n.voice);
            }
        }
        let _ = voices.key_off(mask, false);
    }

    /// Runs an AME script from `pos` (just past `F0 75`); whether the
    /// calling track goes on, and where it does.
    #[allow(clippy::too_many_lines, reason = "one arm per AME operation")]
    fn run_ame(&mut self, data: &[u8], caller: usize, mut pos: usize, excite: u8) -> (bool, usize) {
        let at = |p: usize| data.get(p).copied().unwrap_or(0xF7);
        let mut skip = 0u8;
        let mut cont = true;
        let mut done = false;
        let mut guard = 0;
        while !done {
            guard += 1;
            if guard > 4096 || pos >= data.len() {
                return (false, pos);
            }
            let op = at(pos);
            pos += 1;
            // Most operations run unless skipped; a skip set by a failed
            // test covers the next operation (1) or a block (2).
            let run = |skip: &mut u8| -> bool {
                if *skip != 0 {
                    if *skip == 1 {
                        *skip = 0;
                    }
                    false
                } else {
                    true
                }
            };
            match op {
                0x0..=0x2 => {
                    if run(&mut skip) {
                        let v = i32::from(at(pos)) + 1;
                        let e = i32::from(excite);
                        let fail = match op {
                            0 => e <= v,
                            1 => e != v,
                            _ => e > v,
                        };
                        if fail {
                            skip = 1;
                        }
                    }
                    pos += 1;
                }
                0x3 => {
                    if run(&mut skip)
                        && let Some(t) = self.tracks.get_mut(&usize::from(at(pos)))
                    {
                        t.complete = true;
                    }
                    pos += 1;
                }
                0x4 => {
                    if skip == 1 {
                        skip = 2;
                    }
                }
                0x5 => {
                    if skip == 2 {
                        skip = 0;
                    }
                }
                0x6 | 0x7 => {
                    if run(&mut skip) {
                        let r = i32::from(self.registers[usize::from(at(pos)) & 15]);
                        let v = i32::from(at(pos + 1));
                        let fail = if op == 6 { r > v - 1 } else { r < v + 1 };
                        if fail {
                            skip = 1;
                        }
                    }
                    pos += 2;
                }
                0xB => {
                    self.macros[usize::from(at(pos)) & 15] = Some((caller, pos + 1));
                    while at(pos) != 0xF7 {
                        pos += 1;
                    }
                    pos += 1;
                }
                0xC => {
                    if run(&mut skip)
                        && let Some((_, mpos)) = self.macros[usize::from(at(pos)) & 15]
                    {
                        let (sub, _) = self.run_ame(data, caller, mpos, excite);
                        if !sub {
                            cont = false;
                            done = true;
                        }
                    }
                    pos += 1;
                }
                0xD | 0xE => {
                    if run(&mut skip) {
                        let seg = usize::from(self.registers[usize::from(at(pos)) & 15]);
                        if op == 0xD {
                            cont = false;
                            done = true;
                        }
                        self.starts.push(seg.wrapping_sub(1));
                    }
                    pos += 1;
                }
                0xF => {
                    if skip != 0 {
                        while at(pos) != 0x7F {
                            pos += 1;
                        }
                        pos += 1;
                        if skip == 1 {
                            skip = 0;
                        }
                    } else {
                        let group = usize::from(at(pos)) & 15;
                        let g = &mut self.groups[group];
                        g.basis = at(pos + 1);
                        pos += 2;
                        g.count = 0;
                        while at(pos) != 0xF7 && g.count < 16 {
                            g.channels[g.count] = (at(pos), at(pos + 1), at(pos + 2));
                            g.count += 1;
                            pos += 3;
                        }
                        pos += 1;
                    }
                }
                0x10 => {
                    if run(&mut skip) {
                        let g = self.groups[usize::from(at(pos)) & 15];
                        let comp = if g.basis == 0 {
                            excite
                        } else {
                            self.registers[usize::from(g.basis - 1) & 15]
                        };
                        if let Some(t) = self.tracks.get_mut(&caller) {
                            for &(channel, low, high) in &g.channels[..g.count] {
                                t.mute[usize::from(channel) & 15] = comp < low || comp > high;
                            }
                        }
                    }
                    pos += 1;
                }
                0x11 | 0x12 => {
                    if run(&mut skip) {
                        if op == 0x11 {
                            cont = false;
                            done = true;
                        }
                        self.starts.push(usize::from(at(pos)));
                    }
                    pos += 1;
                }
                0x13 => {
                    if run(&mut skip) {
                        self.registers[usize::from(at(pos)) & 15] = at(pos + 1);
                    }
                    pos += 2;
                }
                0x14 | 0x15 => {
                    if run(&mut skip) {
                        let r = &mut self.registers[usize::from(at(pos)) & 15];
                        if op == 0x14 && *r < 0x7F {
                            *r += 1;
                        } else if op == 0x15 && *r > 0 {
                            *r -= 1;
                        }
                    }
                    pos += 1;
                }
                0x16 => {
                    if run(&mut skip) && self.registers[usize::from(at(pos)) & 15] != at(pos + 1) {
                        skip = 1;
                    }
                    pos += 2;
                }
                _ => {
                    tracing::debug!(target: "snd989", "music: unknown AME operation {op:#x}");
                    return (false, pos);
                }
            }
            if at(pos) == 0xF7 {
                pos += 1;
                done = true;
            }
        }
        (cont, pos)
    }
}
