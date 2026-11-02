//! Decode and disassembly throughput: `cargo bench -p ps2-isa`.

#![allow(missing_docs, reason = "benchmark harness")]

use std::fmt::Write;

use divan::{Bencher, black_box};
use ps2_isa::ee::Insn;
use ps2_isa::vu::Pair;

fn main() {
    divan::main();
}

/// A fixed spread of words: an xorshift sequence covering every opcode table.
fn words(n: usize) -> Vec<u32> {
    let mut x = 0x2545_F491_u32;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        })
        .collect()
}

const N: usize = 1 << 16;

#[divan::bench]
fn ee_decode(bencher: Bencher<'_, '_>) {
    let words = words(N);
    bencher
        .counter(divan::counter::ItemsCount::new(N))
        .bench(|| {
            words
                .iter()
                .filter(|&&w| black_box(Insn::decode(w)).is_valid())
                .count()
        });
}

#[divan::bench]
fn ee_disassemble(bencher: Bencher<'_, '_>) {
    let words = words(N);
    let mut text = String::with_capacity(64);
    bencher
        .counter(divan::counter::ItemsCount::new(N))
        .bench_local(|| {
            for (pc, &w) in (0u32..).step_by(4).zip(&words) {
                text.clear();
                let _ = write!(text, "{}", Insn::decode(w).display(pc));
                black_box(&text);
            }
        });
}

#[divan::bench]
fn vu_decode(bencher: Bencher<'_, '_>) {
    let words = words(N * 2);
    bencher
        .counter(divan::counter::ItemsCount::new(N))
        .bench(|| {
            words
                .as_chunks::<2>()
                .0
                .iter()
                .filter(|p| black_box(Pair::from_words(p[0], p[1])).is_valid())
                .count()
        });
}
