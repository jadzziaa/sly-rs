//! The reverb: 989snd's effect state per core (its type, its work area in
//! sound memory, the depth) and libsd's `sceSdSetEffectAttr`, which loads a
//! preset into the core's reverb registers.
//!
//! The driver's logic is ported from rac-rs (`snd989/reverb.rs`, the R&C
//! driver's reverb, whose commands Sly 1's build numbers 14, 15 and 81):
//! a type change keys the effect off, frees the old work area and takes a
//! new one from the top of a 128 KiB page of sound memory, points EEA at
//! its end, loads the preset, clears the area, and switches the effect on
//! ten ticks later with the depth as the reverb return volume (EVOL). A
//! work area reserved with `snd_PreAllocReverbWorkArea` stays put.
//!
//! The presets are libsd's (ten of them, PS1-style: two all-pass sizes,
//! eight volumes, twenty addresses in 8-byte units, two input volumes),
//! read from the disc's `LIBSD.IRX`; the register writes and their order
//! are those of Sly 1's libsd for type 0 (off), seen in its traffic:
//! the reverb return off, the all-pass sizes, the volumes, the addresses
//! (in halfwords, four per unit), the input volumes, then ESA, the work
//! area's start: EEA's page end less the type's size. Echo and delay
//! have their delay and feedback set as the driver asks (0 and 0). All ten
//! types match the disc driver's writes, call for call (checked by
//! injecting `snd_SetReverbType` into both drivers).

use crate::bank::SpuMemory;

/// Reverb types (libsd's modes: off, room, studio A, B, C, hall, space,
/// echo, delay, pipe).
pub(crate) const TYPES: usize = 10;
/// Halfwords of a preset: 32 registers.
const PRESET_LEN: usize = 32;
/// Driver ticks the work area's clear takes before the effect goes on.
const PENDING_TICKS: u32 = 10;

/// The work-area size of each type, in libsd's units of 8 bytes (the
/// libsd table, the same in every build seen).
const UNITS: [u32; TYPES] = [
    2, 0x4D8, 0x3E8, 0x908, 0xDFC, 0x15BC, 0x1ED8, 0x3008, 0x3008, 0x780,
];

/// libsd's presets.
#[derive(Clone, Debug)]
pub(crate) struct Presets {
    regs: [[u16; PRESET_LEN]; TYPES],
}

impl Presets {
    /// Finds the preset table in a libsd module image: records of a word
    /// and 32 halfwords, "room" (type 1) starting with its all-pass sizes
    /// 0x7D and 0x5B and IIR volume 0x6D80.
    pub(crate) fn from_libsd(image: &[u8]) -> Option<Self> {
        let room = [0x7D_u8, 0, 0x5B, 0, 0x80, 0x6D];
        let at = image.windows(room.len()).position(|w| w == room)?;
        let first = at.checked_sub(4 + 0x44)? + 4;
        let mut regs = [[0; PRESET_LEN]; TYPES];
        for (t, preset) in regs.iter_mut().enumerate() {
            let base = first + t * 0x44;
            for (k, r) in preset.iter_mut().enumerate() {
                let b = image.get(base + 2 * k..base + 2 * k + 2)?;
                *r = u16::from_le_bytes([b[0], b[1]]);
            }
        }
        // Off: only the "same" and "diff" destinations are 1.
        (regs[0][10] == 1 && regs[0][18] == 1).then_some(Self { regs })
    }

    /// Type 0 alone, as Sly 1's libsd holds it (when no libsd image is
    /// at hand).
    pub(crate) fn off_only() -> Self {
        let mut regs = [[0; PRESET_LEN]; TYPES];
        for t in &mut regs {
            t[10] = 1;
            t[11] = 1;
            t[18] = 1;
            t[19] = 1;
        }
        Self { regs }
    }
}

/// The reverb state.
#[derive(Clone, Debug)]
pub(crate) struct Reverb {
    presets: Presets,
    /// Each core's type (-1: never set).
    mode: [i32; 2],
    /// Each core's work area (bytes).
    area: [u32; 2],
    /// The type a reserved work area was sized for (-1 or 0: none).
    fixed: [i32; 2],
    /// The depth, left and right.
    depth: [[u16; 2]; 2],
    /// A work area is being cleared.
    clearing: [bool; 2],
    pending: [u32; 2],
    /// Each core's EEA page, as last written.
    eea: [u32; 2],
}

/// A write the reverb makes: a register and a value, or a clear of sound
/// memory (address, bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Write(u32, u16),
    Clear(u32, u32),
    /// ATTR's effect bit, on or off.
    Enable(usize, bool),
}

fn size(mode: i32) -> Option<u32> {
    usize::try_from(mode)
        .ok()
        .and_then(|m| UNITS.get(m))
        .map(|u| u * 8)
}

/// Core `c`'s register block, and its volume block.
fn blocks(c: usize) -> (u32, u32) {
    (
        0x1F90_0000 + 0x400 * c as u32,
        0x1F90_0760 + 0x28 * c as u32,
    )
}

impl Reverb {
    pub(crate) fn new(presets: Presets) -> Self {
        Self {
            presets,
            mode: [-1; 2],
            area: [0; 2],
            fixed: [-1; 2],
            depth: [[0; 2]; 2],
            clearing: [false; 2],
            pending: [0; 2],
            eea: [0xE, 0xF],
        }
    }

    /// `snd_PreAllocReverbWorkArea(cores, type)`: a work area for `mode`
    /// reserved on each core of the mask.
    pub(crate) fn reserve(
        &mut self,
        mem: &mut SpuMemory,
        cores: u32,
        mode: i32,
        out: &mut Vec<Op>,
    ) {
        for c in 0..2 {
            if cores & (1 << c) == 0 {
                continue;
            }
            if self.mode[c] > 0 {
                self.change(mem, c, 0, out);
            }
            if self.fixed[c] > 0
                && let Some(n) = size(self.fixed[c])
            {
                mem.release(self.area[c], n);
            }
            self.fixed[c] = mode;
            if mode > 0 {
                if let Some(a) = size(mode).and_then(|n| mem.alloc_effect(n)) {
                    self.area[c] = a;
                } else {
                    self.mode[c] = -1;
                    self.fixed[c] = -1;
                }
            }
        }
    }

    /// `snd_SetReverbType(cores, type)`.
    pub(crate) fn set_type(
        &mut self,
        mem: &mut SpuMemory,
        cores: u32,
        mode: i32,
        out: &mut Vec<Op>,
    ) {
        for c in 0..2 {
            if cores & (1 << c) != 0 {
                self.change(mem, c, mode, out);
            }
        }
    }

    /// `snd_SetReverbDepth(cores, left, right)`: the return volume, now
    /// unless a work area is being cleared.
    pub(crate) fn set_depth(&mut self, cores: u32, l: u16, r: u16, out: &mut Vec<Op>) {
        for c in 0..2 {
            if cores & (1 << c) != 0 {
                self.depth[c] = [l, r];
                if !self.clearing[c] {
                    let (_, vols) = blocks(c);
                    out.push(Op::Write(vols + 4, l));
                    out.push(Op::Write(vols + 6, r));
                }
            }
        }
    }

    /// The type of core `c` changes.
    fn change(&mut self, mem: &mut SpuMemory, c: usize, mode: i32, out: &mut Vec<Op>) {
        let Some(bytes) = size(mode) else { return };
        let (_, vols) = blocks(c);
        if mode != self.mode[c] {
            if self.fixed[c] > 0 && size(self.fixed[c]).is_some_and(|f| f < bytes) {
                tracing::warn!(target: "snd989", "reverb type {mode} does not fit core {c}'s reserved work area");
                return;
            }
            if self.mode[c] > 0 {
                out.push(Op::Enable(c, false));
                out.push(Op::Write(vols + 4, 0));
                out.push(Op::Write(vols + 6, 0));
            }
            if self.fixed[c] <= 0 {
                if self.mode[c] > 0
                    && let Some(n) = size(self.mode[c])
                {
                    mem.release(self.area[c], n);
                }
                if mode != 0 {
                    let Some(a) = mem.alloc_effect(bytes) else {
                        self.area[c] = 0;
                        self.mode[c] = -1;
                        tracing::warn!(target: "snd989", "no sound memory for reverb type {mode}");
                        return;
                    };
                    self.area[c] = a;
                }
            }
            if mode != 0 {
                // EEA: the 128 KiB page the work area ends in.
                self.eea[c] = (self.area[c] + bytes - 1) >> 17;
                out.push(Op::Write(blocks(c).0 + 0x33C, self.eea[c] as u16));
            }
        }
        self.load_preset(c, mode, out);
        if mode != 0 && mode != self.mode[c] {
            out.push(Op::Clear(self.area[c], bytes));
            self.clearing[c] = true;
            self.pending[c] = PENDING_TICKS;
        }
        self.mode[c] = mode;
    }

    /// libsd's `sceSdSetEffectAttr`: the preset into core `c`'s registers.
    fn load_preset(&self, c: usize, mode: i32, out: &mut Vec<Op>) {
        let Some(mut p) = usize::try_from(mode)
            .ok()
            .and_then(|m| self.presets.regs.get(m))
            .copied()
        else {
            return;
        };
        if mode == 7 || mode == 8 {
            // Echo and delay: libsd's presets hold the longest delay (127
            // steps of 32 units) and the strongest feedback; the driver
            // asks for delay and feedback 0, as Sly 1's libsd writes them.
            let (delay, feedback) = (0u16, 0u16);
            for (k, steps) in [(10, 2), (11, 1), (12, 1), (16, 1), (26, 1), (27, 1)] {
                p[k] = p[k]
                    .wrapping_sub(steps * 0xFE0)
                    .wrapping_add(steps * 32 * delay);
            }
            p[7] = (feedback << 8).wrapping_neg();
        }
        let (regs, vols) = blocks(c);
        out.push(Op::Write(vols + 4, 0));
        out.push(Op::Write(vols + 6, 0));
        let address = |out: &mut Vec<Op>, at: u32, units: u16| {
            let v = u32::from(units) * 4;
            out.push(Op::Write(at, (v >> 16) as u16));
            out.push(Op::Write(at + 2, (v & 0xFFFF) as u16));
        };
        address(out, regs + 0x2E4, p[0]);
        address(out, regs + 0x2E8, p[1]);
        for k in 0..8 {
            out.push(Op::Write(vols + 0x14 + 2 * k as u32, p[2 + k]));
        }
        for k in 0..20 {
            address(out, regs + 0x2EC + 4 * k as u32, p[10 + k]);
        }
        out.push(Op::Write(vols + 0x24, p[30]));
        out.push(Op::Write(vols + 0x26, p[31]));
        let units = usize::try_from(mode)
            .ok()
            .and_then(|m| UNITS.get(m))
            .copied()
            .unwrap_or(2);
        let esa = ((self.eea[c] << 16) | 0xFFFF) + 1 - units * 4;
        out.push(Op::Write(regs + 0x2E0, (esa >> 16) as u16));
        out.push(Op::Write(regs + 0x2E2, (esa & 0xFFFF) as u16));
    }

    /// A driver tick: a cleared work area's effect goes on.
    pub(crate) fn tick(&mut self, out: &mut Vec<Op>) {
        for c in 0..2 {
            if self.pending[c] != 0 {
                self.pending[c] -= 1;
                if self.pending[c] == 0 {
                    let (_, vols) = blocks(c);
                    out.push(Op::Write(vols + 4, self.depth[c][0]));
                    out.push(Op::Write(vols + 6, self.depth[c][1]));
                    out.push(Op::Enable(c, true));
                    self.clearing[c] = false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Type 0 on core 1 writes what Sly 1's driver writes: EVOL off, the
    /// sizes, the volumes, the addresses (only the "same" and "diff"
    /// destinations 4), the input volumes, then ESA 0xF_FFF8.
    #[test]
    fn type_off_on_core_1() {
        let mut r = Reverb::new(Presets::off_only());
        let mut mem = SpuMemory::default();
        let mut out = Vec::new();
        r.reserve(&mut mem, 2, 4, &mut out);
        assert_eq!(out, Vec::new());
        r.set_type(&mut mem, 2, 0, &mut out);
        let writes: Vec<(u32, u16)> = out
            .iter()
            .filter_map(|o| match *o {
                Op::Write(a, v) => Some((a & 0x7FF, v)),
                _ => None,
            })
            .collect();
        assert_eq!(writes.len(), 2 + 4 + 8 + 40 + 2 + 2);
        assert_eq!(writes[0], (0x78C, 0));
        assert_eq!(writes[14], (0x6EC, 0));
        assert_eq!(writes[15], (0x6EE, 4));
        assert_eq!(writes[17], (0x6F2, 4));
        assert_eq!(writes[31], (0x70E, 4));
        assert_eq!(writes[writes.len() - 2], (0x6E0, 0xF));
        assert_eq!(writes[writes.len() - 1], (0x6E2, 0xFFF8));
        let nonzero = writes.iter().filter(|w| w.1 != 0).count();
        assert_eq!(nonzero, 6);
    }

    /// A real type takes a work area from the top of sound memory, clears
    /// it, and goes on ten ticks later at the depth.
    #[test]
    fn a_type_clears_then_goes_on() {
        let mut r = Reverb::new(Presets::off_only());
        let mut mem = SpuMemory::default();
        let mut out = Vec::new();
        r.set_depth(1, 0x3000, 0x3000, &mut out);
        r.set_type(&mut mem, 1, 1, &mut out);
        assert!(out.contains(&Op::Clear(0x20_0000 - 0x26C0, 0x26C0)));
        out.clear();
        for _ in 0..9 {
            r.tick(&mut out);
        }
        assert_eq!(out, Vec::new());
        r.tick(&mut out);
        assert_eq!(out.last(), Some(&Op::Enable(0, true)));
    }
}
