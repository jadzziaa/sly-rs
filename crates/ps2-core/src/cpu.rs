//! The EE core's architectural state.

/// The register file and control state of the R5900 core, its FPU (COP1)
/// and VU0 as COP2.
#[derive(Clone)]
pub struct Cpu {
    /// General-purpose registers, 128 bits wide (MMI uses all of them;
    /// ordinary instructions use the low 64).
    pub gpr: [u128; 32],
    /// Address of the next instruction to run.
    pub pc: u32,
    /// HI: HI0 in bits 63..0, HI1 (the second pipeline) in 127..64.
    pub hi: u128,
    /// LO: LO0 in bits 63..0, LO1 in 127..64.
    pub lo: u128,
    /// The shift-amount register of `qfsrv` (in bytes × 8: a bit count).
    pub sa: u32,
    /// FPU registers, as raw bits.
    pub fpr: [u32; 32],
    /// The FPU accumulator (`adda.s`, `madd.s`, ...).
    pub fpu_acc: u32,
    /// FPU control register 31: condition bit 23, flags.
    pub fcr31: u32,
    /// COP0 registers.
    pub cop0: [u32; 32],
    /// VU0, which COP2 macro instructions drive.
    pub vu0: Vu0,
    /// Instructions run so far: the machine's clock (one instruction per EE
    /// cycle).
    pub cycles: u64,
    /// CPCOND0, the condition `bc0f`/`bc0t` test: true when the DMA channels
    /// the DMAC's PCR selects are idle. The machine keeps it current.
    pub cpcond0: bool,
}

/// COP0 register numbers used by the semantics.
pub mod cop0 {
    /// Count: increments every cycle.
    pub const COUNT: usize = 9;
    /// Status: interrupt enables and mode bits.
    pub const STATUS: usize = 12;
    /// Cause.
    pub const CAUSE: usize = 13;
    /// Exception PC.
    pub const EPC: usize = 14;
    /// Processor ID.
    pub const PRID: usize = 15;
    /// Status bit EIE: the master interrupt enable `ei`/`di` toggle.
    pub const STATUS_EIE: u32 = 1 << 16;
}

/// VU0's registers as COP2 sees them. Vector registers hold raw bits;
/// `vf[0]` reads as (0, 0, 0, 1.0) and `vi[0]` as 0.
#[derive(Clone, Default)]
pub struct Vu0 {
    /// Floating-point vector registers: x, y, z, w.
    pub vf: [[u32; 4]; 32],
    /// Integer registers (16 bits used).
    pub vi: [u32; 16],
    /// The accumulator.
    pub acc: [u32; 4],
    /// Q: the division unit's result.
    pub q: u32,
    /// P: the EFU's result (VU1 only; kept for completeness).
    pub p: u32,
    /// I: the immediate register.
    pub i: u32,
    /// R: the random-number register.
    pub r: u32,
    /// Status flags (control register 16).
    pub status: u32,
    /// MAC flags (control register 17).
    pub mac: u32,
    /// Clipping flags (control register 18).
    pub clip: u32,
    /// CMSAR0: start address of a VU0 micro program (register 27).
    pub cmsar0: u32,
    /// FBRST (register 28).
    pub fbrst: u32,
    /// TPC (register 26).
    pub tpc: u32,
}

impl Default for Cpu {
    fn default() -> Self {
        let mut cop0 = [0; 32];
        cop0[cop0::PRID] = 0x2E20;
        let mut vu0 = Vu0::default();
        vu0.vf[0] = [0, 0, 0, 0x3F80_0000];
        Self {
            gpr: [0; 32],
            pc: 0,
            hi: 0,
            lo: 0,
            sa: 0,
            fpr: [0; 32],
            fpu_acc: 0,
            fcr31: 0,
            cop0,
            vu0,
            cycles: 0,
            cpcond0: true,
        }
    }
}

impl Cpu {
    /// The low 64 bits of a register.
    #[inline]
    #[must_use]
    pub const fn r64(&self, r: u8) -> u64 {
        self.gpr[r as usize] as u64
    }

    /// The low 32 bits of a register.
    #[inline]
    #[must_use]
    pub const fn r32(&self, r: u8) -> u32 {
        self.gpr[r as usize] as u32
    }

    /// Writes the low 64 bits of a register, keeping the upper 64
    /// (`$zero` stays zero).
    #[inline]
    pub fn set64(&mut self, r: u8, value: u64) {
        if r != 0 {
            let reg = &mut self.gpr[r as usize];
            *reg = (*reg & !u128::from(u64::MAX)) | u128::from(value);
        }
    }

    /// Writes a 32-bit result sign-extended to 64 bits.
    #[inline]
    pub fn set32(&mut self, r: u8, value: u32) {
        self.set64(r, i64::from(value.cast_signed()).cast_unsigned());
    }

    /// Writes all 128 bits of a register.
    #[inline]
    pub fn set128(&mut self, r: u8, value: u128) {
        if r != 0 {
            self.gpr[r as usize] = value;
        }
    }
}
