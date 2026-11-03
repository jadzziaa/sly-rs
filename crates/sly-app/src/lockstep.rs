//! `lockstep`: the recompiled game and the interpreter side by side,
//! compared at every field. Execution is meant to be identical to the cycle,
//! so any difference is a recompiler bug; the first one is narrowed down to
//! a window of a few cycles. The IOP's registers and RAM are compared too.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ps2_ee::{FIELD_CYCLES, Machine};
use sly_common::Game;

use crate::Recompiled;

/// Where two machines differ, if they do.
fn difference(a: &Machine, b: &Machine) -> Option<String> {
    if a.cpu != b.cpu {
        let (x, y) = (&a.cpu, &b.cpu);
        let mut parts = Vec::new();
        if x.pc != y.pc || x.cycles != y.cycles {
            parts.push(format!(
                "pc {:08x}/{:08x} cycles {}/{}",
                x.pc, y.pc, x.cycles, y.cycles
            ));
        }
        for r in 0..32 {
            if x.gpr[r] != y.gpr[r] {
                parts.push(format!("r{r} {:x}/{:x}", x.gpr[r], y.gpr[r]));
            }
        }
        if x.fpr != y.fpr || x.fpu_acc != y.fpu_acc || x.fcr31 != y.fcr31 {
            parts.push("FPU".to_owned());
        }
        if x.hi != y.hi || x.lo != y.lo || x.sa != y.sa {
            parts.push("HI/LO/SA".to_owned());
        }
        if x.vu0 != y.vu0 {
            parts.push("VU0".to_owned());
        }
        if x.cop0 != y.cop0 {
            parts.push("COP0".to_owned());
        }
        return Some(format!("registers: {}", parts.join(", ")));
    }
    if a.iop.core.cpu != b.iop.core.cpu {
        let (x, y) = (&a.iop.core.cpu, &b.iop.core.cpu);
        return Some(format!(
            "IOP registers: pc {:08x}/{:08x} cycles {}/{}",
            x.pc, y.pc, x.cycles, y.cycles
        ));
    }
    for (name, x, y) in [
        ("RAM", &a.mem.regions.ram, &b.mem.regions.ram),
        (
            "scratchpad",
            &a.mem.regions.scratchpad,
            &b.mem.regions.scratchpad,
        ),
        ("IOP RAM", &a.mem.regions.iop_ram, &b.mem.regions.iop_ram),
    ] {
        if x != y {
            let at = x
                .iter()
                .zip(y.iter())
                .position(|(p, q)| p != q)
                .unwrap_or(0);
            return Some(format!("{name} from offset {at:#x}"));
        }
    }
    None
}

pub(crate) fn run(game: &str, fields: u64, recompiled: Option<&Recompiled>) -> Result<()> {
    let recompiled = recompiled
        .context("this binary carries no recompiled code (build one with cargo xtask recomp)")?;
    let game = Game::from_key(game).with_context(|| format!("unknown game {game:?}"))?;
    let (mut compiled, _) = crate::boot(game, Some(recompiled))?;
    let (mut interpreted, _) = crate::boot(game, None)?;
    let mut out = io::stdout().lock();
    let (mut time_c, mut time_i) = (Duration::ZERO, Duration::ZERO);
    for field in 1..=fields {
        let started = Instant::now();
        let a = compiled.run(FIELD_CYCLES);
        time_c += started.elapsed();
        let started = Instant::now();
        let b = interpreted.run(FIELD_CYCLES);
        time_i += started.elapsed();
        let differs = if a == b {
            difference(&compiled, &interpreted)
        } else {
            Some(format!("stops: {a:?} / {b:?}"))
        };
        if let Some(what) = differs {
            writeln!(out, "field {field}: the machines differ: {what}")?;
            narrow(game, recompiled, (field - 1) * FIELD_CYCLES, &mut out)?;
            bail!("lockstep failed at field {field}");
        }
        if field % 60 == 0 || field == fields {
            writeln!(
                out,
                "field {field}: equal (state {:016x})",
                compiled.state_hash()
            )?;
        }
    }
    let stats = &compiled.stats;
    let total = stats.compiled + stats.interpreted;
    #[allow(clippy::cast_precision_loss, reason = "a report")]
    let share = 100.0 * stats.compiled as f64 / total.max(1) as f64;
    writeln!(
        out,
        "{fields} fields equal; {} of {total} instructions recompiled ({share:.4}%), \
         {} more were system calls; host time {:.2} s recompiled, {:.2} s interpreted",
        stats.compiled,
        stats.kernel,
        time_c.as_secs_f64(),
        time_i.as_secs_f64()
    )?;
    writeln!(out, "exits: {:?}", stats.exits)?;
    Ok(())
}

/// Narrows the difference down: from `from` (where the machines were still
/// equal), runs both again in smaller and smaller chunks while the
/// difference still shows. Generated code runs only whole blocks that fit in
/// a chunk, so stepping one cycle at a time would hide a bug inside a block;
/// the smallest chunk that still shows the difference brackets it.
fn narrow(game: Game, recompiled: &Recompiled, from: u64, out: &mut impl Write) -> Result<()> {
    let (mut start, mut size) = (from, FIELD_CYCLES / 8);
    let mut window = None;
    while size >= 16 {
        let (mut compiled, _) = crate::boot(game, Some(recompiled))?;
        let (mut interpreted, _) = crate::boot(game, None)?;
        compiled.run(start);
        interpreted.run(start);
        let mut found = None;
        for _ in 0..=FIELD_CYCLES / size {
            let at = (compiled.cpu.pc, interpreted.cpu.cycles);
            compiled.run(size);
            interpreted.run(size);
            if let Some(what) = difference(&compiled, &interpreted) {
                found = Some((at, what));
                break;
            }
        }
        let Some(((pc, cycle), what)) = found else {
            break;
        };
        window = Some((pc, cycle, size, what));
        start = cycle;
        size /= 8;
    }
    match window {
        Some((pc, cycle, size, what)) => writeln!(
            out,
            "difference within {size} cycles from cycle {cycle} (pc {pc:08x}): {what}"
        )?,
        None => writeln!(out, "the difference did not show again in smaller chunks")?,
    }
    Ok(())
}
