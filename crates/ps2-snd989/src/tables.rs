//! The driver's arithmetic: the pan table, libsd's note-to-pitch
//! conversion, the volume and pan math, the LFO waves and the R250 random
//! numbers. Ported from rac-rs (`snd989/pitch.rs`, `voices.rs`, `lfo.rs`,
//! `rng.rs`), which follows the R&C 989snd instruction by instruction;
//! Sly 1's driver holds the same pan and wave tables (its 989SND.IRX has
//! them byte for byte), and its random generator is the same.

/// Entries of the pan table: one per half degree, from 0 to 89.5.
const PAN_ENTRIES: usize = 180;

/// The pan table: `floor(0x3FFF * cos(a))`, `floor(0x3FFF * sin(a))` at
/// half degrees, except that entry 1's right is 182 in the driver's data
/// (the formula gives 142).
#[derive(Clone, Debug)]
pub(crate) struct PanTable {
    pairs: [(i16, i16); PAN_ENTRIES],
}

impl Default for PanTable {
    fn default() -> Self {
        let mut pairs = [(0, 0); PAN_ENTRIES];
        for (i, p) in pairs.iter_mut().enumerate() {
            let a = (i as f64 * 0.5).to_radians();
            *p = (
                (16383.0 * a.cos()).floor() as i16,
                (16383.0 * a.sin()).floor() as i16,
            );
        }
        pairs[1].1 = 182;
        Self { pairs }
    }
}

/// The pan stage of the voice volume: amplitude `a` at angle `angle`
/// (degrees, not yet folded into 0..360), given the voice's previous left
/// and right in `out` (a voice behind the listener keeps its inverted
/// channel). `mode` is the output mode: 1 mono, 2 surround without phase
/// inversion, otherwise stereo.
pub(crate) fn pan_volume(table: &PanTable, mode: i16, a: i32, angle: i32, out: &mut [i16; 2]) {
    if a == 0 {
        *out = [0, 0];
        return;
    }
    if mode == 1 {
        *out = [a as i16, a as i16];
        return;
    }
    let mut idx = angle.rem_euclid(0x168);
    idx = if idx < 0x10E { idx + 0x5A } else { idx - 0x10E };
    let (o0, o1) = (i32::from(out[0]), i32::from(out[1]));
    if idx < 0xB4 {
        let (tl, tr) = table.pairs[idx as usize];
        let mut l = a.wrapping_mul(i32::from(tl)) / 0x3FFF;
        let mut r = a.wrapping_mul(i32::from(tr)) / 0x3FFF;
        if o0 < 0 && o1 < 0 {
            l = -l;
            r = -r;
        } else if o0 < 0 || o1 < 0 {
            let (a1, a0) = (o0.abs(), o1.abs());
            let f = if a0 < a1 { o0 / a1 } else { o1 / a0 };
            l = l.wrapping_mul(f);
            r = r.wrapping_mul(f);
        }
        *out = [l as i16, r as i16];
    } else {
        // Behind: the channels swap and, unless the mode says not to, one
        // is phase inverted.
        let (tl, tr) = table.pairs[(idx - 0xB4) as usize];
        let mut r = a.wrapping_mul(i32::from(tl)) / 0x3FFF;
        let mut l = a.wrapping_mul(i32::from(tr)) / 0x3FFF;
        if mode != 2 {
            if o0 >= 0 {
                if o1 >= 0 {
                    if r < l {
                        r = -r;
                    } else {
                        l = -l;
                    }
                } else {
                    r = -r;
                }
            } else if o1 >= 0 {
                l = -l;
            } else if l < r {
                r = -r;
            } else {
                l = -l;
            }
        }
        *out = [l as i16, r as i16];
    }
}

/// A sound effect tone's left and right: the handler's volume (0..127)
/// and pan, the tone's volume and pan.
pub(crate) fn effect_volume(
    table: &PanTable,
    mode: i16,
    volume: i32,
    pan: i32,
    tone_volume: i32,
    tone_pan: i32,
    out: &mut [i16; 2],
) {
    let t0 = (0x7F * 0x102_i32).wrapping_mul(tone_volume) / 0x7F;
    let a = t0.wrapping_mul(volume) / 0x7F;
    pan_volume(table, mode, a, tone_pan + pan, out);
}

/// A stream voice's left and right: the stream's volume and pan, its
/// channel's volume and pan, and the group byte volume.
#[allow(clippy::too_many_arguments, reason = "the driver's own arguments")]
pub(crate) fn stream_volume(
    table: &PanTable,
    mode: i16,
    volume: i32,
    pan: i32,
    channel_volume: i32,
    byte_volume: i32,
    channel_pan: i32,
    out: &mut [i16; 2],
) {
    let s1 = volume.wrapping_mul(0x102).wrapping_mul(0x7F) / 0x7F;
    let s2 = s1.wrapping_mul(channel_volume) / 0x7F;
    let a = s2.wrapping_mul(byte_volume) / 0x7F;
    pan_volume(table, mode, a, pan + channel_pan, out);
}

/// A music tone's left and right: the song's volume, the note's velocity
/// times its channel's volume (over 127), the pan (song and channel), the
/// program's volume and pan and the tone's.
pub(crate) fn music_volume(
    table: &PanTable,
    mode: i16,
    volumes: [i32; 4],
    pans: [i32; 3],
    out: &mut [i16; 2],
) {
    let [song, velocity, program, tone] = volumes;
    let mut a = song * 258;
    a = a * velocity / 0x7F;
    a = a * program / 0x7F;
    a = a * tone / 0x7F;
    pan_volume(table, mode, a, pans[0] + pans[1] + pans[2], out);
}

/// The group master volume (0..0x400) applied to a voice's left or right,
/// as a square law. Groups from 7 on pass the value through.
pub(crate) fn group_master(v: i16, group: i32, masters: &[i32; 9]) -> i16 {
    if !(0..7).contains(&group) {
        return v;
    }
    let v = i32::from(v).min(0x7FFE);
    let x = v.wrapping_mul(masters[group as usize]) / 1024;
    let sign = if x >= 0 { 1 } else { -1 };
    (sign * (x.wrapping_mul(x) / 0x7FFE)) as i16
}

/// The VOLL or VOLR register value the driver writes for `v`.
pub(crate) fn volume_register(v: i16, group: i32, masters: &[i32; 9]) -> u16 {
    (group_master(v, group, masters) as u16) >> 1
}

/// The pan scaling of sounds with flag bit 2: the pan narrows as the
/// volume scale drops.
pub(crate) fn pan_scale(pan: i32, scale: i32) -> i32 {
    if scale >= 0x356 {
        pan
    } else if scale <= 0xAA {
        0
    } else if pan < 0xB5 {
        pan * (scale - 0xAA) / 0x2AC
    } else {
        0x168 - (0x168 - pan) * (scale - 0xAA) / 0x2AC
    }
}

// ---------------------------------------------------------------- pitch

/// libsd's pitch tables: `floor(0x8000 * 2^(k/12))` for 12 semitones and
/// `floor(0x8000 * 2^(k/1536))` for 128 steps of a semitone.
#[derive(Clone, Debug)]
pub(crate) struct PitchTables {
    semitones: [u16; 12],
    fine: [u16; 128],
}

impl Default for PitchTables {
    fn default() -> Self {
        let mut semitones = [0; 12];
        let mut fine = [0; 128];
        for (k, v) in semitones.iter_mut().enumerate() {
            *v = (32768.0 * (k as f64 / 12.0).exp2()).floor() as u16;
        }
        for (k, v) in fine.iter_mut().enumerate() {
            *v = (32768.0 * (k as f64 / 1536.0).exp2()).floor() as u16;
        }
        Self { semitones, fine }
    }
}

/// The pitch bend term and the pitch modulation, added to the base note
/// and split into (note, fine) with fine in -127..=127.
fn split_note(
    bend_down: i8,
    bend_up: i8,
    bend: i32,
    pitch_mod: i32,
    base_note: i32,
    base_fine: i32,
) -> (i32, i32) {
    let term = if bend < 0 {
        let mut t = i32::from(bend_down) * bend * 0x80;
        if t < 0 {
            t += 0x7FFF;
        }
        t >> 15
    } else {
        (i32::from(bend_up) * bend * 0x80) / 0x7FFF
    };
    let total = base_note * 0x80 + base_fine + pitch_mod + term;
    let adj = if total < 0 { total + 0x7F } else { total };
    let note = adj >> 7;
    (note, total - note * 0x80)
}

/// libsd's `sceSdNote2Pitch(centre, centre_fine, note, fine)`, as the
/// instructions compute it.
fn note_to_pitch(tab: &PitchTables, centre: i32, centre_fine: i32, note: i32, fine: i32) -> u16 {
    let mut a3 = i32::from(fine as i16) + (centre_fine & 0xFFFF);
    let v1 = (if a3 < 0 { a3 + 0x7F } else { a3 }) >> 7;
    let n = i32::from((note + v1 - centre) as i16);
    let hi = ((i64::from(n) * 0x2AAA_AAAB_i64) >> 32) as i32;
    a3 -= v1 << 7;
    let q = (hi >> 1) - (n >> 31);
    let mut t0 = q - 2;
    let rem = n - 12 * q;
    let mut r = rem;
    let mut fix = true;
    if rem < 0 {
        r = rem + 12;
        t0 = q - 3;
    } else if rem == 0 {
        if a3 >= 0 {
            fix = false;
        } else {
            r = 12;
            t0 = q - 3;
        }
    }
    if fix && a3 < 0 {
        r = (r - 1) + v1;
        a3 += (v1 + 1) << 7;
    }
    let semitone = tab
        .semitones
        .get(i32::from(r as i16) as usize)
        .copied()
        .unwrap_or(0);
    let fine = tab.fine.get(a3 as usize).copied().unwrap_or(0);
    let p = (u32::from(semitone).wrapping_mul(u32::from(fine)) as i32) >> 16;
    let t0 = i32::from(t0 as i16);
    let p = if t0 < 0 {
        let s = (-t0) as u32;
        (p as u32).wrapping_add(1u32 << ((s - 1) & 31)) >> (s & 31)
    } else {
        p as u32
    };
    (p & 0xFFFF) as u16
}

/// A voice's PITCH register: the tone's centre and fine, its bend ranges,
/// the bend and pitch modulation, and the note played (60 for a sound
/// effect, the key for music). A non-negative centre byte marks a 44.1
/// kHz sample, scaled to the SPU2's 48 kHz.
#[allow(clippy::too_many_arguments, reason = "the driver's own arguments")]
pub(crate) fn voice_pitch(
    tab: &PitchTables,
    centre_byte: i8,
    centre_fine: i8,
    bend_down: i8,
    bend_up: i8,
    bend: i16,
    pitch_mod: i16,
    base_note: u8,
    base_fine: u8,
) -> u16 {
    let (note, fine) = split_note(
        bend_down,
        bend_up,
        i32::from(bend),
        i32::from(pitch_mod),
        i32::from(base_note),
        i32::from(base_fine),
    );
    let at_44k = centre_byte >= 0;
    let centre = if at_44k {
        centre_byte
    } else {
        centre_byte.wrapping_neg()
    };
    let p = u32::from(note_to_pitch(
        tab,
        i32::from(centre),
        i32::from(centre_fine),
        note,
        fine,
    ));
    let p = if at_44k { p * 0xAC44 / 48000 } else { p };
    (p & 0xFFFF) as u16
}

// ---------------------------------------------------------------- random

/// Values in the R250 table.
pub(crate) const R250_LEN: usize = 250;

/// The driver's random numbers: an R250 lag generator over a table the
/// module's data holds (it is never seeded), shared by every user in the
/// order they run.
#[derive(Clone, Debug)]
pub(crate) struct R250 {
    table: [u16; R250_LEN],
    index: usize,
}

impl R250 {
    /// From the module's table and index.
    pub(crate) fn new(table: &[u16; R250_LEN], index: usize) -> Self {
        Self {
            table: *table,
            index: index % R250_LEN,
        }
    }

    /// The next value.
    pub(crate) fn next(&mut self) -> u16 {
        let i = self.index;
        let j = if i < 0x93 { i + 0x67 } else { i - 0x93 };
        let v = self.table[i] ^ self.table[j];
        self.table[i] = v;
        self.index = if i > 0xF8 { 0 } else { i + 1 };
        v
    }
}

// ---------------------------------------------------------------- LFO waves

/// Entries of the LFO wave table.
pub(crate) const SINE_LEN: usize = 2048;

/// The LFO "sine" table, a cosine: `trunc(0x7FFF * cos(2 pi i / 2048))`.
pub(crate) fn sine_table() -> Vec<i16> {
    (0..SINE_LEN)
        .map(|i| (32767.0 * (std::f64::consts::TAU * i as f64 / SINE_LEN as f64).cos()) as i16)
        .collect()
}

/// The triangle wave at phase `idx` (0..2048).
pub(crate) fn triangle(idx: i32) -> i32 {
    if idx < 0x200 {
        ((idx << 15) - idx) >> 9
    } else if idx < 0x600 {
        let v1 = idx - 0x200;
        0x7FFF - ((((v1 << 15) - v1) << 1) >> 10)
    } else {
        let v1 = idx - 0x600;
        (((v1 << 15) - v1) >> 9) - 0x7FFF
    }
}

/// The saw wave at phase `idx`.
pub(crate) fn saw(idx: i32) -> i32 {
    if idx < 0x400 {
        let n = (idx << 15) - idx;
        let hi = ((i64::from(n) * i64::from(0x8020_0803_u32 as i32)) >> 32) as i32;
        (hi.wrapping_add(n) >> 9) - (n >> 31)
    } else {
        let v1 = idx - 0x400;
        (((v1 << 15) - v1) >> 10) - 0x7FFF
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitches() {
        let t = PitchTables::default();
        assert_eq!(voice_pitch(&t, -60, 0, 2, 2, 0, 0, 60, 0), 0x1000);
        assert_eq!(voice_pitch(&t, 60, 0, 2, 2, 0, 0, 60, 0), 3763);
        assert_eq!(voice_pitch(&t, -48, 0, 2, 2, 0, 0, 60, 0), 0x2000);
        assert_eq!(voice_pitch(&t, -72, 0, 2, 2, 0, 0, 60, 0), 0x0800);
        assert_eq!(
            voice_pitch(&t, -60, 0, 2, 2, 0, 128, 60, 0),
            voice_pitch(&t, -60, 0, 2, 2, 0, 0, 61, 0)
        );
        assert_eq!(split_note(2, 2, 0, -1, 60, 0), (59, 127));
        assert_eq!(split_note(2, 2, 0x7FFF, 0, 60, 0), (62, 0));
    }

    /// A stream at full volume, centred, in a group at full volume: the
    /// register value Sly 1's driver writes (0x1FFE).
    #[test]
    fn stream_volume_matches_the_driver() {
        let table = PanTable::default();
        let mut out = [0; 2];
        stream_volume(&table, 0, 127, 0, 127, 127, 0, &mut out);
        let masters = [0x400; 9];
        assert_eq!(volume_register(out[0], 2, &masters), 0x1FFE);
        assert_eq!(volume_register(out[1], 2, &masters), 0x1FFE);
    }

    #[test]
    fn waves() {
        assert_eq!(triangle(0), 0);
        assert_eq!(triangle(0x200), 0x7FFF);
        assert_eq!(saw(0), 0);
        assert_eq!(sine_table()[0], 0x7FFF);
    }
}
