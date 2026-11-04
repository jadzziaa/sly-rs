//! The GS on a thread of its own.
//!
//! Nothing the EE does depends on what the GS draws: the only things that
//! flow back are the events GIF packets ask for (SIGNAL, FINISH, LABEL),
//! which a [`Scanner`] finds by walking the packets' tags as they are sent.
//! So the GS can draw on a worker thread while the machine runs on. It
//! receives the packets in the order they were sent, so what it draws does
//! not depend on the threads' timing. Whoever needs the GS itself (a
//! picture, a dump, a report) runs a closure on it with [`Threaded::with`],
//! which waits for the packets before it.

use std::sync::mpsc;
use std::thread::JoinHandle;

use crate::{Events, Gs};

/// Something GIF packets are sent to: the GS, or the GS on its thread.
pub trait Sink {
    /// Sends quadwords of GIF packets on `path` (1 to 3).
    fn gif(&mut self, path: usize, data: Vec<u128>);
}

impl Sink for Gs {
    fn gif(&mut self, path: usize, data: Vec<u128>) {
        Self::gif(self, path, data);
    }
}

/// A path's tag, as far as finding events needs it.
#[derive(Clone, Copy, Debug, Default)]
struct Tag {
    nloop: u32,
    nreg: u32,
    regs: u64,
    flg: u32,
    reg: u32,
}

/// Finds the events GIF packets ask for (PACKED A+D writes to SIGNAL,
/// FINISH and LABEL), keeping each path's tag across sends as the GS does.
#[derive(Clone, Debug, Default)]
pub struct Scanner {
    paths: [Option<Tag>; 4],
    events: Events,
}

impl Scanner {
    /// Walks quadwords sent on `path`.
    pub fn scan(&mut self, path: usize, data: &[u128]) {
        let slot = &mut self.paths[path.min(3)];
        for &q in data {
            let Some(mut tag) = *slot else {
                let lo = q as u64;
                let nloop = (lo & 0x7FFF) as u32;
                let flg = ((lo >> 58) & 3) as u32;
                let nreg = match (lo >> 60) as u32 {
                    0 => 16,
                    n => n,
                };
                *slot = (nloop > 0).then_some(Tag {
                    nloop,
                    nreg: if flg >= 2 { 1 } else { nreg },
                    regs: (q >> 64) as u64,
                    flg: flg.min(2),
                    reg: 0,
                });
                continue;
            };
            match tag.flg {
                0 => {
                    if (tag.regs >> (4 * tag.reg)) & 0xF == 0xE {
                        match (q >> 64) as u8 {
                            0x60 => self.events.signal = Some(q as u64),
                            0x61 => self.events.finish = true,
                            0x62 => self.events.label = Some(q as u64),
                            _ => {}
                        }
                    }
                    tag.reg += 1;
                }
                1 => {
                    // Two registers per quadword; the loop count ends per
                    // half, and the last half of an odd count is padding.
                    for _ in 0..2 {
                        tag.reg += 1;
                        if tag.reg == tag.nreg {
                            tag.reg = 0;
                            tag.nloop -= 1;
                            if tag.nloop == 0 {
                                break;
                            }
                        }
                    }
                }
                _ => tag.reg = tag.nreg,
            }
            if tag.reg == tag.nreg {
                tag.reg = 0;
                tag.nloop -= 1;
            }
            *slot = (tag.nloop > 0).then_some(tag);
        }
    }

    /// Takes the events found since the last call.
    pub fn take_events(&mut self) -> Events {
        std::mem::take(&mut self.events)
    }
}

/// Threads to draw with: `$SLY_RS_GS_THREADS`, or all but two of the
/// host's (the machine's thread and the GS's own), at most 16 (past that
/// the GS's own thread and the primitives drawn in order set the pace).
fn drawing_threads() -> usize {
    std::env::var("SLY_RS_GS_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(1, |n| n.get().saturating_sub(2))
                .clamp(1, 16)
        })
}

/// Packets the machine may be ahead of the GS by.
const QUEUE: usize = 4096;

/// Work for the GS's thread.
enum Job {
    Gif(usize, Vec<u128>),
    Call(Box<dyn FnOnce(&mut Gs) + Send>),
}

/// A GS on its own thread.
pub struct Threaded {
    jobs: Option<mpsc::SyncSender<Job>>,
    worker: Option<JoinHandle<()>>,
    scanner: Scanner,
    dumping: bool,
    /// Time the machine spent waiting for room in the queue (for reports).
    pub waited: std::time::Duration,
}

impl Default for Threaded {
    fn default() -> Self {
        // A bounded queue: when the GS falls behind, the machine waits for
        // it rather than piling packets up in memory.
        let (jobs, queue) = mpsc::sync_channel::<Job>(QUEUE);
        let worker = std::thread::Builder::new()
            .name("gs".to_owned())
            .spawn(move || {
                let mut gs = Gs::default();
                gs.set_threads(drawing_threads());
                for job in queue {
                    let started = std::time::Instant::now();
                    match job {
                        Job::Gif(path, data) => gs.gif(path, data),
                        Job::Call(f) => {
                            // Whoever looks at the GS sees everything drawn.
                            gs.flush();
                            f(&mut gs);
                        }
                    }
                    gs.busy += started.elapsed();
                }
            })
            .ok();
        Self {
            jobs: Some(jobs),
            worker,
            scanner: Scanner::default(),
            dumping: false,
            waited: std::time::Duration::ZERO,
        }
    }
}

impl Threaded {
    fn send(&mut self, job: Job) {
        if let Some(jobs) = &self.jobs {
            // A send fails only when the thread has died, which `with`
            // reports.
            if let Err(mpsc::TrySendError::Full(job)) = jobs.try_send(job) {
                let started = std::time::Instant::now();
                let _ = jobs.send(job);
                self.waited += started.elapsed();
            }
        }
    }

    /// Takes the events the packets sent since the last call asked for.
    pub fn take_events(&mut self) -> Events {
        self.scanner.take_events()
    }

    /// Runs `f` on the GS once the packets sent before have reached it,
    /// and gives its result.
    ///
    /// # Panics
    ///
    /// If the GS's thread has died (it panicked).
    pub fn with<R: Send + 'static>(&mut self, f: impl FnOnce(&mut Gs) -> R + Send + 'static) -> R {
        let (reply, result) = mpsc::sync_channel(1);
        self.send(Job::Call(Box::new(move |gs| {
            let _ = reply.send(f(gs));
        })));
        let Ok(r) = result.recv() else {
            panic!("the GS thread has stopped");
        };
        r
    }

    /// Runs `f` on the GS in turn with the packets, without waiting.
    pub fn post(&mut self, f: impl FnOnce(&mut Gs) + Send + 'static) {
        self.send(Job::Call(Box::new(f)));
    }

    /// Starts a PCSX2 dump (see [`crate::dump`]).
    pub fn start_dump(&mut self, privileged: Box<[u8; crate::dump::PRIV_SIZE]>) {
        self.dumping = true;
        self.post(move |gs| gs.start_dump(&privileged));
    }

    /// Whether a dump is being written.
    #[must_use]
    pub const fn dumping(&self) -> bool {
        self.dumping
    }

    /// Records a V-sync in the dump.
    pub fn dump_vsync(&mut self, field: u8, privileged: Box<[u8; crate::dump::PRIV_SIZE]>) {
        self.post(move |gs| gs.dump_vsync(field, &privileged));
    }

    /// Ends the dump and gives its bytes.
    pub fn finish_dump(&mut self) -> Option<Vec<u8>> {
        self.dumping = false;
        self.with(Gs::finish_dump)
    }
}

impl Sink for Threaded {
    fn gif(&mut self, path: usize, data: Vec<u128>) {
        self.scanner.scan(path, &data);
        self.send(Job::Gif(path, data));
    }
}

impl Drop for Threaded {
    fn drop(&mut self) {
        // Closing the queue ends the thread once it has drawn everything.
        self.jobs = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Psm;

    fn tag(nloop: u32, eop: bool, flg: u32, regs: &[u8]) -> u128 {
        let lo = u64::from(nloop)
            | (u64::from(eop) << 15)
            | (u64::from(flg) << 58)
            | ((regs.len() as u64 & 0xF) << 60);
        let hi = regs
            .iter()
            .enumerate()
            .fold(0u64, |h, (i, &r)| h | (u64::from(r) << (4 * i)));
        u128::from(lo) | (u128::from(hi) << 64)
    }

    #[test]
    fn the_scanner_finds_what_the_gs_does() {
        let ad = |reg: u8, v: u64| u128::from(v) | (u128::from(reg) << 64);
        let packets = [
            // REGLIST with an odd count, an IMAGE, then PACKED A+D with
            // SIGNAL and LABEL split over two sends.
            vec![
                tag(1, false, 1, &[0x1, 0x3, 0x1]),
                1,
                2,
                tag(2, false, 2, &[]),
                7,
                8,
            ],
            vec![tag(3, true, 0, &[0xE]), ad(0x60, 0x1234), ad(0x3B, 0x80)],
            vec![ad(0x62, 0x55)],
        ];
        let mut gs = Gs::default();
        let mut scanner = Scanner::default();
        for p in &packets {
            gs.gif(3, p.clone());
            scanner.scan(3, p);
        }
        let events = scanner.take_events();
        assert_eq!(events, gs.take_events());
        assert_eq!((events.signal, events.label), (Some(0x1234), Some(0x55)));
    }

    #[test]
    fn the_threaded_gs_draws_in_order() {
        let mut gs = Threaded::default();
        gs.post(|gs| gs.mem.write(Psm::Ct32, 0, 1, 3, 4, 0x1122_3344));
        let ad = |reg: u8, v: u64| u128::from(v) | (u128::from(reg) << 64);
        Sink::gif(&mut gs, 3, vec![tag(1, true, 0, &[0xE]), ad(0x61, 0)]);
        assert!(gs.take_events().finish);
        assert_eq!(
            gs.with(|gs| gs.mem.read(Psm::Ct32, 0, 1, 3, 4)),
            0x1122_3344
        );
    }
}
