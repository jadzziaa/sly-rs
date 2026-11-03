//! The VUs' four-lane float operations: `cargo bench -p ps2-core`.

#![allow(missing_docs, reason = "benchmark harness")]

use divan::{Bencher, black_box};
use ps2_core::float::x4;

fn main() {
    divan::main();
}

/// Normal singles of assorted magnitudes and signs, four to a vector.
fn vectors(n: usize) -> Vec<[u32; 4]> {
    let mut x = 0x2545_F491_u32;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        // Exponents 100..156, both signs.
        (x & 0x807F_FFFF) | ((100 + (x >> 8) % 56) << 23)
    };
    (0..n).map(|_| [next(), next(), next(), next()]).collect()
}

const N: usize = 1 << 12;

#[divan::bench]
fn add(b: Bencher<'_, '_>) {
    let v = vectors(N + 1);
    b.counter(divan::counter::ItemsCount::new(N)).bench(|| {
        v.windows(2)
            .fold(0, |m, w| m ^ black_box(x4::add(w[0], w[1]))[0])
    });
}

#[divan::bench]
fn mul(b: Bencher<'_, '_>) {
    let v = vectors(N + 1);
    b.counter(divan::counter::ItemsCount::new(N)).bench(|| {
        v.windows(2)
            .fold(0, |m, w| m ^ black_box(x4::mul(w[0], w[1]))[0])
    });
}

#[divan::bench]
fn madd(b: Bencher<'_, '_>) {
    let v = vectors(N + 2);
    b.counter(divan::counter::ItemsCount::new(N)).bench(|| {
        v.windows(3)
            .fold(0, |m, w| m ^ black_box(x4::madd(w[0], w[1], w[2]))[0])
    });
}
