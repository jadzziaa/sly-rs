//! The integer arithmetic the GS draws triangles' texture coordinates
//! with, shared by the software GS and the GPU's shaders so that the two
//! give the same pixels (see `docs/research/gpu-gs.md`).
//!
//! Texture coordinates are integers in 1/16 texel, rounded down: nearest
//! sampling takes `c >> 4`, bilinear `c - 8` then shifts. A triangle's are
//! computed from its edge functions `w` (the integer weights of its three
//! vertices at a pixel, summing to its doubled area), with no floating
//! point per pixel:
//!
//! - UV coordinates (FST): `floor(Σ wₖ·Uₖ / area)`, `Uₖ` the vertices' UV
//!   in 1/16 texel.
//! - STQ (perspective): each of S, T and Q is scaled at setup, once per
//!   triangle, to integers of at most 24 bits ([`Stq::new`]); per pixel,
//!   `Σ wₖ·Sₖ` and `Σ wₖ·Qₖ` are exact, and their ratio comes from a
//!   32-bit reciprocal ([`Reciprocal`]).
//!
//! Everything else a pixel needs (depth, colour, fog, the inside test)
//! was integer already and is unchanged.

/// One of S, T or Q at a triangle's three vertices, as integers: the
/// values times `2^scale`, rounded to nearest, the largest magnitude in
/// `[2^23, 2^24)`. Non-finite values count as 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Scaled {
    pub(crate) values: [i64; 3],
    pub(crate) scale: i32,
}

impl Scaled {
    pub(crate) fn new(values: [f32; 3]) -> Self {
        let values = values.map(|v| if v.is_finite() { f64::from(v) } else { 0.0 });
        let largest = values.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        if largest == 0.0 {
            return Self {
                values: [0; 3],
                scale: 0,
            };
        }
        // The largest value's binary exponent (every f32 is a normal f64).
        let exponent = ((largest.to_bits() >> 52) & 0x7FF) as i32 - 1023;
        let scale = 23 - exponent;
        Self {
            values: values.map(|v| (v * 2f64.powi(scale)).round() as i64),
            scale,
        }
    }

    /// `Σ wₖ·vₖ`: at most 3·2^35·2^24, inside an `i64`.
    #[must_use]
    pub(crate) fn weighted(&self, w: [i64; 3]) -> i64 {
        w[0] * self.values[0] + w[1] * self.values[1] + w[2] * self.values[2]
    }
}

/// A triangle's S, T and Q, scaled, and the exponents that turn `S/Q` and
/// `T/Q` into 1/16 texel.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Stq {
    s: Scaled,
    t: Scaled,
    q: Scaled,
    shift_u: i32,
    shift_v: i32,
}

impl Stq {
    /// From the vertices' S, T, Q and the texture's width and height as
    /// powers of two.
    pub(crate) fn new(stq: [[f32; 3]; 3], tw: u32, th: u32) -> Self {
        let (s, t, q) = (
            Scaled::new(stq.map(|v| v[0])),
            Scaled::new(stq.map(|v| v[1])),
            Scaled::new(stq.map(|v| v[2])),
        );
        // u = 16·2^tw·s/q, with s = Σwₖ·Sₖ / (area·2^scale_s) and q
        // likewise: the areas cancel.
        let (tw, th) = (tw.min(10) as i32, th.min(10) as i32);
        Self {
            s,
            t,
            q,
            shift_u: q.scale - s.scale + tw + 4,
            shift_v: q.scale - t.scale + th + 4,
        }
    }

    /// The texture coordinates at a pixel with weights `w`, in 1/16 texel.
    #[must_use]
    pub(crate) fn at(&self, w: [i64; 3]) -> (i32, i32) {
        let q = Reciprocal::of(self.q.weighted(w));
        (
            q.ratio(self.s.weighted(w), self.shift_u),
            q.ratio(self.t.weighted(w), self.shift_v),
        )
    }
}

/// The largest magnitude [`Reciprocal::ratio`] gives, so that later
/// offsets do not overflow.
pub(crate) const RATIO_MAX: u64 = 1 << 30;

/// `2^62 / d` for `d`'s top 32 bits (`d` normalised by its leading zeros):
/// one exact 64-by-32-bit division, rounded down and up. `d = 0` counts
/// as 1.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Reciprocal {
    negative: bool,
    zeros: u32,
    down: u64,
    up: u64,
}

impl Reciprocal {
    #[must_use]
    pub(crate) fn of(d: i64) -> Self {
        let ad = d.unsigned_abs().max(1);
        let zeros = ad.leading_zeros();
        let top = (ad << zeros) >> 32;
        let (down, rest) = ((1u64 << 62) / top, (1u64 << 62) % top);
        Self {
            negative: d < 0,
            zeros,
            down,
            up: down + u64::from(rest != 0),
        }
    }

    /// `n / d · 2^k`, rounded down: `n`'s top 32 bits times the
    /// reciprocal, shifted, the magnitude held to [`RATIO_MAX`]. Each
    /// approximation leans toward the exact quotient, so a quotient that is
    /// exactly an integer comes out exactly: a positive one takes the
    /// reciprocal rounded up and drops the fraction, a negative one takes it
    /// rounded down and rounds the magnitude up.
    #[must_use]
    pub(crate) fn ratio(&self, n: i64, k: i32) -> i32 {
        if n == 0 {
            return 0;
        }
        let negative = (n < 0) != self.negative;
        let an = n.unsigned_abs();
        let zn = an.leading_zeros();
        let top = (an << zn) >> 32;
        let product = top * if negative { self.down } else { self.up };
        // n/d ≈ (top_n/top_d)·2^(zd-zn) ≈ product·2^(zd-zn-62).
        let shift = 62 - k + zn as i32 - self.zeros as i32;
        let magnitude = if shift >= 63 {
            u64::from(negative && product != 0)
        } else if shift > 0 {
            let whole = product >> shift;
            whole + u64::from(negative && product & ((1 << shift) - 1) != 0)
        } else if shift > -33 && product <= RATIO_MAX >> -shift {
            product << -shift
        } else {
            RATIO_MAX
        }
        .min(RATIO_MAX) as i32;
        if negative { -magnitude } else { magnitude }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `floor(n/d·2^k)` in exact arithmetic.
    fn exact(n: i64, d: i64, k: i32) -> i128 {
        let (mut n, mut d) = (i128::from(n), i128::from(if d == 0 { 1 } else { d }));
        if d < 0 {
            (n, d) = (-n, -d);
        }
        if k >= 0 {
            (n << k).div_euclid(d)
        } else {
            n.div_euclid(d << -k)
        }
    }

    #[test]
    fn ratio_is_the_quotient_to_within_its_precision() {
        let mut state = 0x1234_5678_9ABC_DEF1u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..200_000 {
            let n = (next() as i64) >> (next() % 60);
            let d = match (next() as i64) >> (next() % 60) {
                0 => 1,
                d => d,
            };
            let k = (next() % 40) as i32 - 20;
            let want = exact(n, d, k);
            let got = i128::from(Reciprocal::of(d).ratio(n, k));
            if want.unsigned_abs() >= u128::from(RATIO_MAX) {
                continue;
            }
            // 2^-29 relative, and one more for the rounding of each side.
            let slack = (want.abs() >> 28) + 2;
            assert!(
                (got - want).abs() <= slack,
                "{n} / {d} · 2^{k}: {got} against {want}"
            );
        }
    }

    #[test]
    fn scaled_values_keep_24_bits_and_their_sign() {
        let s = Scaled::new([0.75, -3.0, 1e-9]);
        assert_eq!(s.values[1], -(3 << 22));
        assert_eq!(s.values[0], 3 << 20);
        assert_eq!(s.values[2], 0);
        assert_eq!(Scaled::new([0.0, f32::NAN, f32::INFINITY]).values, [0; 3]);
    }

    #[test]
    fn exact_quotients_come_out_exactly() {
        for (n, d, k) in [
            (6, 3, 4),
            (-6, 3, 4),
            (7 << 20, 7, -3),
            (-(5 << 20), -5, 0),
            (3, 3, 10),
        ] {
            let want = exact(n, d, k);
            assert_eq!(
                i128::from(Reciprocal::of(d).ratio(n, k)),
                want,
                "{n} / {d} · 2^{k}"
            );
        }
    }

    #[test]
    fn a_constant_coordinate_comes_out_exactly() {
        // The same S/Q at every vertex: every pixel gets that texel.
        let stq = Stq::new([[0.5, 0.25, 0.5]; 3], 6, 6);
        for w in [[1, 2, 3], [100_000, 7, 999], [0, 0, 5]] {
            assert_eq!(stq.at(w), (64 * 16, 32 * 16));
        }
    }
}
