//! VIF1 and VU1 on a thread of their own, with the GS behind them.
//!
//! The EE runs a shadow of VIF1 ([`ps2_vu::Vif::shadow`]): it reads the
//! same codes and data, so VIF1's registers, its stalls and interrupts and
//! the end of every DMA transfer are known on the EE's side at once,
//! exactly as when VIF1 ran there; none of them depends on what VU1 does.
//! The words it reads are handed, in order, to this thread, where the real
//! VIF1 runs them: VU1's programs, the micro code and data they load, and
//! the packets they kick to the GS. Everything else bound for the GS (PATH3,
//! pictures, dumps) goes through the same queue, so the GS gets it all in
//! the order the EE sent it.
//!
//! What the EE sees of VU1's work, it sees at fixed points of its own
//! time, so nothing depends on how fast the threads run:
//!
//! - the events GS packets ask for (SIGNAL, FINISH, LABEL): the EE looks
//!   for them itself in PATH2 (DIRECT, through the shadow) and PATH3, and
//!   raises them when the transfer ends, as before; PATH1's packets are
//!   built by VU1, so this thread looks for theirs and the EE raises them at
//!   the first V-blank start after the transfer ends (Sly 1's packets never
//!   ask for any there);
//! - VU1's memories and registers: the EE's loads and stores, state hashes
//!   and comparisons wait for everything sent before them.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ps2_gs::Events;
use ps2_gs::thread::{Scanner, Sink, Threaded};

/// VU1's memories: micro (code) and data, 16 KiB each.
const MEMORY: usize = 16 << 10;
/// Jobs the EE may be ahead of the thread by.
const QUEUE: usize = 4096;

/// What VU1's thread holds.
pub struct Vu1 {
    /// VIF1, the real one.
    pub vif: ps2_vu::Vif,
    /// VU1's registers.
    pub regs: ps2_vu::Regs,
    /// VU1's micro memory.
    pub code: Box<[u8]>,
    /// VU1's data memory.
    pub data: Box<[u8]>,
    /// Writes that may have changed the micro memory (for translations).
    pub code_writes: u64,
    /// The GS, on a thread of its own.
    pub gs: Threaded,
}

impl Vu1 {
    /// Runs VIF1 on the words it holds.
    fn run_vif(&mut self) {
        let mut mem = ps2_vu::Memories {
            code: &mut self.code,
            data: &mut self.data,
            code_writes: &mut self.code_writes,
        };
        self.vif
            .run(&mut self.regs, &mut mem, &mut Paths(&mut self.gs));
    }

    /// Runs the VU1 program at `address` (the EE started it with CMSAR1).
    pub fn call(&mut self, address: u32) {
        let (itop, top) = (self.vif.read(0xD0), self.vif.read(0xE0));
        let mut paths = Paths(&mut self.gs);
        ps2_vu::micro::run(
            &mut ps2_vu::micro::Unit {
                regs: &mut self.regs,
                code: &self.code,
                data: &mut self.data,
                vu1: Some(ps2_vu::micro::Vu1Link {
                    itop,
                    top,
                    gs: &mut paths,
                }),
                cache: &mut self.vif.cache,
                translations: self.vif.translations,
                code_writes: self.code_writes,
            },
            address,
        );
    }
}

/// PATH1 to the GS with its events looked for; PATH2 and PATH3 without,
/// the EE looking for theirs.
struct Paths<'a>(&'a mut Threaded);

impl Sink for Paths<'_> {
    fn gif(&mut self, path: usize, data: Vec<u128>) {
        if path == 1 {
            Sink::gif(self.0, path, data);
        } else {
            self.0.gif_unscanned(path, data);
        }
    }
}

/// A GS that takes nothing: for VIF0, which sends nothing to the GS.
pub struct Nowhere;

impl Sink for Nowhere {
    fn gif(&mut self, _: usize, _: Vec<u128>) {}
}

/// Work for VU1's thread.
enum Job {
    /// VIF1 words, as the shadow read them.
    Vif(Vec<u32>),
    /// A write to VIF1's registers.
    VifWrite(u32, u32),
    /// PATH3 packets (their events already looked for).
    Path3(Vec<u128>),
    /// Anything else, in order with the rest.
    Call(Box<dyn FnOnce(&mut Vu1) + Send>),
}

/// Host time and work on VU1's thread, for reports.
#[derive(Default)]
pub struct Stats {
    /// Nanoseconds busy.
    pub busy: AtomicU64,
    /// Nanoseconds in VU1 programs (with what they send the GS).
    pub programs_time: AtomicU64,
    /// VU1 programs run.
    pub programs: AtomicU64,
    /// Instruction pairs they ran.
    pub pairs: AtomicU64,
    /// Nanoseconds waiting for room in the GS's queue.
    pub gs_waited: AtomicU64,
}

/// VU1's thread, as the EE sees it.
pub struct Remote {
    jobs: Option<mpsc::SyncSender<(u64, Job)>>,
    /// After every job: its number and the PATH1 events it gave.
    done: mpsc::Receiver<(u64, Events)>,
    worker: Option<JoinHandle<()>>,
    /// Jobs sent, and finished.
    sent: u64,
    finished: u64,
    /// PATH1 events given by jobs, not yet taken.
    path1: VecDeque<(u64, Events)>,
    /// Looks for the events of PATH2 (through the shadow VIF1) and PATH3.
    scanner: Scanner,
    dumping: bool,
    /// Host time the EE waited for the thread (room in its queue, or
    /// work before a look at VU1 or the GS).
    pub waited: Duration,
    /// The same by what the EE waited for: room in the queue, PATH1's
    /// events at V-blank, a look (reading VU1's memories, the GS).
    pub waits: [Duration; 3],
    /// The thread's own figures.
    pub stats: Arc<Stats>,
}

impl Default for Remote {
    fn default() -> Self {
        let (jobs, queue) = mpsc::sync_channel::<(u64, Job)>(QUEUE);
        let (finished, done) = mpsc::channel();
        let stats = Arc::new(Stats::default());
        let shared = Arc::clone(&stats);
        let worker = std::thread::Builder::new()
            .name("vu1".to_owned())
            .spawn(move || {
                let mut vu1 = Vu1 {
                    vif: ps2_vu::Vif::new(true),
                    regs: ps2_vu::reset_regs(),
                    code: vec![0; MEMORY].into_boxed_slice(),
                    data: vec![0; MEMORY].into_boxed_slice(),
                    code_writes: 0,
                    gs: Threaded::default(),
                };
                for (number, job) in queue {
                    let started = Instant::now();
                    let (programs, gs_waited) = (vu1.vif.run_time, vu1.gs.waited);
                    match job {
                        Job::Vif(words) => {
                            vu1.vif.push_vec(words);
                            vu1.run_vif();
                        }
                        Job::VifWrite(offset, value) => vu1.vif.write(offset, value),
                        Job::Path3(data) => vu1.gs.gif_unscanned(3, data),
                        Job::Call(f) => f(&mut vu1),
                    }
                    // What the job kicked to the GS goes to it now.
                    vu1.gs.hand_over();
                    let add = |at: &AtomicU64, d: Duration| {
                        at.fetch_add(d.as_nanos() as u64, Ordering::Relaxed);
                    };
                    add(&shared.busy, started.elapsed());
                    add(
                        &shared.programs_time,
                        vu1.vif.run_time.saturating_sub(programs),
                    );
                    add(&shared.gs_waited, vu1.gs.waited.saturating_sub(gs_waited));
                    shared.programs.store(vu1.vif.programs, Ordering::Relaxed);
                    shared.pairs.store(vu1.vif.pairs, Ordering::Relaxed);
                    if finished.send((number, vu1.gs.take_events())).is_err() {
                        return;
                    }
                }
            })
            .ok();
        Self {
            jobs: Some(jobs),
            done,
            worker,
            sent: 0,
            finished: 0,
            path1: VecDeque::new(),
            scanner: Scanner::default(),
            dumping: false,
            waited: Duration::ZERO,
            waits: [Duration::ZERO; 3],
            stats,
        }
    }
}

impl Remote {
    /// Queues `job`; returns its number.
    fn send(&mut self, job: Job) -> u64 {
        self.sent += 1;
        if let Some(jobs) = &self.jobs {
            // A send fails only when the thread has died, which `wait_for`
            // reports.
            if let Err(mpsc::TrySendError::Full(job)) = jobs.try_send((self.sent, job)) {
                let started = Instant::now();
                let _ = jobs.send(job);
                self.waited += started.elapsed();
                self.waits[0] += started.elapsed();
            }
        }
        self.collect(None);
        self.sent
    }

    /// Takes the thread's reports: those ready, or (with a `block`, the
    /// kind of wait it counts as) the next.
    fn collect(&mut self, block: Option<usize>) {
        let mut take = |(number, events): (u64, Events)| {
            self.finished = number;
            if events != Events::default() {
                self.path1.push_back((number, events));
            }
        };
        if let Some(why) = block {
            let started = Instant::now();
            let report = self.done.recv();
            self.waited += started.elapsed();
            self.waits[why] += started.elapsed();
            match report {
                Ok(report) => take(report),
                Err(mpsc::RecvError) => panic!("VU1's thread has stopped"),
            }
        }
        while let Ok(report) = self.done.try_recv() {
            take(report);
        }
    }

    /// Waits until job `number` is done, the wait counted as `why`.
    fn wait_for(&mut self, number: u64, why: usize) {
        while self.finished < number {
            self.collect(Some(why));
        }
    }

    /// VIF1 words the shadow read; returns the job's number.
    pub fn vif(&mut self, words: Vec<u32>) -> u64 {
        self.send(Job::Vif(words))
    }

    /// A write to VIF1's registers.
    pub fn vif_write(&mut self, offset: u32, value: u32) {
        self.send(Job::VifWrite(offset, value));
    }

    /// Runs `f` on VU1's thread, in order with the rest, without waiting.
    pub fn call(&mut self, f: impl FnOnce(&mut Vu1) + Send + 'static) {
        self.send(Job::Call(Box::new(f)));
    }

    /// Runs `f` on VU1's thread once everything sent before is done, and
    /// gives its result.
    ///
    /// # Panics
    ///
    /// If VU1's thread has stopped.
    pub fn look<R: Send + 'static>(&mut self, f: impl FnOnce(&mut Vu1) -> R + Send + 'static) -> R {
        let (sent, result) = mpsc::sync_channel(1);
        let number = self.send(Job::Call(Box::new(move |vu1| {
            let _ = sent.send(f(vu1));
        })));
        self.wait_for(number, 2);
        match result.recv() {
            Ok(value) => value,
            Err(mpsc::RecvError) => panic!("VU1's thread has stopped"),
        }
    }

    /// The PATH1 events of jobs up to `number`, once they are done.
    pub fn path1_events(&mut self, number: u64) -> Events {
        self.wait_for(number, 1);
        let mut all = Events::default();
        while self.path1.front().is_some_and(|(n, _)| *n <= number) {
            if let Some((_, e)) = self.path1.pop_front() {
                all.signal = e.signal.or(all.signal);
                all.label = e.label.or(all.label);
                all.finish |= e.finish;
            }
        }
        all
    }

    /// The thread's figures so far: busy, in VU1 programs, waiting for the
    /// GS; programs run and their pairs. Current once a look or `with` has
    /// waited for it.
    #[must_use]
    pub fn thread_times(&self) -> (Duration, Duration, Duration, u64, u64) {
        let ns = |at: &AtomicU64| Duration::from_nanos(at.load(Ordering::Relaxed));
        let s = &self.stats;
        (
            ns(&s.busy),
            ns(&s.programs_time),
            ns(&s.gs_waited),
            s.programs.load(Ordering::Relaxed),
            s.pairs.load(Ordering::Relaxed),
        )
    }

    /// Takes the events PATH2 and PATH3's packets asked for since the last
    /// call.
    pub fn take_events(&mut self) -> Events {
        self.scanner.take_events()
    }

    /// The scanner for PATH2, for the shadow VIF1's DIRECT packets.
    pub fn path2(&mut self) -> impl Sink + '_ {
        Path2(&mut self.scanner)
    }

    /// Runs `f` on the GS once everything sent before has reached it, and
    /// gives its result.
    pub fn with<R: Send + 'static>(
        &mut self,
        f: impl FnOnce(&mut ps2_gs::Gs) -> R + Send + 'static,
    ) -> R {
        self.look(move |vu1| vu1.gs.with(f))
    }

    /// Runs `f` on the GS after everything sent before, without waiting.
    pub fn post(&mut self, f: impl FnOnce(&mut ps2_gs::Gs) + Send + 'static) {
        self.call(move |vu1| vu1.gs.post(f));
    }

    /// Starts a GS dump (see [`Threaded::start_dump`]).
    pub fn start_dump(&mut self, privileged: Box<[u8; ps2_gs::dump::PRIV_SIZE]>) {
        self.dumping = true;
        self.call(move |vu1| vu1.gs.start_dump(privileged));
    }

    /// Whether a GS dump is being written.
    #[must_use]
    pub const fn dumping(&self) -> bool {
        self.dumping
    }

    /// A V-sync in the GS dump.
    pub fn dump_vsync(&mut self, field: u8, privileged: Box<[u8; ps2_gs::dump::PRIV_SIZE]>) {
        self.call(move |vu1| vu1.gs.dump_vsync(field, privileged));
    }

    /// Ends the GS dump and gives it.
    pub fn finish_dump(&mut self) -> Option<Vec<u8>> {
        self.dumping = false;
        self.look(|vu1| vu1.gs.finish_dump())
    }
}

impl Sink for Remote {
    /// PATH3 packets from the EE: their events looked for here.
    fn gif(&mut self, path: usize, data: Vec<u128>) {
        self.scanner.scan(path, &data);
        self.send(Job::Path3(data));
    }
}

/// The shadow VIF1's DIRECT packets: their events looked for, the packets
/// themselves left to the real VIF1.
struct Path2<'a>(&'a mut Scanner);

impl Sink for Path2<'_> {
    fn gif(&mut self, path: usize, data: Vec<u128>) {
        self.0.scan(path, &data);
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        // Closing the queue ends the thread once it has run everything.
        self.jobs = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
