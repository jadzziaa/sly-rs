//! `vu-bench`: runs recorded VU1 programs (`$SLY_RS_VU_RECORD`) through
//! the interpreter and through this binary's translated microcode, times
//! both, and checks that they give the same registers, data memory,
//! packets and pair counts. `vu-trace` lists a recording's programs or
//! steps one pair by pair.

use std::fmt::Write as _;
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

/// The 32-bit words of a quadword, x first.
fn words(q: u128) -> [u32; 4] {
    let b = q.to_le_bytes();
    [0, 1, 2, 3].map(|k| u32::from_le_bytes([b[4 * k], b[4 * k + 1], b[4 * k + 2], b[4 * k + 3]]))
}

/// Whether a packet carries a 32-bit word.
fn carries(packet: &[u128], word: u32) -> bool {
    packet.iter().any(|&q| words(q).contains(&word))
}

/// Where `word` sits in data memory, as a qword index.
fn held(data: &[u8], word: u32) -> Option<usize> {
    data.as_chunks::<4>()
        .0
        .iter()
        .position(|b| u32::from_le_bytes(*b) == word)
        .map(|i| i / 4)
}

/// `vu-trace`: lists a recording's programs (entry, pairs, packets sent,
/// and where `word` is held and whether a packet carries it), or steps
/// one through the interpreter.
pub(crate) fn trace(
    path: &Path,
    program: Option<usize>,
    word: Option<u32>,
    data: Option<(usize, usize)>,
) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let rec = Recording::from_bytes(&bytes).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    let mut out = io::stdout().lock();
    let Some(index) = program else {
        for (k, start) in rec.starts.iter().enumerate() {
            let (o, _) = run(&rec, start, &mut Cache::default(), &[]);
            let mut line = format!(
                "{k:3}: pc {:#06x} top {:#x} itop {:#x} pairs {} packets {} ({} qwords)",
                start.pc,
                start.top,
                start.itop,
                o.pairs,
                o.packets.0.len(),
                o.packets.0.iter().map(|(_, p)| p.len()).sum::<usize>(),
            );
            if let Some(w) = word {
                let sent = o.packets.0.iter().any(|(_, p)| carries(p, w));
                write!(
                    line,
                    ", word held at qword {:x?}, sent {sent}",
                    held(&start.data, w)
                )?;
            }
            writeln!(out, "{line}")?;
        }
        return Ok(());
    };
    let start = rec
        .starts
        .get(index)
        .ok_or_else(|| anyhow!("no program {index}"))?;
    if let Some((from, count)) = data {
        for q in from..(from + count).min(start.data.len() / 16) {
            let mut raw = [0; 16];
            raw.copy_from_slice(&start.data[16 * q..16 * q + 16]);
            let [x, y, z, w] = words(u128::from_le_bytes(raw));
            writeln!(out, "  data {q:#05x}: {w:08x} {z:08x} {y:08x} {x:08x}")?;
        }
    }
    let packets = step(&rec, start, &mut out)?;
    for (path, p) in &packets.0 {
        write!(out, "packet on path {path}: {} qwords", p.len())?;
        if let Some(w) = word {
            write!(out, ", carries the word: {}", carries(p, w))?;
        }
        writeln!(out)?;
        for (i, &q) in p.iter().enumerate() {
            let [x, y, z, w] = words(q);
            writeln!(out, "  {i:4}: {w:08x} {z:08x} {y:08x} {x:08x}")?;
        }
    }
    Ok(())
}

/// Steps a program through the interpreter, printing each pair and the
/// registers it changed; gives the packets it sent.
fn step(rec: &Recording, s: &Start, out: &mut impl io::Write) -> Result<Packets> {
    let code = &rec.codes[s.code];
    let mut regs = s.regs.clone();
    let mut data = s.data.clone();
    let mut packets = Packets::default();
    let mut cache = Cache::default();
    let mut unit = Unit {
        regs: &mut regs,
        code,
        data: &mut data,
        vu1: Some(Vu1Link {
            itop: s.itop,
            top: s.top,
            gs: &mut packets,
        }),
        cache: &mut cache,
        translations: &[],
        code_writes: 0,
    };
    let mut state = micro::State::new(s.pc, &s.regs);
    loop {
        let pc = state.pc;
        let at = pc as usize;
        let mut raw = [0; 8];
        raw.copy_from_slice(&code[at..at + 8]);
        let pair = micro::Pair::decode(u64::from_le_bytes(raw));
        let before = unit.regs.clone();
        let ended = micro::exec(
            &mut unit,
            &mut state,
            pc,
            pair,
            micro::upper_class(pair.upper_op()),
        );
        writeln!(
            out,
            "{pc:#06x}  {:<60}{}",
            pair.display(pc).to_string(),
            changes(&before, unit.regs)
        )?;
        if ended || state.cycle > micro::MAX_PAIRS {
            break;
        }
    }
    Ok(packets)
}

/// The registers that differ between `a` and `b`, floats as floats.
fn changes(before: &ps2_core::Vu0, after: &ps2_core::Vu0) -> String {
    let floats = |v: [u32; 4]| v.map(f32::from_bits);
    let mut line = String::new();
    for (i, (old, new)) in before.vf.iter().zip(&after.vf).enumerate() {
        if old != new {
            let _ = write!(line, " vf{i}={:?}", floats(*new));
        }
    }
    for (i, (old, new)) in before.vi.iter().zip(&after.vi).enumerate() {
        if old != new {
            let _ = write!(line, " vi{i}={new:#x}");
        }
    }
    if before.acc != after.acc {
        let _ = write!(line, " acc={:?}", floats(after.acc));
    }
    for (name, old, new) in [
        ("q", before.q, after.q),
        ("p", before.p, after.p),
        ("mac", before.mac, after.mac),
        ("status", before.status, after.status),
        ("clip", before.clip, after.clip),
    ] {
        if old != new {
            let _ = write!(line, " {name}={new:#x}");
        }
    }
    line
}
