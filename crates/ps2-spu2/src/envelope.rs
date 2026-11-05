//! Envelopes and volume slides, as PCSX2's `pcsx2/SPU2/ADSR.cpp` computes
//! them, one step per output sample.

/// The envelope's phases (PCSX2's `V_ADSR::PHASE_*`).
pub(crate) const PHASE_STOPPED: u8 = 0;
pub(crate) const PHASE_ATTACK: u8 = 1;
pub(crate) const PHASE_DECAY: u8 = 2;
pub(crate) const PHASE_SUSTAIN: u8 = 3;
pub(crate) const PHASE_RELEASE: u8 = 4;

const ADSR_MAX_VOL: i32 = 0x7FFF;

/// One phase's rate and target, worked out from ADSR1/ADSR2 when they are
/// written (PCSX2's `CachedADSR`).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CachedPhase {
    decr: bool,
    exp: bool,
    shift: u8,
    step: i8,
    target: i32,
}

/// A voice's envelope (PCSX2's `V_ADSR`).
#[derive(Clone, Debug, Default)]
pub(crate) struct Adsr {
    /// ADSR1: sustain level, decay rate, attack rate and mode.
    pub(crate) reg1: u16,
    /// ADSR2: release rate and mode, sustain rate, direction and mode.
    pub(crate) reg2: u16,
    /// Indexed by phase. All zero until ADSR1 or ADSR2 is first written, as
    /// in PCSX2 (it only fills them in `UpdateCache`).
    cached: [CachedPhase; 5],
    counter: u32,
    /// The level, 0 to 0x7FFF (ENVX). A write to ENVX can put up to 0xFFFF
    /// here, as in PCSX2.
    pub(crate) value: i32,
    pub(crate) phase: u8,
}

impl Adsr {
    /// The two registers as PCSX2's bitfields see them (ADSR1 low).
    fn reg32(&self) -> u32 {
        u32::from(self.reg1) | (u32::from(self.reg2) << 16)
    }

    /// Works the phases' rates out again after ADSR1 or ADSR2 changed.
    pub(crate) fn update_cache(&mut self) {
        let r = self.reg32();
        let sustain_level = r & 0xF;
        let decay_shift = (r >> 4) & 0xF;
        let attack_step = (r >> 8) & 0x3;
        let attack_shift = (r >> 10) & 0x1F;
        let attack_mode = (r >> 15) & 1 != 0;
        let release_shift = (r >> 16) & 0x1F;
        let release_mode = (r >> 21) & 1 != 0;
        let sustain_step = (r >> 22) & 0x3;
        let sustain_shift = (r >> 24) & 0x1F;
        let sustain_dir = (r >> 30) & 1 != 0;
        let sustain_mode = (r >> 31) & 1 != 0;

        self.cached[usize::from(PHASE_ATTACK)] = CachedPhase {
            decr: false,
            exp: attack_mode,
            shift: attack_shift as u8,
            step: 7 - attack_step as i8,
            target: ADSR_MAX_VOL,
        };
        self.cached[usize::from(PHASE_DECAY)] = CachedPhase {
            decr: true,
            exp: true,
            shift: decay_shift as u8,
            step: -8,
            target: ((sustain_level + 1) << 11) as i32,
        };
        let step = 7 - sustain_step as i8;
        self.cached[usize::from(PHASE_SUSTAIN)] = CachedPhase {
            decr: sustain_dir,
            exp: sustain_mode,
            shift: sustain_shift as u8,
            step: if sustain_dir { !step } else { step },
            target: 0,
        };
        self.cached[usize::from(PHASE_RELEASE)] = CachedPhase {
            decr: true,
            exp: release_mode,
            shift: release_shift as u8,
            step: -8,
            target: 0,
        };
    }

    /// One sample's step; false once the envelope has run out (the voice
    /// then stops). Not called while stopped.
    pub(crate) fn calculate(&mut self) -> bool {
        let p = self.cached[usize::from(self.phase.min(PHASE_RELEASE))];

        // "maybe not correct for the "infinite" settings" (PCSX2).
        let mut counter_inc: u32 = 0x8000 >> (i32::from(p.shift) - 11).max(0);
        let mut level_inc: i32 = i32::from(p.step) << (11 - i32::from(p.shift)).max(0);

        if p.exp {
            if !p.decr && self.value > 0x6000 {
                counter_inc >>= 2;
            }
            if p.decr {
                level_inc = i32::from((level_inc.wrapping_mul(self.value) >> 15) as i16);
            }
        }

        counter_inc = counter_inc.max(1);
        self.counter = self.counter.wrapping_add(counter_inc);

        if self.counter >= 0x8000 {
            self.counter = 0;
            self.value = self
                .value
                .wrapping_add(level_inc)
                .clamp(0, i32::from(i16::MAX));
        }

        // Stay in sustain until key off or silence.
        if self.phase == PHASE_SUSTAIN {
            return self.value != 0;
        }

        if (!p.decr && self.value >= p.target) || (p.decr && self.value <= p.target) {
            self.phase += 1;
        }

        self.phase <= PHASE_RELEASE
    }

    /// Key on.
    pub(crate) fn attack(&mut self) {
        self.phase = PHASE_ATTACK;
        self.counter = 0;
        self.value = 0;
    }

    /// Key off.
    pub(crate) fn release(&mut self) {
        if self.phase != PHASE_STOPPED {
            self.phase = PHASE_RELEASE;
            self.counter = 0;
        }
    }
}

/// A volume that can slide (PCSX2's `V_VolumeSlide`): voices' VOLL/VOLR
/// and the cores' MVOLL/MVOLR.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct VolumeSlide {
    /// The register as written.
    pub(crate) reg: u16,
    counter: u32,
    /// The current volume (VOLX, MVOLX).
    pub(crate) value: i32,
}

impl VolumeSlide {
    fn step(self) -> u16 {
        self.reg & 3
    }
    fn shift(self) -> u16 {
        (self.reg >> 2) & 0x1F
    }
    fn phase(self) -> bool {
        self.reg & 0x1000 != 0
    }
    fn decr(self) -> bool {
        self.reg & 0x2000 != 0
    }
    fn exp(self) -> bool {
        self.reg & 0x4000 != 0
    }
    fn enable(self) -> bool {
        self.reg & 0x8000 != 0
    }

    /// A register write: a fixed volume takes effect at once, a slide
    /// starts from the current volume.
    pub(crate) fn reg_set(&mut self, src: u16) {
        self.reg = src;
        if !self.enable() {
            self.value = i32::from((src << 1) as i16);
        }
    }

    /// One sample's step of a slide.
    pub(crate) fn update(&mut self) {
        if !self.enable() {
            return;
        }

        let mut step_size: i32 = 7 - i32::from(self.step());
        if self.decr() {
            step_size = !step_size;
        }

        let shift = i32::from(self.shift());
        let mut counter_inc: u32 = 0x8000 >> (shift - 11).max(0);
        let mut level_inc: i32 = step_size << (11 - shift).max(0);

        if self.exp() {
            if !self.decr() && self.value > 0x6000 {
                counter_inc >>= 2;
            }
            if self.decr() {
                level_inc = i32::from((level_inc.wrapping_mul(self.value) >> 15) as i16);
            }
        }

        // Allow counter_inc to be zero only when all bits of the rate field
        // are set (PCSX2's test, kept as written).
        if self.step() != 3 && self.shift() != 0x1F {
            counter_inc = counter_inc.max(1);
        }
        self.counter = self.counter.wrapping_add(counter_inc);

        // A negative phase "increases" towards -0x8000 or "decreases"
        // towards 0, except in exponential decrease.
        if !(self.exp() && self.decr()) && self.phase() {
            level_inc = level_inc.wrapping_neg();
        }

        if self.counter >= 0x8000 {
            self.counter = 0;
            let (min, max) = (i32::from(i16::MIN), i32::from(i16::MAX));
            let (low, high) = if !self.decr() {
                (min, max)
            } else if self.exp() {
                (0, max)
            } else if self.phase() {
                (min, 0)
            } else {
                (0, max)
            };
            self.value = self.value.wrapping_add(level_inc).clamp(low, high);
        }
    }
}
