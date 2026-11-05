//! Sound effects: the handlers that run a sound's grains, one per playing
//! sound, and their LFOs. Ported from rac-rs (`snd989/handler.rs` and
//! `lfo.rs`, an exact port of the R&C driver's grain interpreter), with
//! the child sounds (grains 5 and 6) Sly 1's banks use added after
//! OpenGOAL's reading of them (`sfxgrain.cpp`).
//!
//! A handler is a slot of 64; its handle word is `0x85000000 | slot << 16
//! | serial`. Every tick the handlers run in the order they started: each
//! counts down the wait of its current grain, then runs grains until one
//! waits. A tone grain takes a voice and keys it on; a program that ends
//! with voices still sounding stays until they end.

use std::collections::HashMap;
use std::rc::Rc;

use crate::bank::{Bank, Content, Effect, Grain, Tone};
use crate::tables::{R250, SINE_LEN, effect_volume, pan_scale, saw, sine_table, triangle};
use crate::voices::{
    KeyOn, Owner, VoiceEnd, VoiceMask, Voices, mask_clear, mask_empty, mask_set, mask_voices,
};

/// Handler slots.
const SLOTS: usize = 64;
/// The type in a sound effect's handle.
pub(crate) const HANDLE_TYPE: u32 = 0x0500_0000;
/// The bit of a handle whose handler is running.
const HANDLE_LINKED: u32 = 0x8000_0000;
/// Grains one handler may run in one tick (a guard: a program of
/// zero-delay grains looping for ever would hang the real driver).
const MAX_GRAINS: u32 = 10_000;
/// LFOs running at once.
const MAX_LFOS: usize = 10;

/// A tone's priority, lowered by a fading volume when its flags ask.
pub(crate) fn tone_priority(scale: i16, tone: &Tone) -> i8 {
    let p0 = i32::from(tone.priority);
    if tone.flags & 6 == 0 {
        return tone.priority;
    }
    let d = match (tone.flags >> 1) & 3 {
        1 => p0 / 5,
        2 => p0 / 2,
        _ => p0,
    };
    if d == 0 || i32::from(scale) > 0x3B6 {
        return tone.priority;
    }
    (p0 - (d - d * i32::from(scale) / 0x3B6)) as i8
}

/// `r % n` as the IOP computes it; `None` for a division by zero.
fn c_mod(r: i32, n: i32) -> Option<i32> {
    match n {
        0 => None,
        -1 => Some(0),
        _ => Some(r % n),
    }
}

/// One of a handler's four LFOs.
#[derive(Clone, Debug, Default)]
struct LfoSlot {
    /// 0 none, 1 sine, 2 square, 3 triangle, 4 saw, 5 random.
    wave: u16,
    /// 0 off, 1 volume, 2 pan, 3 pitch modulation, 4 bend, 5 another
    /// LFO's rate, 6 another LFO's depth.
    target: u8,
    link: u8,
    /// Bit 0 inverts, bit 1 starts at a random phase.
    flags: u8,
    in_list: bool,
    depth: u16,
    depth_wide: u32,
    phase: u32,
    rate: u32,
    rate_base: u32,
    /// The square's width, or the random wave's held value.
    aux: u32,
    random_held: u32,
    amp: i32,
}

/// A playing sound effect.
#[derive(Clone, Debug, Default)]
struct Handler {
    word: u32,
    bank: Option<Rc<Bank>>,
    bank_handle: u32,
    sound: usize,
    base_volume: i16,
    volume: i16,
    pan: i16,
    pitch_mod: i16,
    /// Bit 1 paused, bit 2 stopping.
    flags: u8,
    group: i8,
    voices: VoiceMask,
    bend: i16,
    vol_off: i16,
    scale: i16,
    pan_off: i16,
    pan_base: i16,
    bend_off: i16,
    bend_base: i16,
    pmod_off: i16,
    pmod_base: i16,
    wait: i16,
    /// The grain to run; -1 when the program has finished.
    idx: i16,
    regs: [i8; 4],
    block_active: bool,
    block_count: u16,
    block_skip: i16,
    lfo: [LfoSlot; 4],
    /// The handler that started this one (grain 5), and the ones it
    /// started.
    parent: Option<usize>,
    children: Vec<usize>,
}

impl Handler {
    fn in_use(&self) -> bool {
        self.bank.is_some()
    }

    fn paused(&self) -> bool {
        self.flags & 2 != 0
    }

    fn effect(&self) -> Option<&Effect> {
        match &self.bank.as_ref()?.content {
            Content::Effects(sounds) => sounds.get(self.sound),
            Content::Music { .. } => None,
        }
    }
}

/// How a sound starts: `snd_PlaySoundVolPanPMPB`'s arguments.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Play {
    /// Volume scale (0x400 is 1.0; 0x7FFFFFFF the sound's own).
    pub(crate) scale: i32,
    /// Pan in degrees (-1 or -2: the sound's own).
    pub(crate) pan: i32,
    pub(crate) pitch_mod: i16,
    pub(crate) bend: i16,
}

/// The sound effect handlers and their LFOs.
pub(crate) struct Effects {
    slots: Vec<Handler>,
    /// Running handlers, in start order.
    order: Vec<usize>,
    /// Running LFOs, as (handler, index), in start order.
    lfos: Vec<(usize, usize)>,
    sine: Vec<i16>,
    /// The state grains 25 and 39 keep in the bank (the last block they
    /// chose), by (bank, sound, grain).
    picks: HashMap<(u32, usize, usize), i16>,
}

impl Effects {
    pub(crate) fn new() -> Self {
        Self {
            slots: (0..SLOTS)
                .map(|_| Handler {
                    word: 1,
                    ..Handler::default()
                })
                .collect(),
            order: Vec::new(),
            lfos: Vec::new(),
            sine: sine_table(),
            picks: HashMap::new(),
        }
    }

    /// The slot a handle names, if its handler still runs.
    fn find(&self, handle: u32) -> Option<usize> {
        if (handle >> 24) & 0x1F != 5 {
            return None;
        }
        let slot = ((handle >> 16) & 0xFF) as usize;
        (slot < SLOTS && self.slots[slot].word == handle && self.slots[slot].in_use())
            .then_some(slot)
    }

    /// `snd_SoundIsStillPlaying`: the handle while it plays, else 0.
    pub(crate) fn playing(&self, handle: u32) -> u32 {
        if self.find(handle).is_some() {
            handle
        } else {
            0
        }
    }

    /// Forgets a bank's grain state (it was unloaded).
    pub(crate) fn forget_bank(&mut self, bank: u32) {
        self.picks.retain(|k, _| k.0 != bank);
    }

    fn alloc_slot(&mut self) -> Option<usize> {
        let s = (0..SLOTS).find(|&i| !self.slots[i].in_use())?;
        let h = &mut self.slots[s];
        h.word = ((h.word & 0xFFFF).wrapping_add(1) & 0xFFFF) | ((s as u32) << 16) | HANDLE_TYPE;
        h.flags = 0;
        Some(s)
    }

    fn link(&mut self, s: usize) {
        self.slots[s].word |= HANDLE_LINKED;
        if !self.order.contains(&s) {
            self.order.push(s);
        }
    }

    fn free(&mut self, s: usize) {
        self.slots[s].bank = None;
        self.slots[s].word &= !HANDLE_LINKED;
        self.order.retain(|&x| x != s);
        if let Some(p) = self.slots[s].parent.take() {
            self.slots[p].children.retain(|&c| c != s);
            // A parent waiting only for its children goes with the last.
            let ph = &self.slots[p];
            if ph.in_use() && ph.idx == -1 && mask_empty(ph.voices) && ph.children.is_empty() {
                self.release_lfos(p);
                self.free(p);
            }
        }
    }

    // ------------------------------------------------ voices ending

    /// A voice of handler `s` ended or was taken: once the program has
    /// finished and no voice is left, the handler goes.
    pub(crate) fn voice_end(&mut self, end: VoiceEnd) {
        let Owner::Effect(s) = end.owner else { return };
        if s >= SLOTS {
            return;
        }
        mask_clear(&mut self.slots[s].voices, end.voice);
        let h = &self.slots[s];
        if h.in_use() && mask_empty(h.voices) && h.idx == -1 && h.children.is_empty() {
            self.release_lfos(s);
            self.free(s);
        }
    }

    fn voice_ends(&mut self, ends: Vec<VoiceEnd>) {
        for e in ends {
            self.voice_end(e);
        }
    }

    // ------------------------------------------------ starting a sound

    /// `snd_PlaySoundVolPanPMPB`: starts sound `sound` of `bank`; returns
    /// its handle, or 0 (no such sound, no free slot, or a program that
    /// ended at once without a voice). Grains without a delay run now.
    #[allow(clippy::too_many_arguments, reason = "the driver's own arguments")]
    pub(crate) fn play(
        &mut self,
        voices: &mut Voices,
        rng: &mut R250,
        bank_handle: u32,
        bank: &Rc<Bank>,
        sound: i32,
        a: &Play,
        parent: Option<usize>,
        regs: [i8; 4],
    ) -> u32 {
        let Content::Effects(sounds) = &bank.content else {
            return 0;
        };
        let Some(sb) = usize::try_from(sound).ok().and_then(|i| sounds.get(i)) else {
            return 0;
        };
        if sb.grains.is_empty() {
            return 0;
        }
        let sound = sound as usize;
        let base = i16::from(sb.volume);
        let scale = if a.scale == 0x7FFF_FFFF {
            0x400
        } else {
            a.scale
        };
        let mut pan = a.pan;
        if (pan.wrapping_add(2) as u32) < 2 {
            pan = i32::from(sb.pan as i16);
        }
        let vol0 = (scale.wrapping_mul(i32::from(base)) >> 10).min(0x7F);
        if sb.flags & 2 != 0 {
            // One instance at a time: the others stop first.
            for c in self.order.clone() {
                if self.slots[c].bank_handle == bank_handle
                    && self.slots[c].sound == sound
                    && self.slots[c].in_use()
                {
                    self.stop_path(voices, rng, c, false);
                }
            }
        }
        let Some(s) = self.alloc_slot() else { return 0 };
        let word = self.slots[s].word;
        self.slots[s] = Handler {
            word,
            bank: Some(Rc::clone(bank)),
            bank_handle,
            sound,
            base_volume: base,
            volume: vol0 as i16,
            pan: pan as i16,
            pitch_mod: a.pitch_mod,
            flags: 0,
            group: sb.group as i8,
            voices: [0, 0],
            bend: a.bend,
            vol_off: 0,
            scale: scale as i16,
            pan_off: 0,
            pan_base: pan as i16,
            bend_off: 0,
            bend_base: a.bend,
            pmod_off: 0,
            pmod_base: a.pitch_mod,
            wait: sb.grains[0].delay as i16,
            idx: 0,
            regs,
            block_active: false,
            block_count: 0,
            block_skip: 0,
            lfo: Default::default(),
            parent,
            children: Vec::new(),
        };
        if let Some(p) = parent {
            self.slots[p].children.push(s);
        }
        let mut finished = false;
        let mut guard = 0;
        while self.slots[s].wait == 0 && self.slots[s].idx != -1 {
            guard += 1;
            if guard > MAX_GRAINS || self.run_grain(voices, rng, s) {
                finished = true;
                break;
            }
        }
        if finished
            || (self.slots[s].wait == 0
                && mask_empty(self.slots[s].voices)
                && self.slots[s].children.is_empty())
        {
            self.stop_path(voices, rng, s, false);
            self.free(s);
            return 0;
        }
        self.link(s);
        self.slots[s].word
    }

    // ------------------------------------------------ the grains

    /// Runs the current grain and moves on. True when the handler has
    /// finished (a stop, an error, or the end with no voice).
    fn run_grain(&mut self, voices: &mut Voices, rng: &mut R250, s: usize) -> bool {
        let Some(bank) = self.slots[s].bank.clone() else {
            return true;
        };
        let Some(effect) = (match &bank.content {
            Content::Effects(sounds) => sounds.get(self.slots[s].sound),
            Content::Music { .. } => None,
        }) else {
            return true;
        };
        let grains = &effect.grains;
        let count = i32::from(grains.len() as u8 as i8);
        let idx = self.slots[s].idx;
        let Some(g) = usize::try_from(idx).ok().and_then(|i| grains.get(i)) else {
            return true;
        };
        let ret = self.exec(voices, rng, s, &bank, effect, g, idx as usize);
        if ret < 0 {
            return true;
        }
        let h = &mut self.slots[s];
        if h.block_active {
            h.block_count = h.block_count.wrapping_sub(1);
            if h.block_count == 0 {
                h.block_active = false;
                h.idx = h.idx.wrapping_add(h.block_skip);
            }
        }
        h.idx = h.idx.wrapping_add(1);
        if count - 1 < i32::from(h.idx) {
            if mask_empty(h.voices) && h.children.is_empty() {
                return true;
            }
            h.idx = -1;
        } else {
            let delay = usize::try_from(h.idx)
                .ok()
                .and_then(|i| grains.get(i))
                .map_or(0, |g| g.delay);
            h.wait = (i32::from(delay) + ret) as i16;
        }
        false
    }

    fn reg(&self, s: usize, i: i16) -> i8 {
        self.slots[s].regs.get(i as usize).copied().unwrap_or(0)
    }

    fn set_reg(&mut self, s: usize, i: i16, value: i8) {
        if let Some(r) = self.slots[s].regs.get_mut(i as usize) {
            *r = value;
        }
    }

    fn find_marker(grains: &[Grain], id: i32) -> Option<usize> {
        grains
            .iter()
            .position(|g| g.kind == 35 && i32::from(g.s16_at(0)) == id)
    }

    fn pick(&self, s: usize, gi: usize, g: &Grain) -> i16 {
        let h = &self.slots[s];
        self.picks
            .get(&(h.bank_handle, h.sound, gi))
            .copied()
            .unwrap_or_else(|| g.s16_at(4))
    }

    fn set_pick(&mut self, s: usize, gi: usize, v: i16) {
        let h = &self.slots[s];
        self.picks.insert((h.bank_handle, h.sound, gi), v);
    }

    fn block_start(&mut self, s: usize, a: i16, n: i16, l: i16) {
        let h = &mut self.slots[s];
        h.idx = h.idx.wrapping_add((i32::from(a) * i32::from(l)) as i16);
        h.block_count = l.wrapping_add(1) as u16;
        h.block_active = true;
        h.block_skip = (((i32::from(n) - 1) - i32::from(a)) * i32::from(l)) as i16;
    }

    /// One grain; the extra wait before the next (0 or more), or -1 to
    /// stop.
    #[allow(clippy::too_many_arguments, reason = "the interpreter's state")]
    #[allow(clippy::too_many_lines, reason = "one arm per grain type")]
    fn exec(
        &mut self,
        voices: &mut Voices,
        rng: &mut R250,
        s: usize,
        bank: &Rc<Bank>,
        effect: &Effect,
        g: &Grain,
        gi: usize,
    ) -> i32 {
        let grains = &effect.grains;
        let count = i32::from(grains.len() as u8 as i8);
        match g.kind {
            1 => self.grain_tone(voices, rng, s, bank, effect, g),
            4 => self.grain_lfo(voices, rng, s, g),
            5 => {
                // Starts a child sound of the same bank.
                let id = g.u32_at(0xC) as i32;
                let mut vol = g.u32_at(0) as i32;
                if vol < 0 {
                    vol = if vol >= -4 {
                        i32::from(self.reg(s, (-vol - 1) as i16))
                    } else {
                        i32::from(rng.next() % 0x7F)
                    };
                }
                let mut pan = g.u32_at(4) as i32;
                if pan < 0 {
                    pan = if pan >= -4 {
                        0x168 * i32::from(self.reg(s, (-pan - 1) as i16)).abs().min(127) / 127
                    } else {
                        i32::from(rng.next()) % 0x168
                    };
                }
                let h = &self.slots[s];
                let scale = i32::from(h.scale) * vol.abs().clamp(0, 127) / 127;
                let play = Play {
                    scale,
                    pan,
                    pitch_mod: h.pmod_base,
                    bend: h.bend_base,
                };
                let (bank_handle, regs) = (h.bank_handle, h.regs);
                if id >= 0 {
                    self.play(voices, rng, bank_handle, bank, id, &play, Some(s), regs);
                }
                0
            }
            6 => {
                // Stops the children playing sound `id`.
                let id = g.u32_at(0xC) as usize;
                for c in self.slots[s].children.clone() {
                    if self.slots[c].sound == id {
                        self.stop_path(voices, rng, c, false);
                        self.free(c);
                    }
                }
                0
            }
            22 => {
                let mut j = i32::from(self.slots[s].idx) - 1;
                while j >= 0 {
                    if grains[j as usize].kind == 21 {
                        self.slots[s].idx = (j - 1) as i16;
                        return 0;
                    }
                    j -= 1;
                }
                -1
            }
            23 => {
                let mut j = i32::from(self.slots[s].idx) + 1;
                while j < count {
                    if grains[j as usize].kind == 22 {
                        self.slots[s].idx = j as i16;
                        return 0;
                    }
                    j += 1;
                }
                -1
            }
            25 => {
                // A random block of grains: n blocks of l, never the last
                // one chosen twice in a row.
                if self.slots[s].block_active {
                    return -1;
                }
                let r = i32::from(rng.next());
                let (n, l) = (i32::from(g.s16_at(0)), g.s16_at(2));
                let Some(mut a) = c_mod(r, n) else { return -1 };
                if a == i32::from(self.pick(s, gi, g)) {
                    a += 1;
                    if a == n {
                        a = 0;
                    }
                }
                self.set_pick(s, gi, a as i16);
                self.block_start(s, a as i16, n as i16, l);
                0
            }
            26 => {
                let r = i32::from(rng.next());
                c_mod(r, i32::from(g.s16_at(0)) + 1).unwrap_or(-1)
            }
            27 => {
                let r = u32::from(rng.next()) % 0x7FFF;
                let y = (r.wrapping_mul(0xFFFF) as i32) / 0x7FFF - 0x8000;
                let b = (y.wrapping_mul(i32::from(g.s16_at(0))) / 100) as i16;
                self.set_bend(voices, s, b);
                0
            }
            28 => {
                let x = i32::from(g.s16_at(0));
                let b = if x < 0 {
                    ((x.wrapping_mul(-0x8000) / 0x7F) as i16).wrapping_neg()
                } else {
                    (x.wrapping_mul(0x7FFF) / 0x7F) as i16
                };
                self.set_bend(voices, s, b);
                0
            }
            29 => {
                let x = i32::from(self.slots[s].bend)
                    + i32::from(g.s16_at(0)).wrapping_mul(0x7FFF) / 0x7F;
                self.set_bend(voices, s, x.clamp(-0x8000, 0x7FFF) as i16);
                0
            }
            30 => {
                self.set_reg(s, g.s16_at(0), g.u8_at(2) as i8);
                0
            }
            31 => {
                let n = i32::from(g.s16_at(4)) - i32::from(g.s16_at(2)) + 1;
                let r = i32::from(rng.next());
                let Some(m) = c_mod(r, n) else { return -1 };
                self.set_reg(s, g.s16_at(0), (m as i8).wrapping_add(g.s16_at(2) as i8));
                0
            }
            32 => {
                let r = (i32::from(self.reg(s, g.s16_at(0))) + 1).min(0x7F);
                self.set_reg(s, g.s16_at(0), r as i8);
                0
            }
            33 => {
                let r = (i32::from(self.reg(s, g.s16_at(0))) - 1).max(-0x80);
                self.set_reg(s, g.s16_at(0), r as i8);
                0
            }
            34 => {
                let (r, val) = (i32::from(self.reg(s, g.s16_at(0))), i32::from(g.s16_at(4)));
                let ok = match g.s16_at(2) {
                    0 => r < val,
                    1 => r == val,
                    2 => r > val,
                    _ => true,
                };
                if !ok {
                    self.slots[s].idx = self.slots[s].idx.wrapping_add(1);
                }
                0
            }
            36 => match Self::find_marker(grains, i32::from(g.s16_at(0))) {
                Some(i) => {
                    self.slots[s].idx = (i as i32 - 1) as i16;
                    0
                }
                None => -1,
            },
            37 => {
                let n = i32::from(g.s16_at(2)) - i32::from(g.s16_at(0)) + 1;
                let r = i32::from(rng.next());
                let Some(m) = c_mod(r, n) else { return -1 };
                match Self::find_marker(grains, m + i32::from(g.s16_at(0))) {
                    Some(i) => {
                        self.slots[s].idx = (i as i32 - 1) as i16;
                        0
                    }
                    None => -1,
                }
            }
            38 => {
                // Waits for every voice: runs again next tick.
                if mask_empty(self.slots[s].voices) {
                    0
                } else {
                    self.slots[s].idx = self.slots[s].idx.wrapping_sub(1);
                    1
                }
            }
            39 => {
                if self.slots[s].block_active {
                    return -1;
                }
                let n = g.s16_at(0);
                let a = self.pick(s, gi, g);
                let mut next = a.wrapping_add(1);
                if next == n {
                    next = 0;
                }
                self.set_pick(s, gi, next);
                self.block_start(s, a, n, g.s16_at(2));
                0
            }
            40 => {
                let r = (i32::from(self.reg(s, g.s16_at(2))) + i32::from(g.s16_at(0)))
                    .clamp(-0x80, 0x7F);
                self.set_reg(s, g.s16_at(2), r as i8);
                0
            }
            41 => {
                let m = self.slots[s].voices;
                let ends = voices.key_off(m, false);
                self.slots[s].voices = [0, 0];
                self.voice_ends(ends);
                0
            }
            42 => {
                let m = self.slots[s].voices;
                let ends = voices.kill(m, false);
                self.slots[s].voices = [0, 0];
                self.voice_ends(ends);
                0
            }
            43 => {
                self.slots[s].idx = (count - 1) as i16;
                0
            }
            0 | 2 | 3 | 20 | 21 | 35 => 0,
            // 24 stops; other types are not grains a bank holds.
            _ => -1,
        }
    }

    /// A tone grain: takes a voice and keys it on. A tone that gets no
    /// voice is dropped.
    fn grain_tone(
        &mut self,
        voices: &mut Voices,
        rng: &mut R250,
        s: usize,
        bank: &Rc<Bank>,
        effect: &Effect,
        g: &Grain,
    ) -> i32 {
        let Some(tone) = g.tone() else { return 0 };
        let h = &mut self.slots[s];
        let sum =
            i32::from(h.vol_off as u16) + ((i32::from(h.scale) * i32::from(h.base_volume)) >> 10);
        h.volume = (sum as i16).clamp(0, 0x7F);
        let mut p = h.pan_base.wrapping_add(h.pan_off);
        while p > 0x167 {
            p = p.wrapping_sub(0x168);
        }
        while p < 0 {
            p = p.wrapping_add(0x168);
        }
        h.pan = p;
        let priority = tone_priority(h.scale, &tone);
        let Some(granted) = voices.alloc(effect.group as i8, priority) else {
            return 0;
        };
        if let Some(stolen) = granted.stolen {
            self.voice_end(stolen);
        }
        let tone_volume: i8 = if tone.volume < 0 {
            if tone.volume < -4 {
                (rng.next() & 0x7F) as i8
            } else {
                self.reg(s, !i16::from(tone.volume))
            }
        } else {
            tone.volume
        };
        let tone_pan: i16 = if tone.pan < 0 {
            if tone.pan < -4 {
                (i32::from(rng.next()) % 0x168) as i16
            } else {
                ((i32::from(self.reg(s, !tone.pan)) * 0x168) / 0x7F) as i16
            }
        } else {
            tone.pan
        };
        let h = &mut self.slots[s];
        mask_set(&mut h.voices, granted.voice);
        let tpan = if effect.flags & 4 != 0 {
            pan_scale(i32::from(tone_pan), i32::from(h.scale))
        } else {
            i32::from(tone_pan)
        };
        let mut lr = [0; 2];
        effect_volume(
            &voices.pan,
            voices.out_mode,
            i32::from(h.volume),
            i32::from(h.pan),
            i32::from(tone_volume),
            tpan,
            &mut lr,
        );
        let key = KeyOn {
            owner: Owner::Effect(s),
            base: bank.base,
            group: effect.group as i8,
            lr,
            tone_volume,
            tone_pan,
            centre: 60,
            centre_fine: 0,
            bend: h.bend,
            pitch_mod: h.pitch_mod,
            tone,
        };
        voices.key_on(granted.voice, &key);
        0
    }

    // ------------------------------------------------ bend, pitch, volume

    fn set_bend(&mut self, voices: &mut Voices, s: usize, value: i16) {
        self.slots[s].bend_base = value;
        self.reapply_pitch(voices, s);
    }

    fn set_pmod(&mut self, voices: &mut Voices, s: usize, value: i16) {
        self.slots[s].pmod_base = value;
        self.reapply_pitch(voices, s);
    }

    fn reapply_pitch(&mut self, voices: &mut Voices, s: usize) {
        let h = &mut self.slots[s];
        h.bend = (i32::from(h.bend_base) + i32::from(h.bend_off)).clamp(-0x8000, 0x7FFF) as i16;
        h.pitch_mod = h.pmod_base.wrapping_add(h.pmod_off);
        let (bend, pm, paused) = (h.bend, h.pitch_mod, h.paused());
        for v in mask_voices(h.voices) {
            voices.repitch(v, bend, pm, paused);
        }
    }

    /// A running volume and pan change: `scale` 0x7FFFFFFF keeps the
    /// volume, a negative one is `|x| * 0x400 / 0x7F`; `pan` -2 keeps the
    /// pan, -1 is the sound's own. The voices change only if the volume or
    /// pan did.
    fn update_volume(&mut self, voices: &mut Voices, s: usize, scale: i32, pan: i32) {
        let sound_pan = self.slots[s].effect().map_or(0, |e| e.pan as i16);
        let flags = self.slots[s].effect().map_or(0, |e| e.flags);
        let h = &mut self.slots[s];
        if scale < 0 {
            h.scale = (scale.wrapping_mul(-0x400) / 0x7F) as i16;
        } else if scale != 0x7FFF_FFFF {
            h.scale = scale as i16;
        }
        if pan == -1 {
            h.pan_base = sound_pan;
        } else if pan != -2 {
            h.pan_base = pan as i16;
        }
        let vol = (((i32::from(h.scale) * i32::from(h.base_volume)) >> 10) + i32::from(h.vol_off))
            .clamp(0, 0x7F);
        let p = (i32::from(h.pan_base) + i32::from(h.pan_off)).rem_euclid(0x168);
        if vol != i32::from(h.volume) || p != i32::from(h.pan) {
            h.volume = vol as i16;
            h.pan = p as i16;
            let (volume, pan, scale, paused) = (h.volume, h.pan, h.scale, h.paused());
            for v in mask_voices(h.voices) {
                voices.revolume(v, volume, pan, scale, flags, paused);
            }
        }
    }

    /// `snd_SetSoundVolPan`.
    pub(crate) fn set_vol_pan(&mut self, voices: &mut Voices, handle: u32, vol: i32, pan: i32) {
        if let Some(s) = self.find(handle) {
            self.update_volume(voices, s, vol, pan);
            for c in self.slots[s].children.clone() {
                self.update_volume(voices, c, vol, pan);
            }
        }
    }

    /// `snd_SetSoundPitchModifier` and the like: the pitch modulation.
    pub(crate) fn set_pitch_mod(&mut self, voices: &mut Voices, handle: u32, value: i16) {
        if let Some(s) = self.find(handle) {
            self.set_pmod(voices, s, value);
        }
    }

    /// The bend of a playing sound.
    pub(crate) fn set_bend_of(&mut self, voices: &mut Voices, handle: u32, value: i16) {
        if let Some(s) = self.find(handle) {
            self.set_bend(voices, s, value);
        }
    }

    // ------------------------------------------------ stopping

    /// The stop section of a program (the grains after a type 43 marker)
    /// runs at once, after its voices are keyed off. True if voices the
    /// section started keep the handler alive.
    fn stop_section(&mut self, voices: &mut Voices, rng: &mut R250, s: usize) -> bool {
        let n = self.slots[s].effect().map_or(0, |e| e.grains.len());
        let label = self.slots[s].effect().and_then(|e| {
            e.grains
                .iter()
                .position(|g| g.kind == 43)
                .filter(|&k| k as i32 != i32::from(n as u8 as i8) - 1)
        });
        if let Some(k) = label {
            let m = self.slots[s].voices;
            let ends = voices.key_off(m, false);
            self.slots[s].voices = [0, 0];
            self.voice_ends(ends);
            self.slots[s].idx = (k + 1) as i16;
            let mut guard = 0;
            while self.slots[s].idx != -1 && self.slots[s].in_use() {
                guard += 1;
                if guard > MAX_GRAINS || self.run_grain(voices, rng, s) {
                    break;
                }
            }
            if !mask_empty(self.slots[s].voices) {
                return true;
            }
        }
        self.release_lfos(s);
        false
    }

    /// Stops handler `s`: its stop section, then its voices (keyed off, or
    /// killed when `kill` or paused), and its children.
    fn stop_path(&mut self, voices: &mut Voices, rng: &mut R250, s: usize, kill: bool) {
        if !self.slots[s].in_use() {
            return;
        }
        for c in self.slots[s].children.clone() {
            self.stop_path(voices, rng, c, kill);
            self.free(c);
        }
        self.slots[s].flags |= 4;
        if self.stop_section(voices, rng, s) {
            return;
        }
        let m = self.slots[s].voices;
        let ends = if !kill && !self.slots[s].paused() {
            voices.key_off(m, true)
        } else {
            voices.kill(m, true)
        };
        let others: Vec<VoiceEnd> = ends
            .into_iter()
            .filter(|e| e.owner != Owner::Effect(s))
            .collect();
        self.free(s);
        self.voice_ends(others);
    }

    /// `snd_StopSound`.
    pub(crate) fn stop(&mut self, voices: &mut Voices, rng: &mut R250, handle: u32) {
        if let Some(s) = self.find(handle) {
            self.stop_path(voices, rng, s, false);
        }
    }

    /// Stops everything playing from `bank` (it is being unloaded).
    pub(crate) fn stop_bank(&mut self, voices: &mut Voices, rng: &mut R250, bank: u32) {
        for c in self.order.clone() {
            if self.slots[c].in_use() && self.slots[c].bank_handle == bank {
                self.stop_path(voices, rng, c, true);
            }
        }
    }

    /// `snd_PauseSound`.
    pub(crate) fn pause(&mut self, voices: &mut Voices, handle: u32) {
        if let Some(s) = self.find(handle)
            && !self.slots[s].paused()
        {
            self.slots[s].flags |= 2;
            voices.pause(Owner::Effect(s));
        }
    }

    /// `snd_ContinueSound`.
    pub(crate) fn resume(&mut self, voices: &mut Voices, handle: u32) {
        if let Some(s) = self.find(handle)
            && self.slots[s].paused()
        {
            voices.resume(self.slots[s].voices);
            self.slots[s].flags &= !2;
        }
    }

    /// `snd_PauseAllSoundsInGroup`: the groups whose bits are set.
    pub(crate) fn pause_groups(&mut self, voices: &mut Voices, mask: u32) {
        for c in self.order.clone() {
            let h = &mut self.slots[c];
            if mask & (1u32 << (i32::from(h.group) & 0x1F)) != 0 && !h.paused() {
                h.flags |= 2;
                voices.pause(Owner::Effect(c));
            }
        }
    }

    /// `snd_ContinueAllSoundsInGroup`.
    pub(crate) fn resume_groups(&mut self, voices: &mut Voices, mask: u32) {
        for c in self.order.clone() {
            let h = &mut self.slots[c];
            if mask & (1u32 << (i32::from(h.group) & 0x1F)) != 0 && h.paused() {
                voices.resume(h.voices);
                h.flags &= !2;
            }
        }
    }

    // ------------------------------------------------ the tick

    /// One handler for one tick. True when it has finished.
    fn run_handler(&mut self, voices: &mut Voices, rng: &mut R250, s: usize) -> bool {
        if self.slots[s].idx == -1 {
            return false;
        }
        self.slots[s].wait = self.slots[s].wait.wrapping_sub(1);
        let mut guard = 0;
        while self.slots[s].wait < 1 {
            guard += 1;
            if guard > MAX_GRAINS || self.run_grain(voices, rng, s) {
                return true;
            }
            if self.slots[s].idx == -1 {
                return false;
            }
        }
        false
    }

    /// Every running handler, in start order (paused ones wait).
    pub(crate) fn tick(&mut self, voices: &mut Voices, rng: &mut R250) {
        for s in self.order.clone() {
            if self.slots[s].in_use() && !self.slots[s].paused() && self.run_handler(voices, rng, s)
            {
                self.stop_path(voices, rng, s, false);
            }
        }
    }

    // ------------------------------------------------ LFOs

    fn grain_lfo(&mut self, voices: &mut Voices, rng: &mut R250, s: usize, g: &Grain) -> i32 {
        let k = usize::from(g.u8_at(0));
        if k > 3 {
            return 0;
        }
        let target = g.u8_at(1);
        self.slots[s].lfo[k].target = target;
        if target == 0 {
            self.stop_lfo(s, k);
            self.slots[s].lfo[k].wave = 0;
        } else {
            let phase = if g.u8_at(8) & 2 == 0 {
                u32::from(g.u16_at(0x12))
            } else {
                u32::from(rng.next() & 0x7FF)
            };
            let sl = &mut self.slots[s].lfo[k];
            sl.wave = u16::from(g.u8_at(3));
            sl.link = g.u8_at(2);
            sl.flags = g.u8_at(8);
            sl.depth = g.u16_at(6);
            sl.depth_wide = u32::from(g.u16_at(6));
            sl.rate = g.u32_at(0xC);
            sl.rate_base = g.u32_at(0xC);
            sl.random_held = 0;
            sl.aux = if sl.wave == 2 {
                u32::from(g.u16_at(4))
            } else {
                0
            };
            sl.phase = phase << 16;
            self.start_lfo(voices, rng, s, k);
        }
        0
    }

    fn amplitude(sl: &mut LfoSlot, sound_volume: i8) {
        let d = i32::from(sl.depth);
        match sl.target {
            1 => sl.amp = i32::from(sound_volume).wrapping_mul(d) >> 10,
            2 => sl.amp = (d * 45) >> 8,
            3 => sl.amp = (d * 381) >> 6,
            4 => sl.amp = (d.wrapping_shl(15).wrapping_sub(d)) >> 10,
            _ => {}
        }
    }

    fn start_lfo(&mut self, voices: &mut Voices, rng: &mut R250, s: usize, k: usize) {
        let sound_volume = self.slots[s].effect().map_or(0, |e| e.volume);
        let sl = &mut self.slots[s].lfo[k];
        if sl.wave == 5 {
            let r1 = i32::from(rng.next());
            let r2 = i32::from(rng.next());
            sl.random_held = 1;
            sl.aux = (-(r1 & 0x7FFF)).wrapping_mul(r2 & 1) as u32;
        }
        Self::amplitude(sl, sound_volume);
        let listed = if sl.in_list {
            true
        } else if self.lfos.len() == MAX_LFOS {
            false
        } else {
            sl.in_list = true;
            self.lfos.push((s, k));
            true
        };
        if listed {
            let _ = voices;
            self.step_lfo(rng, s, k);
        }
    }

    fn stop_lfo(&mut self, s: usize, k: usize) {
        self.slots[s].lfo[k].in_list = false;
        self.lfos.retain(|&e| e != (s, k));
    }

    fn release_lfos(&mut self, s: usize) {
        for k in 0..4 {
            self.stop_lfo(s, k);
        }
    }

    fn wave_value(&self, sl: &mut LfoSlot, idx: i32, rng: &mut R250) -> i32 {
        match sl.wave {
            1 => usize::try_from(idx)
                .ok()
                .filter(|&i| i < SINE_LEN)
                .map_or(0, |i| i32::from(self.sine[i])),
            2 => {
                if idx < sl.aux as i32 {
                    0x7FFF
                } else {
                    -0x7FFF
                }
            }
            3 => triangle(idx),
            4 => saw(idx),
            5 => {
                if idx < 0x400 {
                    if sl.random_held == 0 {
                        sl.random_held = 1;
                        let r1 = i32::from(rng.next());
                        let r2 = i32::from(rng.next());
                        sl.aux = (-(r1 & 0x7FFF)).wrapping_mul(r2 & 1) as u32;
                    }
                } else if sl.random_held == 1 {
                    sl.random_held = 0;
                    let r = i32::from(rng.next());
                    sl.aux = (((r & 0x7FFF) - 0x3FFF) << 1) as u32;
                }
                sl.aux as i32
            }
            _ => 0,
        }
    }

    /// One step of an LFO: 1 if a volume or pan offset changed, 2 if a
    /// pitch offset did.
    fn step_lfo(&mut self, rng: &mut R250, s: usize, k: usize) -> u8 {
        let target = self.slots[s].lfo[k].target;
        if !(1..=6).contains(&target) {
            return 0;
        }
        let sound_volume = self.slots[s].effect().map_or(0, |e| e.volume);
        let mut sl = std::mem::take(&mut self.slots[s].lfo[k]);
        let old = sl.phase as i32;
        let mut new = old.wrapping_add((sl.rate as i32).wrapping_shl(1));
        let idx = old >> 16;
        if 0x7FF_FFFF < new {
            new = new.wrapping_add(0xF800_0000_u32 as i32);
        }
        sl.phase = new as u32;
        let mut w = self.wave_value(&mut sl, idx, rng);
        if sl.flags & 1 != 0 {
            w = w.wrapping_neg();
        }
        let (amp, depth, link) = (sl.amp, sl.depth, usize::from(sl.link));
        let h = &mut self.slots[s];
        h.lfo[k] = sl;
        match target {
            1 => {
                let v = amp.wrapping_mul(w.wrapping_sub(0x7FFF)) >> 16;
                if v == i32::from(h.vol_off) {
                    return 0;
                }
                h.vol_off = v as i16;
                1
            }
            2 => {
                let v = amp.wrapping_mul(w) >> 15;
                if v == i32::from(h.pan_off) {
                    return 0;
                }
                h.pan_off = v as i16;
                1
            }
            3 => {
                let v = amp.wrapping_mul(w) >> 15;
                if v == i32::from(h.pmod_off) {
                    return 0;
                }
                h.pmod_off = v as i16;
                2
            }
            4 => {
                let v = amp.wrapping_mul(w) >> 15;
                if v == i32::from(h.bend_off) {
                    return 0;
                }
                h.bend_off = v as i16;
                2
            }
            5 | 6 if link < 4 => {
                let o = &mut h.lfo[link];
                if target == 5 {
                    let v = ((o.rate_base >> 8).wrapping_mul(u32::from(depth))) >> 10;
                    let v = (v as i32).wrapping_mul(w) >> 15;
                    o.rate = o.rate_base.wrapping_add((v as u32).wrapping_shl(8));
                } else {
                    let v = (o.depth_wide.wrapping_mul(u32::from(depth))) >> 10;
                    let v = (v as i32).wrapping_mul(w) >> 15;
                    o.depth = (o.depth_wide as u16).wrapping_add(v as u16);
                    Self::amplitude(o, sound_volume);
                }
                0
            }
            _ => 0,
        }
    }

    /// The LFOs, every other tick: each steps, and a changed offset
    /// re-applies the handler's volume or pitch.
    pub(crate) fn tick_lfos(&mut self, voices: &mut Voices, rng: &mut R250) {
        let mut i = 0;
        while i < self.lfos.len() {
            let (s, k) = self.lfos[i];
            let r = self.step_lfo(rng, s, k);
            if r & 1 != 0 {
                self.update_volume(voices, s, 0x7FFF_FFFF, -2);
            }
            if r & 2 != 0 {
                self.reapply_pitch(voices, s);
            }
            i += 1;
        }
    }

    /// Stops every handler (killing its voices).
    pub(crate) fn stop_all(&mut self, voices: &mut Voices, rng: &mut R250) {
        for c in self.order.clone() {
            if self.slots[c].in_use() {
                self.stop_path(voices, rng, c, true);
            }
        }
    }
}
