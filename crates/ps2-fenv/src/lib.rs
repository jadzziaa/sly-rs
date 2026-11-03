//! The host's floating-point environment for VU code.
//!
//! The VUs follow PCSX2's float model: results round toward zero,
//! denormal operands read as zero and denormal results become zero. On
//! x86-64 that is SSE under MXCSR's round-toward-zero, denormals-are-zero
//! and flush-to-zero modes, so VU arithmetic can be one vector instruction
//! per operation. [`with_vu_rounding`] runs code in that environment and
//! puts the caller's back afterwards, even if the code panics.
//!
//! Only code written for it may run inside: ordinary Rust floating-point
//! code there rounds differently. The VUs' exact `f64` forms
//! (`ps2_core::float::vu`) give the same results either way.

/// Whether this host can run VU code in the VU environment (x86-64);
/// elsewhere [`with_vu_rounding`] changes nothing, and the VUs must use
/// their exact forms.
pub const AVAILABLE: bool = cfg!(target_arch = "x86_64");

/// MXCSR: exceptions masked (0x1F80), denormals are zero (0x40),
/// round toward zero (0x6000), flush to zero (0x8000).
#[cfg(target_arch = "x86_64")]
const VU: u32 = 0x1F80 | 0x0040 | 0x6000 | 0x8000;

#[cfg(target_arch = "x86_64")]
fn read() -> u32 {
    let mut value = 0u32;
    // SAFETY: `stmxcsr` stores the 32-bit MXCSR register at the address
    // given, which points at `value`, a live, aligned, writable u32.
    #[allow(unsafe_code)]
    unsafe {
        core::arch::asm!(
            "stmxcsr [{}]",
            in(reg) &raw mut value,
            options(nostack, preserves_flags)
        );
    }
    value
}

#[cfg(target_arch = "x86_64")]
fn write(value: u32) {
    // SAFETY: `ldmxcsr` loads MXCSR from the address given, which points
    // at `value`, a live, aligned u32. The modes set (rounding, flushing,
    // masked exceptions) cannot fault; reserved bits are never set.
    #[allow(unsafe_code)]
    unsafe {
        core::arch::asm!(
            "ldmxcsr [{}]",
            in(reg) &raw const value,
            options(nostack, preserves_flags, readonly)
        );
    }
}

/// Puts the caller's MXCSR back when dropped (also when unwinding).
#[cfg(target_arch = "x86_64")]
struct Restore(u32);

#[cfg(target_arch = "x86_64")]
impl Drop for Restore {
    fn drop(&mut self) {
        write(self.0);
    }
}

/// Runs `f` with the host's floating point rounding toward zero and
/// flushing denormals, then restores the caller's environment. Not inlined,
/// so the compiler cannot move floating-point work across the switch.
#[inline(never)]
pub fn with_vu_rounding<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(target_arch = "x86_64")]
    {
        let _restore = Restore(read());
        write(VU);
        f()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        f()
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn rounds_toward_zero_inside_and_restores_outside() {
        let (a, b) = (std::hint::black_box(1.0f32), std::hint::black_box(3.0f32));
        let before = a / b;
        let inside = with_vu_rounding(|| std::hint::black_box(a) / std::hint::black_box(b));
        // 1/3 rounds up to nearest, down toward zero.
        assert_eq!(inside.to_bits() + 1, before.to_bits());
        assert_eq!((a / b).to_bits(), before.to_bits());
        // Denormal results flush to zero inside.
        let tiny = std::hint::black_box(f32::MIN_POSITIVE);
        let half = with_vu_rounding(|| std::hint::black_box(tiny) * std::hint::black_box(0.5f32));
        assert_eq!(half.to_bits(), 0);
        assert_ne!((tiny * 0.5).to_bits(), 0);
    }

    #[test]
    fn a_panic_inside_restores_the_environment() {
        let before = read();
        let result = std::panic::catch_unwind(|| with_vu_rounding(|| panic!("inside")));
        assert!(result.is_err());
        assert_eq!(read(), before);
    }
}
