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

    /// [`Sink::gif`] for packets the sender keeps: a sink that copies them
    /// anyway takes them without the sender allocating.
    fn gif_slice(&mut self, path: usize, data: &[u128]) {
        self.gif(path, data.to_vec());
    }
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
    /// Quadwords of a tag that cannot carry an event (REGLIST, IMAGE, or
    /// PACKED without A+D) still to pass over; 0 for one walked quadword
    /// by quadword.
    skip: u32,
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
        let mut i = 0;
        while let Some(&q) = data.get(i) {
            i += 1;
            let Some(mut tag) = *slot else {
                let lo = q as u64;
                let nloop = (lo & 0x7FFF) as u32;
                let flg = ((lo >> 58) & 3) as u32;
                let nreg = match (lo >> 60) as u32 {
                    0 => 16,
                    n => n,
                };
                let regs = (q >> 64) as u64;
                let ad = flg == 0 && (0..nreg).any(|k| (regs >> (4 * k)) & 0xF == 0xE);
                let skip = match flg {
                    _ if ad => 0,
                    0 => nloop * nreg,
                    1 => (nloop * nreg).div_ceil(2),
                    _ => nloop,
                };
                *slot = (nloop > 0).then_some(Tag {
                    nloop,
                    nreg: if flg >= 2 { 1 } else { nreg },
                    regs,
                    flg: flg.min(2),
                    reg: 0,
                    skip,
                });
                continue;
            };
            if tag.skip > 0 {
                // This quadword and as many after it as the tag has left.
                let n = tag
                    .skip
                    .min(u32::try_from(data.len() - i + 1).unwrap_or(u32::MAX));
                tag.skip -= n;
                i += n as usize - 1;
                *slot = (tag.skip > 0).then_some(tag);
                continue;
            }
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
/// Quadwords of packets on one path gathered into one job before it is
/// sent.
const GATHER: usize = 4096;

/// Work for the GS's thread: packets, a call once everything before is
/// drawn, a call in turn.
enum Job {
    Gif(usize, Vec<u128>),
    Call(Box<dyn FnOnce(&mut Gs) + Send>),
    Post(Box<dyn FnOnce(&mut Gs) + Send>),
}

/// A GS on its own thread.
pub struct Threaded {
    jobs: Option<mpsc::SyncSender<Job>>,
    worker: Option<JoinHandle<()>>,
    scanner: Scanner,
    dumping: bool,
    /// Packets on one path not sent yet: consecutive packets go to the GS
    /// as one job (the GS reads a path's quadwords the same however they
    /// are split), until another job, [`Threaded::hand_over`] or
    /// [`GATHER`] quadwords.
    gathered: Option<(usize, Vec<u128>)>,
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
                loop {
                    let job = match queue.try_recv() {
                        Ok(job) => job,
                        Err(mpsc::TryRecvError::Empty) => {
                            // Nothing to do: the batching thread gets what
                            // is queued for it.
                            gs.hand_over_queued();
                            match queue.recv() {
                                Ok(job) => job,
                                Err(_) => break,
                            }
                        }
                        Err(mpsc::TryRecvError::Disconnected) => break,
                    };
                    let started = std::time::Instant::now();
                    match job {
                        Job::Gif(path, data) => gs.gif(path, data),
                        Job::Call(f) => {
                            // Whoever looks at the GS sees everything drawn.
                            gs.flush();
                            f(&mut gs);
                        }
                        Job::Post(f) => f(&mut gs),
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
            gathered: None,
            waited: std::time::Duration::ZERO,
        }
    }
}

impl Threaded {
    fn send(&mut self, job: Job) {
        self.hand_over();
        self.send_now(job);
    }

    fn send_now(&mut self, job: Job) {
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

    /// Packets on `path`: gathered with those before them on the same
    /// path, or sent at once while a dump is written (which records each
    /// transfer as it comes).
    fn packets(&mut self, path: usize, data: Vec<u128>) {
        if self.dumping {
            self.send(Job::Gif(path, data));
            return;
        }
        match &mut self.gathered {
            Some((p, gathered)) if *p == path => gathered.extend_from_slice(&data),
            _ => {
                self.hand_over();
                self.gathered = Some((path, data));
            }
        }
        self.hand_over_full();
    }

    /// [`Threaded::packets`] for packets the sender keeps.
    fn packets_slice(&mut self, path: usize, data: &[u128]) {
        match &mut self.gathered {
            Some((p, gathered)) if *p == path && !self.dumping => {
                gathered.extend_from_slice(data);
                self.hand_over_full();
            }
            _ => self.packets(path, data.to_vec()),
        }
    }

    /// Sends the packets gathered once they are [`GATHER`] quadwords.
    fn hand_over_full(&mut self) {
        if self
            .gathered
            .as_ref()
            .is_some_and(|(_, g)| g.len() >= GATHER)
        {
            self.hand_over();
        }
    }

    /// Sends the packets gathered so far.
    pub fn hand_over(&mut self) {
        if let Some((path, data)) = self.gathered.take() {
            self.send_now(Job::Gif(path, data));
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

    /// Runs `f` on the GS in turn with the packets, without waiting. What
    /// they queued may not be drawn yet when it runs: `f` flushes if it
    /// reads local memory (as [`Gs::show`] does when the CPU draws).
    pub fn post(&mut self, f: impl FnOnce(&mut Gs) + Send + 'static) {
        self.send(Job::Post(Box::new(f)));
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

impl Threaded {
    /// Sends packets on `path` without looking for their events: for a
    /// sender that scans that path itself (a path's packets must all go
    /// one way or the other).
    pub fn gif_unscanned(&mut self, path: usize, data: Vec<u128>) {
        self.packets(path, data);
    }
}

impl Sink for Threaded {
    fn gif(&mut self, path: usize, data: Vec<u128>) {
        self.scanner.scan(path, &data);
        self.packets(path, data);
    }

    fn gif_slice(&mut self, path: usize, data: &[u128]) {
        self.scanner.scan(path, data);
        self.packets_slice(path, data);
    }
}

impl Drop for Threaded {
    fn drop(&mut self) {
        // Closing the queue ends the thread once it has drawn everything.
        self.hand_over();
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
            // An IMAGE split over sends; a PACKED tag without A+D whose
            // data looks like a SIGNAL write, and one whose A+D nibble
            // lies past NREG.
            vec![tag(3, false, 2, &[]), 1],
            vec![
                2,
                ad(0x60, 0x9999),
                tag(2, false, 0, &[0x1]),
                ad(0x60, 0x7777),
            ],
            vec![
                ad(0x60, 0x6666),
                // NREG 1: the 0xE in the second nibble is not a register.
                (tag(1, false, 0, &[0x1, 0xE]) & !(0xF << 60)) | (1 << 60),
                ad(0x60, 0x5555),
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
