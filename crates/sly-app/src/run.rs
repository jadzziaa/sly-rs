//! `sly-rs run`: boot a game and report how far it gets, and where its
//! instructions ran.

use std::io::{self, Write};
use std::time::Instant;

use anyhow::{Context, Result};
use ps2_analysis::syscall_name;
use sly_common::Game;

use crate::Recompiled;

#[allow(
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    reason = "a report, read top to bottom"
)]
pub(crate) fn run(
    game: &str,
    fields: u64,
    watch: &[String],
    press: &[String],
    recompiled: Option<&Recompiled>,
) -> Result<()> {
    let presses = sly_common::replay::parse_presses(press)?;
    let game = Game::from_key(game).with_context(|| format!("unknown game {game:?}"))?;
    let recompiled = recompiled.filter(|_| watch.is_empty());
    let (mut machine, image) = crate::boot(game, recompiled)?;
    if recompiled.is_none() {
        machine.samples = Some(std::collections::BTreeMap::new());
    }
    for address in watch {
        let address =
            u32::from_str_radix(address.trim_start_matches("0x"), 16).context("watch address")?;
        machine.watches.insert(address, Vec::new());
    }
    let started = Instant::now();
    let stop = sly_common::replay::run(&mut machine, fields, &presses);
    let elapsed = started.elapsed();
    let mut out = io::stdout().lock();
    let cpu = &machine.cpu;
    writeln!(out, "{game}: stopped: {stop:?}")?;
    if game == Game::Sly1 {
        writeln!(
            out,
            "  game: {}",
            sly_common::state::Sly1State::read(&mut machine.mem)
        )?;
    }
    writeln!(
        out,
        "  at pc {:#010x} (ra {:#010x}), {} cycles = {} fields, {:.1} s host ({:.0} M instructions/s)",
        cpu.pc,
        cpu.r32(31),
        cpu.cycles,
        machine.fields,
        elapsed.as_secs_f64(),
        cpu.cycles as f64 / elapsed.as_secs_f64() / 1e6
    )?;
    writeln!(out, "  kernel calls:")?;
    for (number, count) in &machine.kernel.calls {
        writeln!(
            out,
            "    {:>8}  {} ({number:#x})",
            count,
            syscall_name(*number).unwrap_or("?")
        )?;
    }
    let threads: Vec<String> = machine
        .kernel
        .threads
        .iter()
        .enumerate()
        .filter_map(|(id, t)| {
            t.as_ref().map(|t| {
                let (pc, ra) = t.resumes_at().unwrap_or_default();
                format!(
                    "{id}: {:?} prio {} at {pc:08x} (ra {ra:08x})",
                    t.status, t.priority
                )
            })
        })
        .collect();
    writeln!(
        out,
        "  threads (running {:?}): {}",
        machine.kernel.current,
        threads.join("; ")
    )?;
    writeln!(
        out,
        "  semaphores: {}",
        machine.kernel.semas.iter().flatten().count()
    )?;
    let hw = machine.hw.borrow();
    writeln!(out, "  DMA kicks: {:?}", hw.dma_kicks)?;
    writeln!(out, "  IOP commands: {:x?}", machine.iop.commands)?;
    writeln!(
        out,
        "  IOP RPC calls (server, function): {:x?}",
        machine.iop.calls
    )?;
    writeln!(
        out,
        "  SIF registers: {:x?}; SBUS {:x?}",
        machine.kernel.sif_regs, hw.sbus
    )?;
    writeln!(out, "  not built yet: {:?}", machine.skipped)?;
    let core = &machine.iop.core;
    let iop_threads: Vec<String> = core
        .kernel
        .threads
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            t.as_ref().map(|t| {
                format!(
                    "{}: {:?} {:?} prio {} at {:08x}",
                    i + 1,
                    t.status,
                    t.wait,
                    t.priority,
                    if core.kernel.current == Some(i) {
                        core.cpu.pc
                    } else {
                        t.context.pc
                    }
                )
            })
        })
        .collect();
    writeln!(
        out,
        "  IOP threads (running {:?}): {}",
        core.kernel.current.map(|t| t + 1),
        iop_threads.join("; ")
    )?;
    let modules: Vec<String> = core
        .modules
        .iter()
        .map(|m| format!("{} at {:#x}", m.name, m.base))
        .collect();
    writeln!(out, "  IOP modules: {}", modules.join(", "))?;
    let mut iop_calls: Vec<String> = core
        .calls
        .iter()
        .map(|((lib, i), n)| format!("{lib} {i}×{n}"))
        .collect();
    iop_calls.sort();
    writeln!(out, "  IOP kernel calls: {}", iop_calls.join(", "))?;
    writeln!(
        out,
        "  IOP: {} cycles, {} sectors read by modules",
        core.cpu.cycles, core.sectors_read
    )?;
    writeln!(
        out,
        "  SIF packets waiting for the EE: {:x?}",
        machine.iop.waiting()
    )?;
    let stats = &machine.stats;
    if stats.entries > 0 {
        let total = stats.compiled + stats.interpreted;
        writeln!(
            out,
            "  recompiled: {} of {total} instructions ({:.4}%), and {} system calls; {} entries; exits {:?}",
            stats.compiled,
            100.0 * stats.compiled as f64 / total.max(1) as f64,
            stats.kernel,
            stats.entries,
            stats.exits
        )?;
        let mut interpreted: Vec<(u32, u64)> =
            stats.interpreted_at.iter().map(|(&a, &n)| (a, n)).collect();
        interpreted.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
        let top: Vec<String> = interpreted
            .iter()
            .take(12)
            .map(|(a, n)| format!("{a:08x}×{n}"))
            .collect();
        writeln!(out, "  interpreted most at: {}", top.join(" "))?;
    }
    drop(hw);
    let watches = machine.watches.clone();
    for (address, hits) in &watches {
        writeln!(out, "  watch {address:08x}: {} hits", hits.len())?;
        for hit in hits {
            let mut text = |at: u32| -> String {
                let bytes: Vec<u8> = (0..80)
                    .map(|i| machine.mem.read8(at + i))
                    .take_while(|&b| b != 0)
                    .collect();
                if bytes.len() > 2
                    && bytes
                        .iter()
                        .all(|b| b.is_ascii_graphic() || *b == b' ' || *b == b'\n')
                {
                    format!(" {:?}", String::from_utf8_lossy(&bytes))
                } else {
                    String::new()
                }
            };
            let (t0, t1) = (text(hit[0]), text(hit[1]));
            writeln!(
                out,
                "    a0 {:08x}{t0} a1 {:08x}{t1} a2 {:08x} a3 {:08x} ra {:08x}",
                hit[0], hit[1], hit[2], hit[3], hit[4]
            )?;
        }
    }
    // Where the time went, by function.
    let found = ps2_analysis::discover(&image, &[])?;
    let names: std::collections::BTreeMap<u32, &str> = sly_common::symbols(game)
        .iter()
        .map(|s| (s.address, s.readable))
        .collect();
    let mut by_function: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
    let samples = machine.samples.take().unwrap_or_default();
    let total: u64 = samples.values().sum();
    for (pc, n) in samples {
        let start = found.function_at(pc).map_or(pc, |f| f.start);
        *by_function.entry(start).or_default() += n;
    }
    let mut hot: Vec<(u32, u64)> = by_function.into_iter().collect();
    hot.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    writeln!(out, "  state hash {:016x}", machine.state_hash())?;
    writeln!(out, "  hottest functions ({total} samples):")?;
    for (start, n) in hot.into_iter().take(8) {
        writeln!(
            out,
            "    {:>5.1}%  {start:08x} {}",
            100.0 * n as f64 / total.max(1) as f64,
            names.get(&start).copied().unwrap_or("")
        )?;
    }
    Ok(())
}
