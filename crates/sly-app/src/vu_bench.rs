//! `vu-bench`: runs recorded VU1 programs (`$SLY_RS_VU_RECORD`) through
//! the interpreter and through this binary's translated microcode, times
//! both, and checks that they give the same registers, data memory,
//! packets and pair counts.

use std::io::{self, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use ps2_gs::thread::Sink;
use ps2_vu::micro::{self, Cache, Translation, Unit, Vu1Link};
use ps2_vu::record::{Recording, Start, registers};

/// Packets sent, kept for comparing.
#[derive(Default, PartialEq, Eq)]
struct Packets(Vec<(usize, Vec<u128>)>);

impl Sink for Packets {
    fn gif(&mut self, path: usize, data: Vec<u128>) {
        self.0.push((path, data));
    }
}

/// What a program left behind.
#[derive(PartialEq, Eq)]
struct Outcome {
    registers: Vec<u32>,
    data: Vec<u8>,
    packets: Packets,
    pairs: u64,
}

fn run(
    rec: &Recording,
    start: &Start,
    cache: &mut Cache,
    translations: &'static [Translation],
) -> (Outcome, Duration) {
    let mut regs = start.regs.clone();
    let mut data = start.data.clone();
    let mut packets = Packets::default();
    let started = Instant::now();
    let run = micro::run(
        &mut Unit {
            regs: &mut regs,
            code: &rec.codes[start.code],
            data: &mut data,
            vu1: Some(Vu1Link {
                itop: start.itop,
                top: start.top,
                gs: &mut packets,
            }),
            cache,
            translations,
            // Each micro memory gets its own count, so translations are
            // matched again when the recording moves to another.
            code_writes: start.code as u64,
        },
        start.pc,
    );
    let elapsed = started.elapsed();
    (
        Outcome {
            registers: registers(&regs),
            data,
            packets,
            pairs: run.pairs,
        },
        elapsed,
    )
}

#[allow(clippy::cast_precision_loss, reason = "a report")]
pub(crate) fn bench(path: &Path, repeat: u32, translations: &'static [Translation]) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let rec = Recording::from_bytes(&bytes).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    let mut out = io::stdout().lock();
    let pairs: u64 = rec
        .starts
        .iter()
        .map(|s| run(&rec, s, &mut Cache::default(), &[]).0.pairs)
        .sum();
    writeln!(
        out,
        "{} programs, {} micro memories, {pairs} pairs",
        rec.starts.len(),
        rec.codes.len()
    )?;
    let mut ways = vec![("interpreted", &[][..])];
    if !translations.is_empty() {
        ways.push(("translated", translations));
    }
    for (name, translations) in ways {
        let mut best = Duration::MAX;
        let mut cache = Cache::default();
        for _ in 0..repeat.max(1) {
            let total: Duration = rec
                .starts
                .iter()
                .map(|s| run(&rec, s, &mut cache, translations).1)
                .sum();
            best = best.min(total);
        }
        writeln!(
            out,
            "{name}: {:.3} s, {:.2} ns a pair",
            best.as_secs_f64(),
            best.as_secs_f64() * 1e9 / pairs.max(1) as f64
        )?;
    }
    if !translations.is_empty() {
        let (mut a, mut b) = (Cache::default(), Cache::default());
        for (k, s) in rec.starts.iter().enumerate() {
            let interpreted = run(&rec, s, &mut a, &[]).0;
            let translated = run(&rec, s, &mut b, translations).0;
            if interpreted != translated {
                let first = interpreted
                    .registers
                    .iter()
                    .zip(&translated.registers)
                    .position(|(x, y)| x != y);
                bail!(
                    "program {k} (at {:#06x}) differs: register word {first:?}, data {}, packets {}, pairs {} / {}",
                    s.pc,
                    interpreted.data == translated.data,
                    interpreted.packets == translated.packets,
                    interpreted.pairs,
                    translated.pairs
                );
            }
        }
        writeln!(out, "every program gives the same result both ways")?;
    }
    Ok(())
}
