//! The VUs' single-precision forms, run in the VU environment, give exactly
//! what their exact forms give.

#![cfg(target_arch = "x86_64")]
#![allow(clippy::cast_possible_truncation, reason = "random bits")]

use ps2_core::float::x4;

/// Operands across the whole range: random bits, and the edges (exponents
/// 0, 1, 254, 255; mantissas that cancel or carry).
fn operands(n: usize) -> Vec<[u32; 4]> {
    let mut state = 0x6A09_E667_F3BC_C909u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..n)
        .map(|_| {
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
        })
        .collect()
}

#[test]
fn single_precision_in_the_vu_environment_is_exact() {
    let v = operands(3_000_000);
    let triples: Vec<_> = v
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| (c[0], c[1], c[2]))
        .collect();
    // Some lanes close to the other operand: cancellation, one apart.
    let triples: Vec<_> = triples
        .into_iter()
        .map(|(acc, a, mut b)| {
            b[0] = a[0] ^ 0x8000_0000;
            b[1] = a[1].wrapping_add(1);
            (acc, a, b)
        })
        .collect();
    // Expected outside the environment (the exact forms need it), then the
    // single-precision forms inside.
    let exact: Vec<_> = triples
        .iter()
        .map(|&(acc, a, b)| {
            [
                x4::add(a, b),
                x4::sub(a, b),
                x4::mul(a, b),
                x4::madd(acc, a, b),
                x4::msub(acc, a, b),
            ]
        })
        .collect();
    let fast: Vec<_> = ps2_fenv::with_vu_rounding(|| {
        triples
            .iter()
            .map(|&(acc, a, b)| {
                [
                    x4::rtz::add(a, b),
                    x4::rtz::sub(a, b),
                    x4::rtz::mul(a, b),
                    x4::rtz::madd(acc, a, b),
                    x4::rtz::msub(acc, a, b),
                ]
            })
            .collect()
    });
    for (k, ((e, f), t)) in exact.iter().zip(&fast).zip(&triples).enumerate() {
        assert_eq!(e, f, "case {k}: {t:x?} (add, sub, mul, madd, msub)");
    }
}
