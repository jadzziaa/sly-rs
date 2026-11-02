//! PS2 single-precision arithmetic, shared by the FPU and the VUs.
//!
//! The PS2's float units are not IEEE 754:
//! - no infinities or NaNs: exponent 255 is an ordinary (huge) value, which
//!   this module, like PCSX2, treats as ±`FLT_MAX`;
//! - no denormals: they read as zero, and results that would be one become
//!   zero (underflow);
//! - results round toward zero, except division and square root, which
//!   round to nearest;
//! - add and subtract have no guard bits: before adding, the bits of the
//!   smaller operand that fall below the larger operand's last place are
//!   dropped;
//! - overflowing results become ±`FLT_MAX`.
//!
//! Every operation computes in `f64`, where these inputs give the exact
//! result, then rounds once.

/// The largest PS2 single, as bits.
pub const MAX: u32 = 0x7F7F_FFFF;

/// Outcome flags of an operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    /// The result overflowed and was clamped.
    pub overflow: bool,
    /// The result underflowed to zero.
    pub underflow: bool,
}

/// The value of a PS2 single as an `f64`.
#[inline]
#[must_use]
pub fn value(bits: u32) -> f64 {
    let exponent = (bits >> 23) & 0xFF;
    if exponent == 0 {
        return if bits >> 31 == 0 { 0.0 } else { -0.0 };
    }
    let clamped = if exponent == 0xFF {
        (bits & 0x8000_0000) | MAX
    } else {
        bits
    };
    f64::from(f32::from_bits(clamped))
}

/// Rounds `x` toward zero to a PS2 single, clamping overflow and flushing
/// underflow.
#[inline]
#[must_use]
pub fn chop(x: f64) -> (u32, Flags) {
    finish(x, true)
}

/// Rounds `x` to the nearest PS2 single, clamping overflow and flushing
/// underflow.
#[inline]
#[must_use]
pub fn nearest(x: f64) -> (u32, Flags) {
    finish(x, false)
}

fn finish(x: f64, toward_zero: bool) -> (u32, Flags) {
    let sign = if x.is_sign_negative() { 0x8000_0000 } else { 0 };
    let magnitude = x.abs();
    if magnitude == 0.0 {
        return (sign, Flags::default());
    }
    if magnitude > f64::from(f32::MAX) {
        return (
            sign | MAX,
            Flags {
                overflow: true,
                underflow: false,
            },
        );
    }
    let mut rounded = magnitude as f32;
    if toward_zero && f64::from(rounded) > magnitude {
        rounded = f32::from_bits(rounded.to_bits() - 1);
    }
    let bits = rounded.to_bits();
    if bits >= 0x7F80_0000 {
        return (
            sign | MAX,
            Flags {
                overflow: true,
                underflow: false,
            },
        );
    }
    if bits < 0x0080_0000 {
        return (
            sign,
            Flags {
                overflow: false,
                underflow: true,
            },
        );
    }
    (sign | bits, Flags::default())
}

/// Drops the bits of `small` below the last place of an operand
/// `difference` binary orders of magnitude larger.
fn truncate_smaller(small: u32, difference: u32) -> u32 {
    if difference >= 24 {
        small & 0x8000_0000
    } else if difference > 0 {
        small & !((1 << difference) - 1)
    } else {
        small
    }
}

/// The operands of an add with the smaller one truncated as the hardware's
/// adder does.
fn aligned(a: u32, b: u32) -> (u32, u32) {
    let (ea, eb) = ((a >> 23) & 0xFF, (b >> 23) & 0xFF);
    if ea == 0 || eb == 0 {
        return (a, b);
    }
    if ea >= eb {
        (a, truncate_smaller(b, ea - eb))
    } else {
        (truncate_smaller(a, eb - ea), b)
    }
}

/// `a + b`.
#[must_use]
pub fn add(a: u32, b: u32) -> (u32, Flags) {
    let (a, b) = aligned(a, b);
    chop(value(a) + value(b))
}

/// `a - b`.
#[must_use]
pub fn sub(a: u32, b: u32) -> (u32, Flags) {
    add(a, b ^ 0x8000_0000)
}

/// `a * b`.
#[must_use]
pub fn mul(a: u32, b: u32) -> (u32, Flags) {
    chop(value(a) * value(b))
}

/// `acc + a * b`, rounded after each step.
#[must_use]
pub fn madd(acc: u32, a: u32, b: u32) -> (u32, Flags) {
    let (product, f1) = mul(a, b);
    let (sum, f2) = add(acc, product);
    (
        sum,
        Flags {
            overflow: f1.overflow || f2.overflow,
            underflow: f2.underflow,
        },
    )
}

/// `acc - a * b`, rounded after each step.
#[must_use]
pub fn msub(acc: u32, a: u32, b: u32) -> (u32, Flags) {
    let (product, f1) = mul(a, b);
    let (difference, f2) = sub(acc, product);
    (
        difference,
        Flags {
            overflow: f1.overflow || f2.overflow,
            underflow: f2.underflow,
        },
    )
}

/// Outcome of a division-unit operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DivFlags {
    /// Invalid: 0/0, or the root of a negative number.
    pub invalid: bool,
    /// Division by zero.
    pub divide_by_zero: bool,
}

/// `a / b`. Division by zero gives ±`FLT_MAX` (0/0 too, flagged invalid).
#[must_use]
pub fn div(a: u32, b: u32) -> (u32, DivFlags) {
    let sign = (a ^ b) & 0x8000_0000;
    if value(b) == 0.0 {
        let invalid = value(a) == 0.0;
        return (
            sign | MAX,
            DivFlags {
                invalid,
                divide_by_zero: !invalid,
            },
        );
    }
    (nearest(value(a) / value(b)).0, DivFlags::default())
}

/// `sqrt(|a|)`; a negative operand is flagged invalid.
#[must_use]
pub fn sqrt(a: u32) -> (u32, DivFlags) {
    let invalid = value(a) < 0.0;
    (
        nearest(value(a).abs().sqrt()).0,
        DivFlags {
            invalid,
            divide_by_zero: false,
        },
    )
}

/// `a / sqrt(|b|)`.
#[must_use]
pub fn rsqrt(a: u32, b: u32) -> (u32, DivFlags) {
    let root = value(b).abs().sqrt();
    let invalid = value(b) < 0.0;
    if root == 0.0 {
        return (
            (a & 0x8000_0000) | MAX,
            DivFlags {
                invalid,
                divide_by_zero: true,
            },
        );
    }
    (
        nearest(value(a) / root).0,
        DivFlags {
            invalid,
            divide_by_zero: false,
        },
    )
}

/// The larger of two values.
#[must_use]
pub fn max(a: u32, b: u32) -> u32 {
    if value(a) >= value(b) { a } else { b }
}

/// The smaller of two values.
#[must_use]
pub fn min(a: u32, b: u32) -> u32 {
    if value(a) <= value(b) { a } else { b }
}

/// Float to integer, truncating toward zero and saturating.
#[must_use]
pub fn to_int(a: u32) -> u32 {
    let x = value(a).trunc();
    if x >= 2_147_483_647.0 {
        0x7FFF_FFFF
    } else if x <= -2_147_483_648.0 {
        0x8000_0000
    } else {
        (x as i32).cast_unsigned()
    }
}

/// Integer to float, rounding toward zero.
#[must_use]
pub fn from_int(a: u32) -> u32 {
    chop(f64::from(a.cast_signed())).0
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: u32 = 0x3F80_0000;
    const TWO: u32 = 0x4000_0000;

    #[test]
    fn ordinary_arithmetic() {
        assert_eq!(add(ONE, ONE).0, TWO);
        assert_eq!(sub(ONE, ONE).0, 0);
        assert_eq!(mul(TWO, TWO).0, 0x4080_0000);
        assert_eq!(div(ONE, TWO).0, 0x3F00_0000);
        assert_eq!(sqrt(0x4080_0000).0, TWO);
    }

    #[test]
    fn no_infinity_no_nan() {
        // Exponent 255 reads as FLT_MAX; overflow clamps.
        assert_eq!(value(0x7F80_0000), f64::from(f32::MAX));
        assert_eq!(
            mul(MAX, TWO),
            (
                MAX,
                Flags {
                    overflow: true,
                    underflow: false
                }
            )
        );
        assert_eq!(div(ONE, 0).0, MAX);
        assert_eq!(div(0x8000_0000 | ONE, 0).0, 0x8000_0000 | MAX);
    }

    #[test]
    fn denormals_are_zero() {
        assert_eq!(value(0x0000_0001), 0.0);
        let tiny = 0x0080_0000; // the smallest normal
        assert_eq!(
            mul(tiny, 0x3F00_0000),
            (
                0,
                Flags {
                    overflow: false,
                    underflow: true
                }
            )
        );
    }

    #[test]
    fn rounding_is_toward_zero() {
        // 1 + 2^-24 rounds to nearest-even 1.0 in IEEE; 1/3 * 3 shows the chop.
        let third = div(ONE, 0x4040_0000).0; // nearest: 0x3EAAAAAB
        assert_eq!(third, 0x3EAA_AAAB);
        assert_eq!(mul(third, 0x4040_0000).0, ONE);
        // 0.1f * 3 = 0.30000000447 exactly: nearest is 0x3E99999A, chop 0x3E999999.
        assert_eq!(mul(0x3DCC_CCCD, 0x4040_0000).0, 0x3E99_9999);
        assert_eq!(nearest(0.300_000_004_470_348_4).0, 0x3E99_999A);
    }

    #[test]
    fn no_guard_bits_in_addition() {
        // 1.0 - 2^-30: the subtrahend is shifted out entirely, so the result
        // stays 1.0 (IEEE round-toward-zero would give 0x3F7FFFFF).
        assert_eq!(sub(ONE, 0x3080_0000).0, ONE);
    }

    #[test]
    fn conversions_saturate_and_truncate() {
        assert_eq!(to_int(0x4049_0FDB), 3); // pi
        assert_eq!(to_int(0xC049_0FDB), (-3_i32).cast_unsigned());
        assert_eq!(to_int(0x5F00_0000), 0x7FFF_FFFF);
        assert_eq!(from_int(16_777_217), 0x4B80_0000); // 2^24 + 1 chops to 2^24
    }
}
