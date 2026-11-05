//! The voice layer: 989snd's 48 voice records (24 per SPU2 core), their
//! allocation by volume group and priority (stealing the oldest of the
//! lowest priority), the key-on register image, the pending key-ons and
//! key-offs flushed once a tick, the end-of-voice poll on ENVX, and the
//! volume and pitch rewrites. Ported from rac-rs (`snd989/voices.rs`, an
//! exact port of the R&C driver, whose register order Sly 1's driver
//! shares), with the SPU2 itself left to `ps2-spu2`: the layer queues the
//! register writes, and the driver hands them to the chip in order.

use crate::bank::Tone;
use crate::tables::{
    PanTable, PitchTables, effect_volume, pan_scale, voice_pitch, volume_register,
};

/// Voices: 24 per core.
pub(crate) const VOICES: usize = 48;
/// The priority of stream voices: never stolen.
pub(crate) const STREAM_PRIORITY: u8 = 0x7F;
/// A voice of this priority or more is never stolen.
const NO_STEAL_PRIORITY: u8 = 0x7E;
/// Polls in a row with ENVX at 0 before a keyed voice counts as ended.
const END_COUNTDOWN: i8 = 4;

/// A set of voices: a 24-bit mask per core.
pub(crate) type VoiceMask = [u32; 2];

pub(crate) fn mask_has(m: VoiceMask, v: usize) -> bool {
    v < VOICES && m[v / 24] & (1 << (v % 24)) != 0
}

pub(crate) fn mask_set(m: &mut VoiceMask, v: usize) {
    m[v / 24] |= 1 << (v % 24);
}

pub(crate) fn mask_clear(m: &mut VoiceMask, v: usize) {
    m[v / 24] &= !(1 << (v % 24));
}

pub(crate) fn mask_empty(m: VoiceMask) -> bool {
    m[0] == 0 && m[1] == 0
}

pub(crate) fn mask_voices(m: VoiceMask) -> impl Iterator<Item = usize> {
    (0..VOICES).filter(move |&v| mask_has(m, v))
}

/// The SPU2 register addresses of the voices and cores.
pub(crate) mod reg {
    /// Core `c`'s register block.
    pub(crate) const fn core(c: usize) -> u32 {
        0x1F90_0000 + 0x400 * c as u32
    }

    /// Voice `v`'s parameter register at `offset` (0 VOLL, 2 VOLR, 4
    /// PITCH, 6 ADSR1, 8 ADSR2, 0xA ENVX).
    pub(crate) const fn voice(v: usize, offset: u32) -> u32 {
        core(v / 24) + 0x10 * (v % 24) as u32 + offset
    }

    /// Voice `v`'s address register at `offset` (0 SSA, 4 LSAX, 8 NAX),
    /// high half first.
    pub(crate) const fn address(v: usize, offset: u32) -> u32 {
        core(v / 24) + 0x1C0 + 12 * (v % 24) as u32 + offset
    }

    pub(crate) const VOLL: u32 = 0;
    pub(crate) const VOLR: u32 = 2;
    pub(crate) const PITCH: u32 = 4;
    pub(crate) const ADSR1: u32 = 6;
    pub(crate) const ADSR2: u32 = 8;
    pub(crate) const ENVX: u32 = 0xA;
    pub(crate) const SSA: u32 = 0;
    pub(crate) const LSAX: u32 = 4;
    pub(crate) const NAX: u32 = 8;
    /// Per-core voice masks, low half (voices 0..15) first.
    pub(crate) const VMIXL: u32 = 0x188;
    pub(crate) const VMIXEL: u32 = 0x18C;
    pub(crate) const VMIXR: u32 = 0x190;
    pub(crate) const VMIXER: u32 = 0x194;
    pub(crate) const KON: u32 = 0x1A0;
    pub(crate) const KOFF: u32 = 0x1A4;
    /// Core 1's master volume, left and right.
    pub(crate) const MVOL1: u32 = 0x1F90_0788;
}

/// What owns a voice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    /// A sound effect's handler slot.
    Effect(usize),
    /// A music handler slot.
    Music(usize),
}

/// A voice that ended or was taken away, with its owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VoiceEnd {
    pub(crate) voice: usize,
    pub(crate) owner: Owner,
}

/// A voice granted by the pool; `stolen` is the owner of a lower-priority
/// voice taken over, to be told before the new tone keys on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Granted {
    pub(crate) voice: usize,
    pub(crate) stolen: Option<VoiceEnd>,
}

/// A voice record's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Free,
    /// Keyed, in the active list.
    Keyed,
    /// Claimed by the pool, not keyed yet.
    Claimed,
    /// A stream's voice.
    Stream,
}

/// A voice record.
#[derive(Clone, Debug)]
struct Record {
    state: State,
    owner: Option<Owner>,
    /// The owner is told when the voice ends.
    callback: bool,
    tone: Option<Tone>,
    /// The note and fine the tone plays at (60 and 0 for a sound effect).
    centre: u8,
    centre_fine: u8,
    priority: u8,
    group: i8,
    /// The tick of its allocation or key-on.
    stamp: u32,
    /// Left and right before the group master.
    l: i16,
    r: i16,
    bend: i16,
    pitch_mod: i16,
    tone_volume: i8,
    tone_pan: i16,
    countdown: i8,
}

impl Default for Record {
    fn default() -> Self {
        Self {
            state: State::Free,
            owner: None,
            callback: false,
            tone: None,
            centre: 60,
            centre_fine: 0,
            priority: 0,
            group: 0,
            stamp: 0,
            l: 0,
            r: 0,
            bend: 0,
            pitch_mod: 0,
            tone_volume: 0,
            tone_pan: 0,
            countdown: 0,
        }
    }
}

/// Everything a key-on needs.
#[derive(Clone, Debug)]
pub(crate) struct KeyOn {
    pub(crate) owner: Owner,
    pub(crate) tone: Tone,
    /// Where the bank's samples sit in sound memory (bytes).
    pub(crate) base: u32,
    pub(crate) group: i8,
    /// Left and right, already worked out.
    pub(crate) lr: [i16; 2],
    pub(crate) tone_volume: i8,
    pub(crate) tone_pan: i16,
    pub(crate) centre: u8,
    pub(crate) centre_fine: u8,
    pub(crate) bend: i16,
    pub(crate) pitch_mod: i16,
}

/// The voice layer.
pub(crate) struct Voices {
    rec: [Record; VOICES],
    /// The first and last voice of each volume group.
    ranges: [(i16, i16); 8],
    /// Group master volumes (0..0x400); index 8 is the master volume.
    pub(crate) masters: [i32; 9],
    /// Output mode: 1 mono, 2 surround without phase inversion.
    pub(crate) out_mode: i16,
    pub(crate) pan: PanTable,
    pitch: PitchTables,
    pending_kon: VoiceMask,
    pending_koff: VoiceMask,
    on_mask: VoiceMask,
    off_mask: VoiceMask,
    reverb: VoiceMask,
    vmix_l: VoiceMask,
    vmix_r: VoiceMask,
    /// Keyed voices, in key-on order.
    active: Vec<usize>,
    pub(crate) tick: u32,
    /// Register writes waiting for the chip, in order.
    pub(crate) writes: Vec<(u32, u16)>,
}

impl Voices {
    pub(crate) fn new() -> Self {
        Self {
            rec: std::array::from_fn(|_| Record::default()),
            ranges: [(0, 0x2F); 8],
            masters: [0x400; 9],
            out_mode: 0,
            pan: PanTable::default(),
            pitch: PitchTables::default(),
            pending_kon: [0; 2],
            pending_koff: [0; 2],
            on_mask: [0; 2],
            off_mask: [0; 2],
            reverb: [0; 2],
            // The driver's init turns every voice on in the dry mix.
            vmix_l: [0xFF_FFFF; 2],
            vmix_r: [0xFF_FFFF; 2],
            active: Vec::new(),
            tick: 0,
            writes: Vec::new(),
        }
    }

    // ------------------------------------------------ registers

    pub(crate) fn write(&mut self, address: u32, value: u16) {
        self.writes.push((address, value));
    }

    fn write_mask(&mut self, core: usize, offset: u32, mask: u32) {
        let at = reg::core(core) + offset;
        self.write(at, (mask & 0xFFFF) as u16);
        self.write(at + 2, (mask >> 16) as u16);
    }

    /// Voice `v`'s start address (a byte address in sound memory).
    pub(crate) fn write_address(&mut self, v: usize, offset: u32, byte_address: u32) {
        let halfwords = byte_address / 2;
        self.write(reg::address(v, offset), (halfwords >> 16) as u16);
        self.write(reg::address(v, offset) + 2, (halfwords & 0xFFFF) as u16);
    }

    fn write_volumes(&mut self, v: usize) {
        let (l, r, g) = (self.rec[v].l, self.rec[v].r, i32::from(self.rec[v].group));
        let (wl, wr) = (
            volume_register(l, g, &self.masters),
            volume_register(r, g, &self.masters),
        );
        self.write(reg::voice(v, reg::VOLL), wl);
        self.write(reg::voice(v, reg::VOLR), wr);
    }

    fn pitch_of(&self, v: usize, bend: i16, pitch_mod: i16) -> Option<u16> {
        let r = &self.rec[v];
        let t = r.tone.as_ref()?;
        Some(voice_pitch(
            &self.pitch,
            t.centre_note,
            t.centre_fine,
            t.bend_down,
            t.bend_up,
            bend,
            pitch_mod,
            r.centre,
            r.centre_fine,
        ))
    }

    // ------------------------------------------------ groups

    /// `snd_SetMixerMode`'s voice mode: mode 1 splits the voices (every
    /// group on the upper 24, group 1 on the lower), any other mode gives
    /// every group all 48.
    pub(crate) fn set_voice_mode(&mut self, mode: i32) {
        if mode == 1 {
            self.ranges = [(0x18, 0x2F); 8];
            self.ranges[1] = (0, 0x17);
        } else {
            self.ranges = [(0, 0x2F); 8];
        }
    }

    /// `snd_SetGroupVoiceRange`.
    pub(crate) fn set_group_range(&mut self, group: usize, first: i32, last: i32) {
        if group < 8 {
            self.ranges[group] = (
                first.max(0) as i16,
                if last >= 0x30 { 0x2F } else { last } as i16,
            );
        }
    }

    /// `snd_SetMasterVolume`: groups 0..6 rewrite their running voices'
    /// volumes; group 8 is the master volume, written to core 1.
    pub(crate) fn set_group_volume(&mut self, group: i32, volume: i32) {
        let vol = volume.clamp(0, 0x400);
        if group == 7 {
            return;
        }
        if (0..=8).contains(&group) {
            self.masters[group as usize] = vol;
        }
        if group == 8 {
            let m = ((vol * 0x3FFF) / 1024) as u16;
            self.write(reg::MVOL1, m);
            self.write(reg::MVOL1 + 2, m);
        } else if (0..7).contains(&group) {
            for v in 0..VOICES {
                if matches!(self.rec[v].state, State::Keyed | State::Stream)
                    && i32::from(self.rec[v].group) == group
                {
                    self.write_volumes(v);
                }
            }
        }
    }

    // ------------------------------------------------ allocation

    fn claim(&mut self, v: usize, priority: u8, group: usize) {
        let r = &mut self.rec[v];
        r.state = State::Claimed;
        r.stamp = self.tick;
        r.priority = priority;
        r.group = group as i8;
        r.countdown = END_COUNTDOWN;
    }

    /// A voice for (group, priority): the first free voice of the group's
    /// range, or else the lowest priority, oldest voice there, if its
    /// priority is not above `priority`.
    pub(crate) fn alloc(&mut self, group: i8, priority: i8) -> Option<Granted> {
        let g = i32::from(group).clamp(0, 7) as usize;
        let (first, last) = self.ranges[g];
        let (lo, hi) = (
            i32::from(first).max(0),
            (i32::from(last) + 1).min(VOICES as i32),
        );
        for v in lo..hi {
            if self.rec[v as usize].state == State::Free {
                self.claim(v as usize, priority as u8, g);
                return Some(Granted {
                    voice: v as usize,
                    stolen: None,
                });
            }
        }
        if lo >= hi {
            return None;
        }
        let mut best = lo as usize;
        for v in (lo + 1)..hi {
            let (c, b) = (&self.rec[v as usize], &self.rec[best]);
            if c.priority < b.priority || (c.priority == b.priority && c.stamp < b.stamp) {
                best = v as usize;
            }
        }
        let vp = self.rec[best].priority;
        if i32::from(priority) < i32::from(vp) || vp >= NO_STEAL_PRIORITY {
            return None;
        }
        let stolen = self.callback_end(best);
        self.list_remove(best);
        self.claim(best, priority as u8, g);
        self.rec[best].callback = false;
        Some(Granted {
            voice: best,
            stolen,
        })
    }

    /// A stream's voice: priority 0x7F, never stolen, not polled.
    pub(crate) fn alloc_stream(&mut self, group: i8) -> Option<Granted> {
        let g = self.alloc(group, STREAM_PRIORITY as i8)?;
        let r = &mut self.rec[g.voice];
        r.state = State::Stream;
        r.owner = None;
        r.tone = None;
        Some(g)
    }

    // ------------------------------------------------ key on and off

    /// Sets up a key-on: the register image now, the KON bit at the end of
    /// the tick. Register order: VOLL, VOLR, VMIXL, VMIXR, PITCH, SSA,
    /// ADSR1, ADSR2.
    pub(crate) fn key_on(&mut self, v: usize, k: &KeyOn) {
        {
            let r = &mut self.rec[v];
            r.owner = Some(k.owner);
            r.callback = true;
            r.tone = Some(k.tone.clone());
            r.tone_volume = k.tone_volume;
            r.tone_pan = k.tone_pan;
            r.centre = k.centre;
            r.centre_fine = k.centre_fine;
            r.bend = k.bend;
            r.pitch_mod = k.pitch_mod;
            r.group = k.group;
            r.l = k.lr[0];
            r.r = k.lr[1];
            r.stamp = self.tick;
        }
        let (core, bit) = (v / 24, 1u32 << (v % 24));
        self.write_volumes(v);
        self.vmix_l[core] |= bit;
        self.write_mask(core, reg::VMIXL, self.vmix_l[core]);
        self.vmix_r[core] |= bit;
        self.write_mask(core, reg::VMIXR, self.vmix_r[core]);
        let pitch = self.pitch_of(v, k.bend, k.pitch_mod).unwrap_or(0);
        self.write(reg::voice(v, reg::PITCH), pitch);
        self.write_address(v, reg::SSA, k.base + k.tone.sample);
        self.write(reg::voice(v, reg::ADSR1), k.tone.adsr1);
        self.write(reg::voice(v, reg::ADSR2), k.tone.adsr2);
        if k.tone.reverb() {
            self.reverb[core] |= bit;
        } else {
            self.reverb[core] &= !bit;
        }
        self.pending_kon[core] |= bit;
        self.list_add(v);
    }

    /// A stream voice's key-on: volumes, dry mix, pitch, start address and
    /// envelope as given.
    pub(crate) fn key_on_stream(
        &mut self,
        v: usize,
        lr: [i16; 2],
        pitch: u16,
        ssa: u32,
        group: i8,
    ) {
        let (core, bit) = (v / 24, 1u32 << (v % 24));
        {
            let r = &mut self.rec[v];
            r.l = lr[0];
            r.r = lr[1];
            r.group = group;
            r.stamp = self.tick;
        }
        self.write_volumes(v);
        self.vmix_l[core] |= bit;
        self.write_mask(core, reg::VMIXL, self.vmix_l[core]);
        self.vmix_r[core] |= bit;
        self.write_mask(core, reg::VMIXR, self.vmix_r[core]);
        self.write(reg::voice(v, reg::PITCH), pitch);
        self.write_address(v, reg::SSA, ssa);
        self.write(reg::voice(v, reg::ADSR1), 0xF);
        self.write(reg::voice(v, reg::ADSR2), 0x1FC0);
        self.reverb[core] &= !bit;
        self.pending_kon[core] |= bit;
    }

    /// A stream voice's volume, from new left and right.
    pub(crate) fn stream_volume(&mut self, v: usize, lr: [i16; 2]) {
        self.rec[v].l = lr[0];
        self.rec[v].r = lr[1];
        self.write_volumes(v);
    }

    /// Releases a stream voice: it joins the active list with priority 0
    /// and no owner, and keys off at the end of the tick; the end poll
    /// frees it.
    pub(crate) fn stream_release(&mut self, v: usize) {
        self.list_add(v);
        self.rec[v].priority = 0;
        self.rec[v].callback = false;
        let (core, bit) = (v / 24, 1u32 << (v % 24));
        self.pending_kon[core] &= !bit;
        self.pending_koff[core] |= bit;
    }

    /// Keys off the keyed voices of `mask` at the end of the tick; with
    /// `notify`, their owners hear of it now.
    pub(crate) fn key_off(&mut self, mask: VoiceMask, notify: bool) -> Vec<VoiceEnd> {
        let mut ends = Vec::new();
        for v in mask_voices(mask) {
            if self.rec[v].state != State::Keyed {
                continue;
            }
            let bit = 1u32 << (v % 24);
            self.pending_koff[v / 24] |= bit;
            if notify && let Some(e) = self.callback_end(v) {
                ends.push(e);
            }
            self.rec[v].callback = false;
            self.pending_kon[v / 24] &= !bit;
        }
        ends
    }

    /// Silences the voices of `mask` at once and frees them.
    pub(crate) fn kill(&mut self, mask: VoiceMask, notify: bool) -> Vec<VoiceEnd> {
        let mut ends = Vec::new();
        for v in mask_voices(mask) {
            if notify
                && self.rec[v].state == State::Keyed
                && let Some(e) = self.callback_end(v)
            {
                ends.push(e);
            }
            self.rec[v].callback = false;
            if self.rec[v].priority != STREAM_PRIORITY {
                self.list_remove(v);
            }
            self.write(reg::voice(v, reg::VOLL), 0);
            self.write(reg::voice(v, reg::VOLR), 0);
            let bit = 1u32 << (v % 24);
            self.pending_kon[v / 24] &= !bit;
            self.pending_koff[v / 24] |= bit;
        }
        ends
    }

    /// Pauses every voice `owner` has: volume and pitch to 0.
    pub(crate) fn pause(&mut self, owner: Owner) {
        for v in 0..VOICES {
            if self.rec[v].state != State::Free && self.rec[v].owner == Some(owner) {
                self.write(reg::voice(v, reg::VOLL), 0);
                self.write(reg::voice(v, reg::VOLR), 0);
                self.write(reg::voice(v, reg::PITCH), 0);
            }
        }
    }

    /// Continues the voices of `mask`: volume and pitch back.
    pub(crate) fn resume(&mut self, mask: VoiceMask) {
        for v in mask_voices(mask) {
            self.write_volumes(v);
            let (bend, pm) = (self.rec[v].bend, self.rec[v].pitch_mod);
            if let Some(p) = self.pitch_of(v, bend, pm) {
                self.write(reg::voice(v, reg::PITCH), p);
            }
        }
    }

    /// A sound effect voice's volume after its handler's changed.
    pub(crate) fn revolume(
        &mut self,
        v: usize,
        volume: i16,
        pan: i16,
        scale: i16,
        sound_flags: u16,
        paused: bool,
    ) {
        let r = &self.rec[v];
        let Some(tone) = r.tone.clone() else { return };
        let tpan = if sound_flags & 4 != 0 {
            pan_scale(i32::from(r.tone_pan), i32::from(scale))
        } else {
            i32::from(r.tone_pan)
        };
        let mut out = [r.l, r.r];
        effect_volume(
            &self.pan,
            self.out_mode,
            i32::from(volume),
            i32::from(pan),
            i32::from(r.tone_volume),
            tpan,
            &mut out,
        );
        self.rec[v].l = out[0];
        self.rec[v].r = out[1];
        if !paused {
            self.write_volumes(v);
        }
        self.rec[v].priority = crate::effects::tone_priority(scale, &tone) as u8;
    }

    /// A music voice's volume, from new left and right.
    pub(crate) fn set_lr(&mut self, v: usize, lr: [i16; 2], paused: bool) {
        self.rec[v].l = lr[0];
        self.rec[v].r = lr[1];
        if !paused {
            self.write_volumes(v);
        }
    }

    /// A voice's left and right as stored (the sign of a rear voice).
    pub(crate) fn lr(&self, v: usize) -> [i16; 2] {
        [self.rec[v].l, self.rec[v].r]
    }

    /// The note a voice plays its tone at (`snd_SetSoundPitch`), in
    /// semitones and 1/128ths.
    pub(crate) fn set_note(&mut self, v: usize, note: u8, fine: u8) {
        self.rec[v].centre = note;
        self.rec[v].centre_fine = fine;
    }

    /// A voice's bend and pitch modulation changed.
    pub(crate) fn repitch(&mut self, v: usize, bend: i16, pitch_mod: i16, paused: bool) {
        self.rec[v].bend = bend;
        self.rec[v].pitch_mod = pitch_mod;
        if !paused && let Some(p) = self.pitch_of(v, bend, pitch_mod) {
            self.write(reg::voice(v, reg::PITCH), p);
        }
    }

    /// The pending key-offs, at the end of a tick.
    pub(crate) fn flush_key_offs(&mut self) {
        if self.pending_koff == [0, 0] {
            return;
        }
        let koff = self.pending_koff;
        self.write_mask(0, reg::KOFF, koff[0]);
        self.write_mask(1, reg::KOFF, koff[1]);
        self.pending_koff = [0, 0];
        for (c, &k) in koff.iter().enumerate() {
            self.on_mask[c] &= !k;
            self.off_mask[c] |= k;
        }
    }

    /// The pending key-ons, after the key-offs: the reverb sends, then KON.
    pub(crate) fn flush_key_ons(&mut self) {
        if self.pending_kon == [0, 0] {
            return;
        }
        let kon = self.pending_kon;
        let rev = self.reverb;
        self.write_mask(0, reg::VMIXEL, rev[0]);
        self.write_mask(0, reg::VMIXER, rev[0]);
        self.write_mask(1, reg::VMIXEL, rev[1]);
        self.write_mask(1, reg::VMIXER, rev[1]);
        self.write_mask(0, reg::KON, kon[0]);
        self.write_mask(1, reg::KON, kon[1]);
        self.pending_kon = [0, 0];
        for (c, &k) in kon.iter().enumerate() {
            self.off_mask[c] &= !k;
            self.on_mask[c] |= k;
        }
    }

    // ------------------------------------------------ the end poll

    /// The voices the end poll reads ENVX of.
    pub(crate) fn polled(&self) -> Vec<usize> {
        self.active
            .iter()
            .copied()
            .filter(|&v| self.pending_kon[v / 24] & (1 << (v % 24)) == 0)
            .collect()
    }

    /// The end poll, given each polled voice's ENVX: a voice still
    /// sounding reloads its countdown; a silent one that was keyed off is
    /// freed; a silent keyed one counts down and ends after four polls.
    pub(crate) fn poll_end(&mut self, envx: &[(usize, u16)]) -> Vec<VoiceEnd> {
        let mut ends = Vec::new();
        for &(v, e) in envx {
            let (core, bit) = (v / 24, 1u32 << (v % 24));
            if !self.active.contains(&v) || self.pending_kon[core] & bit != 0 {
                continue;
            }
            if e != 0 {
                self.rec[v].countdown = END_COUNTDOWN;
            } else if self.off_mask[core] & bit != 0 {
                if self.rec[v].priority != STREAM_PRIORITY {
                    self.list_remove(v);
                }
                if let Some(e) = self.callback_end(v) {
                    ends.push(e);
                }
                self.rec[v].callback = false;
                self.off_mask[core] &= !bit;
            } else if self.on_mask[core] & bit != 0 {
                let c = self.rec[v].countdown.wrapping_sub(1);
                self.rec[v].countdown = c;
                if c == 0 {
                    if self.rec[v].state == State::Keyed
                        && let Some(e) = self.callback_end(v)
                    {
                        ends.push(e);
                    }
                    self.rec[v].callback = false;
                    self.pending_kon[core] &= !bit;
                    self.pending_koff[core] |= bit;
                } else if c < 0 {
                    self.rec[v].countdown = END_COUNTDOWN;
                }
            }
        }
        ends
    }

    // ------------------------------------------------ the active list

    fn list_add(&mut self, v: usize) {
        self.rec[v].state = State::Keyed;
        if !self.active.contains(&v) {
            self.active.push(v);
        }
    }

    fn list_remove(&mut self, v: usize) {
        self.rec[v].state = State::Free;
        self.rec[v].owner = None;
        self.active.retain(|&x| x != v);
    }

    fn callback_end(&self, v: usize) -> Option<VoiceEnd> {
        match (self.rec[v].callback, self.rec[v].owner) {
            (true, Some(owner)) => Some(VoiceEnd { voice: v, owner }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone() -> Tone {
        Tone {
            priority: 10,
            volume: 127,
            centre_note: -60,
            centre_fine: 0,
            pan: 0,
            map_low: 0,
            map_high: 127,
            bend_down: 2,
            bend_up: 2,
            adsr1: 0x80FF,
            adsr2: 0x9FC0,
            flags: 0,
            sample: 0x100,
        }
    }

    /// The key-on register order Sly 1's driver writes, then the KON at the
    /// tick's end.
    #[test]
    fn key_on_writes_in_the_driver_order() {
        let mut voices = Voices::new();
        let g = voices.alloc(0, 10).map(|g| g.voice);
        assert_eq!(g, Some(0));
        voices.key_on(
            0,
            &KeyOn {
                owner: Owner::Effect(0),
                tone: tone(),
                base: 0x5040,
                group: 0,
                lr: [0, 0],
                tone_volume: 127,
                tone_pan: 0,
                centre: 60,
                centre_fine: 0,
                bend: 0,
                pitch_mod: 0,
            },
        );
        let addresses: Vec<u32> = voices.writes.iter().map(|w| w.0 & 0x7FF).collect();
        assert_eq!(
            addresses,
            [
                0x0, 0x2, 0x188, 0x18A, 0x190, 0x192, 0x4, 0x1C0, 0x1C2, 0x6, 0x8
            ]
        );
        assert_eq!(voices.writes[6].1, 0x1000);
        assert_eq!(voices.writes[8].1, (0x5140 / 2) as u16);
        voices.writes.clear();
        voices.flush_key_offs();
        voices.flush_key_ons();
        let kon: Vec<(u32, u16)> = voices.writes.clone();
        assert_eq!(kon[8], (0x1F90_01A0, 1));
    }

    /// A keyed voice ends after four silent polls, and its owner hears.
    #[test]
    fn a_silent_voice_ends_after_four_polls() {
        let mut voices = Voices::new();
        let v = voices.alloc(0, 10).map_or(99, |g| g.voice);
        voices.key_on(
            v,
            &KeyOn {
                owner: Owner::Effect(3),
                tone: tone(),
                base: 0,
                group: 0,
                lr: [0, 0],
                tone_volume: 127,
                tone_pan: 0,
                centre: 60,
                centre_fine: 0,
                bend: 0,
                pitch_mod: 0,
            },
        );
        voices.flush_key_ons();
        for _ in 0..3 {
            assert_eq!(voices.poll_end(&[(v, 0)]), Vec::new());
        }
        let ends = voices.poll_end(&[(v, 0)]);
        assert_eq!(
            ends,
            [VoiceEnd {
                voice: v,
                owner: Owner::Effect(3)
            }]
        );
        voices.flush_key_offs();
        assert_eq!(voices.poll_end(&[(v, 0)]), Vec::new());
        assert_eq!(voices.polled(), Vec::<usize>::new());
    }
}
