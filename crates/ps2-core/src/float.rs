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
//!
//! The VUs follow PCSX2's model instead ([`vu`], [`x4`]): add and subtract
//! round the exact result toward zero, with no guard-bit truncation, and
//! only the zero and sign flags are kept (PCSX2 sets no overflow or
//! underflow MAC flags, and Sly reads none). It is what SSE computes under
//! round-toward-zero with denormals flushed, which lets translated VU code
//! run on the host's single-precision vector instructions.

/// The VUs' arithmetic, one lane: PCSX2's model (see the module's notes).
pub mod vu {
    use super::{MAX, chop, value};

    /// `x + y` (the doubles of two singles) rounded toward zero to a
    /// single, from their sum to the nearest double and its rounding error
    /// (Knuth's `TwoSum`, exact): the sum chopped, one step further toward
    /// zero when it landed on a single and the true sum lies just inside
    /// it. Exact whatever the host's rounding mode, and what SSE gives
    /// under round-toward-zero with denormals flushed.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn sum(x: f64, y: f64) -> u32 {
        let s = x + y;
        let back = s - x;
        let error = (x - (s - back)) + (y - back);
        let bits = chop(s).0;
        let sign = bits & 0x8000_0000;
        let magnitude = bits & 0x7FFF_FFFF;
        // On a single: no bits below a single's last place.
        let on_single = s.to_bits().trailing_zeros() >= 29;
        let inward = (error != 0.0) & (error.is_sign_negative() != s.is_sign_negative());
        let in_range = (magnitude >= 0x0080_0000) & (s.abs() <= f64::from(f32::from_bits(MAX)));
        let magnitude = magnitude - u32::from(on_single & inward & in_range);
        sign | if magnitude < 0x0080_0000 {
            0
        } else {
            magnitude
        }
    }

    /// `a + b`.
    #[must_use]
    pub fn add(a: u32, b: u32) -> u32 {
        sum(value(a), value(b))
    }

    /// `a - b`.
    #[must_use]
    pub fn sub(a: u32, b: u32) -> u32 {
        add(a, b ^ 0x8000_0000)
    }

    /// `a * b` (the product of two singles is exact in a double).
    #[must_use]
    pub fn mul(a: u32, b: u32) -> u32 {
        chop(value(a) * value(b)).0
    }

    /// `acc + a * b`, the product rounded first.
    #[must_use]
    pub fn madd(acc: u32, a: u32, b: u32) -> u32 {
        add(acc, mul(a, b))
    }

    /// `acc - a * b`, the product rounded first.
    #[must_use]
    pub fn msub(acc: u32, a: u32, b: u32) -> u32 {
        sub(acc, mul(a, b))
    }
}

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
#[allow(clippy::inline_always, reason = "the VUs' hot path")]
#[inline(always)]
#[must_use]
pub fn value(bits: u32) -> f64 {
    // Without branches (selects), as everything on the VUs' hot path:
    // exponent 0 reads as a signed zero, exponent 255 as ±`MAX`.
    let exponent = (bits >> 23) & 0xFF;
    let sign = bits & 0x8000_0000;
    let bits = if exponent == 0 { sign } else { bits };
    let bits = if exponent == 0xFF { sign | MAX } else { bits };
    f64::from(f32::from_bits(bits))
}

/// Rounds `x` toward zero to a PS2 single, clamping overflow and flushing
/// underflow.
#[allow(clippy::inline_always, reason = "the VUs' hot path")]
#[inline(always)]
#[must_use]
pub fn chop(x: f64) -> (u32, Flags) {
    // As `finish(x, true)`, from the bits and without branches: a value in
    // single precision's normal range keeps its exponent and loses the low
    // 29 bits of its mantissa. Magnitudes compare as integers, NaNs above
    // everything (they overflow, as in `finish`).
    let bits = x.to_bits();
    let sign = ((bits >> 32) as u32) & 0x8000_0000;
    let magnitude = bits & !(1 << 63);
    // f32::MAX and 2^-126, the smallest normal single, as doubles.
    let overflow = magnitude > 0x47EF_FFFF_E000_0000;
    let tiny = magnitude < 0x3810_0000_0000_0000;
    let underflow = tiny && magnitude != 0;
    let exponent = (magnitude >> 52) as u32;
    let mantissa = (magnitude >> 29) as u32 & 0x007F_FFFF;
    let normal = sign | (exponent.wrapping_sub(896) << 23) | mantissa;
    let result = if tiny { sign } else { normal };
    (
        if overflow { sign | MAX } else { result },
        Flags {
            overflow,
            underflow,
        },
    )
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

/// The operands of an add with the smaller one truncated as the hardware's
/// adder does: the bits below the larger operand's last place are dropped,
/// all but the sign 24 orders of magnitude down or more. Without branches.
#[allow(clippy::inline_always, reason = "the VUs' hot path")]
#[inline(always)]
fn aligned(a: u32, b: u32) -> (u32, u32) {
    let (ea, eb) = ((a >> 23) & 0xFF, (b >> 23) & 0xFF);
    let difference = ea.abs_diff(eb);
    let keep = if difference >= 24 {
        0x8000_0000
    } else {
        !((1u32 << difference.min(31)) - 1)
    };
    let keep = if ea == 0 || eb == 0 { u32::MAX } else { keep };
    let (keep_a, keep_b) = if ea >= eb {
        (u32::MAX, keep)
    } else {
        (keep, u32::MAX)
    };
    (a & keep_a, b & keep_b)
}

/// `a + b`.
#[allow(clippy::inline_always, reason = "the VUs' hot path")]
#[inline(always)]
#[must_use]
pub fn add(a: u32, b: u32) -> (u32, Flags) {
    let (a, b) = aligned(a, b);
    chop(value(a) + value(b))
}

/// `a - b`.
#[allow(clippy::inline_always, reason = "the VUs' hot path")]
#[inline(always)]
#[must_use]
pub fn sub(a: u32, b: u32) -> (u32, Flags) {
    add(a, b ^ 0x8000_0000)
}

/// `a * b`.
#[allow(clippy::inline_always, reason = "the VUs' hot path")]
#[inline(always)]
#[must_use]
pub fn mul(a: u32, b: u32) -> (u32, Flags) {
    chop(value(a) * value(b))
}

/// `acc + a * b`, rounded after each step.
#[allow(clippy::inline_always, reason = "the VUs' hot path")]
#[inline(always)]
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
#[allow(clippy::inline_always, reason = "the VUs' hot path")]
#[inline(always)]
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
#[allow(clippy::inline_always, reason = "the VUs' hot path")]
#[inline(always)]
#[must_use]
pub fn max(a: u32, b: u32) -> u32 {
    if value(a) >= value(b) { a } else { b }
}

/// The smaller of two values.
#[allow(clippy::inline_always, reason = "the VUs' hot path")]
#[inline(always)]
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

/// The VUs' operations on four components at once, giving each component
/// what the scalar operation gives, bit for bit.
///
/// Every lane is computed without branches, from operations SSE2 has
/// (`f64` compares rather than 64-bit integer ones), so the compiler
/// vectorises them; the caller keeps the lanes it writes. Flags are 0 or 1
/// per lane.
pub mod x4 {
    use super::MAX;

    /// Four lanes.
    pub type V = [u32; 4];

    /// A result and its overflow and underflow flags.
    pub type Out = (V, V, V);

    /// [`super::value`], per lane.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn values(a: V) -> [f64; 4] {
        let mut out = [0.0; 4];
        for i in 0..4 {
            let exponent = (a[i] >> 23) & 0xFF;
            let sign = a[i] & 0x8000_0000;
            let bits = if exponent == 0 { sign } else { a[i] };
            let bits = if exponent == 0xFF { sign | MAX } else { bits };
            out[i] = f64::from(f32::from_bits(bits));
        }
        out
    }

    /// [`super::chop`], per lane.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn chop(x: [f64; 4]) -> Out {
        let (mut r, mut o, mut u) = ([0; 4], [0; 4], [0; 4]);
        for i in 0..4 {
            let bits = x[i].to_bits();
            let sign = ((bits >> 32) as u32) & 0x8000_0000;
            let magnitude = x[i].abs();
            // A NaN overflows, as in `chop`.
            let overflow = magnitude > f64::from(f32::MAX) || magnitude.is_nan();
            let tiny = magnitude < f64::from(f32::MIN_POSITIVE);
            let underflow = tiny && magnitude != 0.0;
            // Exponent and mantissa from the double's bits, rebiased; only
            // the low 32 bits matter (modulo 2^32).
            let normal = ((magnitude.to_bits() >> 29) as u32).wrapping_sub(896 << 23);
            let value = if tiny { 0 } else { normal };
            r[i] = sign | if overflow { MAX } else { value };
            o[i] = u32::from(overflow);
            u[i] = u32::from(underflow && !overflow);
        }
        (r, o, u)
    }

    /// [`super::vu::add`], per lane.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn add(a: V, b: V) -> V {
        let (x, y) = (values(a), values(b));
        std::array::from_fn(|i| super::vu::sum(x[i], y[i]))
    }

    /// [`super::vu::sub`], per lane.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn sub(a: V, b: V) -> V {
        add(a, b.map(|x| x ^ 0x8000_0000))
    }

    /// [`super::vu::mul`], per lane.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn mul(a: V, b: V) -> V {
        let (x, y) = (values(a), values(b));
        chop(std::array::from_fn(|i| x[i] * y[i])).0
    }

    /// [`super::vu::madd`], per lane.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn madd(acc: V, a: V, b: V) -> V {
        add(acc, mul(a, b))
    }

    /// [`super::vu::msub`], per lane.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn msub(acc: V, a: V, b: V) -> V {
        sub(acc, mul(a, b))
    }

    /// [`add`], for an operand known to hold no exponent 255 (`A`, `B`):
    /// the same here, where every operand is read exactly; the host forms
    /// ([`rtz`]) skip reading such operands as ±`MAX`.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn add_k<const A: bool, const B: bool>(a: V, b: V) -> V {
        add(a, b)
    }

    /// [`sub`], knowing what [`add_k`] knows.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn sub_k<const A: bool, const B: bool>(a: V, b: V) -> V {
        sub(a, b)
    }

    /// [`mul`], knowing what [`add_k`] knows.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn mul_k<const A: bool, const B: bool>(a: V, b: V) -> V {
        mul(a, b)
    }

    /// [`madd`], knowing what [`add_k`] knows of ACC (`C`) and the factors.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn madd_k<const C: bool, const A: bool, const B: bool>(acc: V, a: V, b: V) -> V {
        madd(acc, a, b)
    }

    /// [`msub`], knowing what [`madd_k`] knows.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn msub_k<const C: bool, const A: bool, const B: bool>(acc: V, a: V, b: V) -> V {
        msub(acc, a, b)
    }

    /// The same operations for code run in the VU environment
    /// (`ps2_fenv::with_vu_rounding`: rounding toward zero, denormals
    /// flushed): one host single-precision operation per lane, which there
    /// gives exactly what [`add`], [`mul`] and the rest give. Outside it
    /// they round to nearest, so they must not run there; and the exact
    /// forms must not run inside, where `f64` rounds toward zero too.
    pub mod rtz {
        use super::{MAX, V};

        /// The lanes as singles, exponent 255 read as ±`MAX` (denormals
        /// read as zero by themselves, in the VU environment).
        /// With `CLEAN`, the caller knows no lane holds exponent 255
        /// (what these operations give never does: rounding toward zero,
        /// a result too large becomes `MAX`, not infinity), so the lanes
        /// are read as they are.
        #[allow(clippy::inline_always, reason = "the VUs' hot path")]
        #[inline(always)]
        fn singles<const CLEAN: bool>(a: V) -> [f32; 4] {
            a.map(|x| {
                let huge = !CLEAN && (x >> 23) & 0xFF == 0xFF;
                f32::from_bits(if huge { (x & 0x8000_0000) | MAX } else { x })
            })
        }

        /// [`super::add`].
        #[allow(clippy::inline_always, reason = "the VUs' hot path")]
        #[inline(always)]
        #[must_use]
        pub fn add(a: V, b: V) -> V {
            add_k::<false, false>(a, b)
        }

        /// [`super::add_k`]: `a` or `b` known to hold no exponent 255.
        #[allow(clippy::inline_always, reason = "the VUs' hot path")]
        #[inline(always)]
        #[must_use]
        pub fn add_k<const A: bool, const B: bool>(a: V, b: V) -> V {
            let (x, y) = (singles::<A>(a), singles::<B>(b));
            std::array::from_fn(|i| (x[i] + y[i]).to_bits())
        }

        /// [`super::sub`].
        #[allow(clippy::inline_always, reason = "the VUs' hot path")]
        #[inline(always)]
        #[must_use]
        pub fn sub(a: V, b: V) -> V {
            sub_k::<false, false>(a, b)
        }

        /// [`super::sub_k`].
        #[allow(clippy::inline_always, reason = "the VUs' hot path")]
        #[inline(always)]
        #[must_use]
        pub fn sub_k<const A: bool, const B: bool>(a: V, b: V) -> V {
            let (x, y) = (singles::<A>(a), singles::<B>(b));
            std::array::from_fn(|i| (x[i] - y[i]).to_bits())
        }

        /// [`super::mul`].
        #[allow(clippy::inline_always, reason = "the VUs' hot path")]
        #[inline(always)]
        #[must_use]
        pub fn mul(a: V, b: V) -> V {
            mul_k::<false, false>(a, b)
        }

        /// [`super::mul_k`].
        #[allow(clippy::inline_always, reason = "the VUs' hot path")]
        #[inline(always)]
        #[must_use]
        pub fn mul_k<const A: bool, const B: bool>(a: V, b: V) -> V {
            let (x, y) = (singles::<A>(a), singles::<B>(b));
            std::array::from_fn(|i| (x[i] * y[i]).to_bits())
        }

        /// [`super::madd`]: the product rounded, then added.
        #[allow(clippy::inline_always, reason = "the VUs' hot path")]
        #[inline(always)]
        #[must_use]
        pub fn madd(acc: V, a: V, b: V) -> V {
            madd_k::<false, false, false>(acc, a, b)
        }

        /// [`super::madd_k`]: the product is known clean.
        #[allow(clippy::inline_always, reason = "the VUs' hot path")]
        #[inline(always)]
        #[must_use]
        pub fn madd_k<const C: bool, const A: bool, const B: bool>(acc: V, a: V, b: V) -> V {
            add_k::<C, true>(acc, mul_k::<A, B>(a, b))
        }

        /// [`super::msub`].
        #[allow(clippy::inline_always, reason = "the VUs' hot path")]
        #[inline(always)]
        #[must_use]
        pub fn msub(acc: V, a: V, b: V) -> V {
            msub_k::<false, false, false>(acc, a, b)
        }

        /// [`super::msub_k`].
        #[allow(clippy::inline_always, reason = "the VUs' hot path")]
        #[inline(always)]
        #[must_use]
        pub fn msub_k<const C: bool, const A: bool, const B: bool>(acc: V, a: V, b: V) -> V {
            sub_k::<C, true>(acc, mul_k::<A, B>(a, b))
        }
    }

    /// ITOF: each lane as a signed integer with `scale` fraction bits,
    /// rounded toward zero to a PS2 single.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn from_fixed(a: V, scale: u32) -> V {
        let divisor = f64::from(1u32 << scale);
        chop(a.map(|x| f64::from(x.cast_signed()) / divisor)).0
    }

    /// FTOI: each lane times 2^`scale`, rounded toward zero to a PS2
    /// single, then to a signed integer, saturating ([`super::to_int`]).
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn to_fixed(a: V, scale: u32) -> V {
        let factor = f64::from(1u32 << scale);
        let scaled = chop(values(a).map(|x| x * factor)).0;
        // `as` truncates and saturates, as `to_int` does.
        values(scaled).map(|x| (x as i32).cast_unsigned())
    }

    /// [`super::max`], per lane.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn max(a: V, b: V) -> V {
        let (x, y) = (values(a), values(b));
        std::array::from_fn(|i| if x[i] >= y[i] { a[i] } else { b[i] })
    }

    /// [`super::min`], per lane.
    #[allow(clippy::inline_always, reason = "the VUs' hot path")]
    #[inline(always)]
    #[must_use]
    pub fn min(a: V, b: V) -> V {
        let (x, y) = (values(a), values(b));
        std::array::from_fn(|i| if x[i] <= y[i] { a[i] } else { b[i] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chop_from_the_bits_agrees_with_rounding_then_correcting() {
        // Doubles spread over every exponent near and inside single
        // precision's range, with assorted mantissas and both signs.
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        for exponent in 800u64..1200 {
            for _ in 0..2000 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let mantissa = state & ((1 << 52) - 1);
                for sign in [0, 1u64 << 63] {
                    let x = f64::from_bits(sign | (exponent << 52) | mantissa);
                    assert_eq!(chop(x), finish(x, true), "{x:e}");
                }
            }
        }
        for x in [
            0.0,
            -0.0,
            f64::from(f32::MAX),
            f64::from(f32::MIN_POSITIVE),
            1.0,
            -3.5,
            f64::NAN,
            -f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX,
            f64::MIN_POSITIVE,
            f64::from_bits(1),
            -f64::from_bits(0x000F_FFFF_FFFF_FFFF),
        ] {
            assert_eq!(chop(x), finish(x, true), "{x:e}");
        }
    }

    /// `value` and `aligned` as first written, with branches.
    fn value_reference(bits: u32) -> f64 {
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

    fn aligned_reference(a: u32, b: u32) -> (u32, u32) {
        let truncate = |small: u32, difference: u32| {
            if difference >= 24 {
                small & 0x8000_0000
            } else if difference > 0 {
                small & !((1 << difference) - 1)
            } else {
                small
            }
        };
        let (ea, eb) = ((a >> 23) & 0xFF, (b >> 23) & 0xFF);
        if ea == 0 || eb == 0 {
            return (a, b);
        }
        if ea >= eb {
            (a, truncate(b, ea - eb))
        } else {
            (truncate(a, eb - ea), b)
        }
    }

    /// The four-lane operations give each lane what the scalar ones give:
    /// random operands over every exponent (zero, denormal, 255 and the
    /// ones between), both signs, and operands near each other's
    /// magnitude, where alignment and cancellation matter.
    #[test]
    fn four_lanes_agree_with_one() {
        let mut state = 0x6A09_E667_F3BC_C909u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let flags = |f: Flags| (u32::from(f.overflow), u32::from(f.underflow));
        for _ in 0..200_000 {
            let mut lanes = || -> [u32; 4] {
                std::array::from_fn(|_| {
                    let r = next();
                    let exponent = match r % 8 {
                        0 => 0,
                        1 => 0xFF,
                        2 => 0xFE,
                        3 => 1,
                        _ => ((r >> 8) as u32) & 0xFF,
                    };
                    ((r >> 16) as u32 & 0x807F_FFFF) | (exponent << 23)
                })
            };
            let (acc, a, mut b) = (lanes(), lanes(), lanes());
            // Some lanes close in magnitude to the other operand.
            b[0] = a[0] ^ 0x8000_0000;
            b[1] = a[1].wrapping_add(1);
            for i in 0..4 {
                let lanes = [
                    ("add", x4::add(a, b), vu::add(a[i], b[i])),
                    ("sub", x4::sub(a, b), vu::sub(a[i], b[i])),
                    ("mul", x4::mul(a, b), vu::mul(a[i], b[i])),
                    ("madd", x4::madd(acc, a, b), vu::madd(acc[i], a[i], b[i])),
                    ("msub", x4::msub(acc, a, b), vu::msub(acc[i], a[i], b[i])),
                ];
                for (name, four, one) in lanes {
                    assert_eq!(four[i], one, "{name} {:#x} {:#x} {:#x}", acc[i], a[i], b[i]);
                }
                assert_eq!(x4::max(a, b)[i], max(a[i], b[i]));
                for scale in [0, 4, 12, 15] {
                    let factor = f64::from(1u32 << scale);
                    assert_eq!(
                        x4::from_fixed(a, scale)[i],
                        chop(f64::from(a[i].cast_signed()) / factor).0
                    );
                    assert_eq!(
                        x4::to_fixed(a, scale)[i],
                        to_int(chop(value(a[i]) * factor).0),
                        "{:#x} {scale}",
                        a[i]
                    );
                }
                assert_eq!(x4::min(a, b)[i], min(a[i], b[i]));
            }
            let doubles: [f64; 4] = std::array::from_fn(|_| f64::from_bits(next()));
            let (r, o, u) = x4::chop(doubles);
            for i in 0..4 {
                let (one, f) = chop(doubles[i]);
                assert_eq!(
                    (r[i], o[i], u[i]),
                    (one, flags(f).0, flags(f).1),
                    "{:e}",
                    doubles[i]
                );
            }
        }
    }

    #[test]
    fn branch_free_value_and_alignment_agree_with_the_first_forms() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for exponent in 0..=255u32 {
            for _ in 0..512 {
                let r = next();
                let a = ((r as u32) & 0x807F_FFFF) | (exponent << 23);
                assert_eq!(value(a).to_bits(), value_reference(a).to_bits(), "{a:#x}");
                // Every exponent difference, both orders.
                let b = (((r >> 32) as u32) & 0x807F_FFFF) | (((r >> 40) as u32 & 0xFF) << 23);
                assert_eq!(aligned(a, b), aligned_reference(a, b), "{a:#x} {b:#x}");
                assert_eq!(aligned(b, a), aligned_reference(b, a), "{b:#x} {a:#x}");
            }
        }
    }

    /// The exact sum of two singles (read as [`value`] does) rounded
    /// toward zero, with results below the smallest normal flushed and
    /// those beyond `MAX` clamped: in integers, independently of `vu`.
    fn add_reference(a: u32, b: u32) -> u32 {
        let read = |x: u32| {
            let exponent = (x >> 23) & 0xFF;
            let sign = x & 0x8000_0000;
            match exponent {
                0 => sign,
                0xFF => sign | MAX,
                _ => x,
            }
        };
        let (a, b) = (read(a), read(b));
        // value = mantissa * 2^(exponent - 150)
        let parts = |x: u32| {
            let exponent = (x >> 23) & 0xFF;
            let magnitude = if exponent == 0 {
                0
            } else {
                i128::from((x & 0x7F_FFFF) | 0x80_0000)
            };
            let signed = if x >> 31 == 1 { -magnitude } else { magnitude };
            (signed, i32::try_from(exponent).unwrap_or(0) - 150)
        };
        let ((ma, ea), (mb, eb)) = (parts(a), parts(b));
        if ma == 0 && mb == 0 {
            return a & b & 0x8000_0000;
        }
        let toward_zero = |sum: i128, low: i32| -> u32 {
            if sum == 0 {
                return 0;
            }
            let sign = if sum < 0 { 0x8000_0000 } else { 0 };
            let mut m = sum.unsigned_abs();
            let mut e = low;
            let width = 128 - i32::try_from(m.leading_zeros()).unwrap_or(0);
            if width > 24 {
                m >>= width - 24;
                e += width - 24;
            } else {
                m <<= 24 - width;
                e -= 24 - width;
            }
            let biased = e + 150;
            if biased >= 255 {
                sign | MAX
            } else if biased <= 0 {
                sign
            } else {
                sign | u32::try_from(biased).unwrap_or(0) << 23
                    | (u32::try_from(m).unwrap_or(0) & 0x7F_FFFF)
            }
        };
        if ma == 0 || mb == 0 {
            return if ma == 0 {
                toward_zero(mb, eb)
            } else {
                toward_zero(ma, ea)
            };
        }
        let (big, small) = if ea >= eb {
            ((ma, ea), (mb, eb))
        } else {
            ((mb, eb), (ma, ea))
        };
        if big.1 - small.1 > 80 {
            // The smaller one only decides on which side of the larger the
            // sum falls: just inside it (toward zero) when signs differ.
            let inside = (big.0 < 0) != (small.0 < 0);
            let scaled = big.0 * 4 - if inside { big.0.signum() } else { 0 };
            return toward_zero(scaled, big.1 - 2);
        }
        let sum = (big.0 << (big.1 - small.1)) + small.0;
        toward_zero(sum, small.1)
    }

    fn check_add(a: u32, b: u32) {
        assert_eq!(vu::add(a, b), add_reference(a, b), "add {a:#x} {b:#x}");
    }

    #[test]
    fn vu_add_rounds_the_exact_sum_toward_zero() {
        // Every exponent pair near overflow, near the smallest normal, far
        // apart and across the range, with mantissas that cancel, carry or
        // sit on a single's last place, both signs.
        let mut state = 0x243F_6A88_85A3_08D3u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let exponents: Vec<u32> = (0..=255)
            .filter(|e| *e < 30 || *e > 225 || e % 7 == 0)
            .collect();
        for &ea in &exponents {
            for &eb in &exponents {
                for k in 0..24 {
                    let r = next();
                    let ma = match k % 4 {
                        0 => 0x7F_FFFF,
                        1 => 0,
                        _ => (r as u32) & 0x7F_FFFF,
                    };
                    let mb = match k % 6 {
                        0 => ma,
                        1 => ma ^ 1,
                        2 => 0x40_0000,
                        _ => ((r >> 32) as u32) & 0x7F_FFFF,
                    };
                    let a = ((r >> 62) as u32) << 31 | ea << 23 | ma;
                    let b = ((r >> 61) as u32 & 1) << 31 | eb << 23 | mb;
                    check_add(a, b);
                }
            }
        }
        // Cancellation to zero, signed zeros, and a tiny opposite operand
        // just under a single.
        for (a, b) in [
            (0x3F80_0000, 0xBF80_0000),
            (0x8000_0000, 0x8000_0000),
            (0x0000_0000, 0x8000_0000),
            (0x3F80_0000, 0x8080_0000),
            (0x7F7F_FFFF, 0x0080_0000),
            (0x7F7F_FFFF, 0xF3FF_FFFF),
        ] {
            check_add(a, b);
        }
    }

    /// A long run of random operands (`cargo test --release -- --ignored`).
    #[test]
    #[ignore = "slow: run before changing the VUs' add"]
    fn vu_add_rounds_toward_zero_at_length() {
        let mut state = 0x1319_8A2E_0370_7344u64;
        for _ in 0..50_000_000u32 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            check_add(state as u32, (state >> 32) as u32);
        }
    }

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
