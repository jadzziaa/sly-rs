//! Batching on a thread of its own, when only a tap (the GPU) draws.
//!
//! The GS thread parses packets and builds primitives; it hands them, with
//! transfers' pixels, CLUT loads and requests for pictures, in order and in
//! chunks, to a thread that batches them (footprints, conflicts, the order
//! of uploads and CLUT loads, as [`Batch`] does) and feeds the drawer. As
//! nothing it does then reads local memory here, the GS thread waits for
//! the batching thread only when something asks for local memory (a read
//! back, a copy, a dump) or for everything drawn ([`Gs::flush`]).
//!
//! When the CPU draws, the GS thread batches itself: CLUT loads and
//! pictures read local memory there, once the batches that write what they
//! read are drawn.

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{Batch, Item, Op, Region, Stats};
use crate::draw::Prim;
use crate::{Gs, Memory, Psm, export};

/// Operations handed over at once.
const CHUNK: usize = 256;
/// Chunks the GS thread may be ahead of the batching thread by.
const CHUNKS: usize = 64;

/// The GS's batching: here, on the GS thread, or on a thread of its own.
pub(crate) struct Batcher {
    place: Place,
    /// A tap is set; only the tap draws; there is a drawer (else the GS
    /// draws each primitive as it comes).
    tapped: bool,
    gpu_only: bool,
    batching: bool,
    /// What batching had done at the last [`Gs::flush`] (for reports).
    stats: Stats,
    /// Time the GS thread waited for the batching thread (for reports).
    waited: Duration,
}

enum Place {
    Here(Box<Batch>),
    There(Stage),
    /// Between [`Batcher::take`] and [`Batcher::put`].
    Moving,
}

/// The batching thread.
struct Stage {
    ops: mpsc::SyncSender<Vec<Op>>,
    /// Operations not yet handed over.
    pending: Vec<Op>,
    /// Chunks done with, for the next (fresh ones, freed on the other
    /// thread, cost the GS thread page faults).
    spare: mpsc::Receiver<Vec<Op>>,
    /// The thread, which gives the batch back when its queue closes.
    thread: JoinHandle<Box<Batch>>,
}

impl Stage {
    fn start(batch: Box<Batch>) -> Self {
        let (ops, todo) = mpsc::sync_channel::<Vec<Op>>(CHUNKS);
        let (done, spare) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("gs-batch".to_owned())
            .spawn(move || {
                let mut batch = batch;
                for mut chunk in todo {
                    let mut started = Instant::now();
                    for op in chunk.drain(..) {
                        if matches!(op, Op::Flush(_)) {
                            // Counted before the statistics go back.
                            batch.stats.busy += started.elapsed();
                            started = Instant::now();
                        }
                        batch.apply(op);
                    }
                    let _ = done.send(chunk);
                    batch.stats.busy += started.elapsed();
                }
                batch
            });
        match thread {
            Ok(thread) => Self {
                ops,
                pending: Vec::with_capacity(CHUNK),
                spare,
                thread,
            },
            Err(e) => panic!("starting the GS's batching thread: {e}"),
        }
    }
}

impl Batcher {
    /// Batching for a GS whose local memory is `mem`, drawing each
    /// primitive as it comes until [`Gs::set_threads`] or [`Gs::set_tap`].
    pub(crate) fn new(mem: std::sync::Arc<Memory>) -> Self {
        let mut batch = Box::<Batch>::default();
        batch.mem = mem;
        Self {
            place: Place::Here(batch),
            tapped: false,
            gpu_only: false,
            batching: false,
            stats: Stats::default(),
            waited: Duration::ZERO,
        }
    }

    /// Does `op`: at once here, or in turn on the batching thread.
    fn push(&mut self, op: Op) {
        match &mut self.place {
            Place::Here(batch) => batch.apply(op),
            Place::There(stage) => {
                stage.pending.push(op);
                if stage.pending.len() >= CHUNK {
                    self.send();
                }
            }
            Place::Moving => {}
        }
    }

    /// Hands the batching thread the operations not yet handed over.
    fn send(&mut self) {
        let Place::There(stage) = &mut self.place else {
            return;
        };
        if stage.pending.is_empty() {
            return;
        }
        let next = stage
            .spare
            .try_recv()
            .unwrap_or_else(|_| Vec::with_capacity(CHUNK));
        let chunk = std::mem::replace(&mut stage.pending, next);
        let sent = match stage.ops.try_send(chunk) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(chunk)) => {
                let started = Instant::now();
                let sent = stage.ops.send(chunk).is_ok();
                self.waited += started.elapsed();
                sent
            }
            Err(mpsc::TrySendError::Disconnected(_)) => false,
        };
        assert!(sent, "the GS's batching thread has stopped");
    }

    /// Draws everything handed over so far and takes the statistics.
    fn flush(&mut self) {
        if let Place::Here(batch) = &mut self.place {
            batch.flush();
            self.stats = batch.stats;
            return;
        }
        if !matches!(self.place, Place::There(_)) {
            return;
        }
        let (reply, stats) = mpsc::sync_channel(1);
        self.push(Op::Flush(reply));
        self.send();
        let started = Instant::now();
        let received = stats.recv();
        self.waited += started.elapsed();
        match received {
            Ok(stats) => self.stats = stats,
            Err(_) => assert!(
                std::thread::panicking(),
                "the GS's batching thread has stopped"
            ),
        }
    }

    /// The batch, from wherever it is, for [`Batcher::put`].
    fn take(&mut self) -> Box<Batch> {
        self.send();
        match std::mem::replace(&mut self.place, Place::Moving) {
            Place::Here(batch) => batch,
            Place::There(Stage { ops, thread, .. }) => {
                // Closing the queue ends the thread.
                drop(ops);
                match thread.join() {
                    Ok(batch) => batch,
                    Err(e) => std::panic::resume_unwind(e),
                }
            }
            Place::Moving => panic!("the GS's batch is being moved"),
        }
    }

    /// Puts the batch where it now belongs: on a thread of its own when
    /// only a tap draws.
    fn put(&mut self, batch: Box<Batch>) {
        self.tapped = batch.tapped;
        self.gpu_only = batch.gpu_only;
        self.batching = batch.drawer.is_some();
        self.stats = batch.stats;
        self.place = if self.gpu_only && self.batching {
            Place::There(Stage::start(batch))
        } else {
            Place::Here(batch)
        };
    }
}

impl Drop for Batcher {
    fn drop(&mut self) {
        if let Place::There(mut stage) = std::mem::replace(&mut self.place, Place::Moving) {
            let chunk = std::mem::take(&mut stage.pending);
            if !chunk.is_empty() {
                let _ = stage.ops.send(chunk);
            }
            drop(stage.ops);
            let _ = stage.thread.join();
        }
    }
}

impl Gs {
    /// Hands the GS's work to `tap` from now on (or to nothing), starting
    /// with local memory and the CLUT as they are. With `software` false
    /// the CPU no longer draws (or writes transfers' pixels): local memory
    /// here goes stale, and pictures, dumps and transfers that read it ask
    /// the tap ([`Gs::picture`], [`Gs::sync_memory`]); so does the CLUT,
    /// which the tap loads (going back to drawing on the CPU, it is right
    /// again from the next CLUT load). Batching then moves to a thread of
    /// its own.
    pub fn set_tap(&mut self, tap: Option<Box<dyn export::Tap>>, software: bool) {
        self.sync_memory();
        self.flush();
        let mut batch = self.batch.take();
        let _ = batch.stop();
        batch.tapped = tap.is_some();
        batch.gpu_only = !software && tap.is_some();
        batch.start_drawer(tap);
        if batch.tapped {
            batch.cluts.push(export::Clut::Set {
                version: self.clut_version,
                halves: Box::new(self.clut),
            });
            let uses = batch.no_uses();
            batch.hand_over(Item::Memory(self.mem.to_words()), uses);
        }
        self.batch.put(batch);
    }

    /// Draws with `threads` threads (1: one primitive after another, as it
    /// comes). The result is the same either way.
    ///
    /// # Panics
    ///
    /// If the threads cannot be started.
    pub fn set_threads(&mut self, threads: usize) {
        self.flush();
        let mut batch = self.batch.take();
        let tap = batch.stop();
        batch.pool = (threads > 1).then(|| {
            match rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .thread_name(|i| format!("gs-draw-{i}"))
                .build()
            {
                Ok(pool) => std::sync::Arc::new(pool),
                Err(e) => panic!("starting the GS's drawing threads: {e}"),
            }
        });
        batch.start_drawer(tap);
        self.batch.put(batch);
    }

    pub(crate) const fn tapped(&self) -> bool {
        self.batch.tapped
    }

    /// Only the tap draws: local memory here is stale, and the CLUT is
    /// loaded by the tap.
    pub(crate) const fn gpu_only(&self) -> bool {
        self.batch.gpu_only
    }

    /// Primitives are batched (else drawn as they come).
    pub(crate) const fn batching(&self) -> bool {
        self.batch.batching
    }

    /// Queues a primitive.
    pub(crate) fn queue(&mut self, prim: Prim) {
        self.batch.push(Op::Prim(prim));
    }

    /// Writes a transfer's pixels, into `region`: at once, or after what
    /// is queued that uses those blocks, in the drawer's order.
    pub(crate) fn upload(
        &mut self,
        psm: Psm,
        bp: u32,
        bw: u32,
        writes: Vec<(u32, u32, u32)>,
        region: Region,
    ) {
        if !self.batching() {
            for (x, y, value) in writes {
                self.mem.write(psm, bp, bw, x, y, value);
            }
            return;
        }
        self.batch.push(Op::Upload {
            psm,
            bp,
            bw,
            writes,
            region,
        });
    }

    /// Before the CLUT is loaded from `region` (the CLUT version already
    /// counted): what is queued that writes there is drawn first when the
    /// CPU draws; for a tap, `load` is handed over in turn.
    pub(crate) fn clut_load(&mut self, region: Region, load: Option<[u32; 16]>) {
        if self.batching() {
            self.batch.push(Op::Clut {
                region,
                version: self.clut_version,
                load,
            });
        }
    }

    /// Hands `then` the display's picture: at once from local memory when
    /// the CPU draws, else once the tap has drawn everything before it.
    pub fn show(
        &mut self,
        regs: &crate::Privileged,
        then: impl FnOnce(crate::Frame) + Send + 'static,
    ) {
        if !self.gpu_only() {
            self.flush();
            then(self.display(regs));
            return;
        }
        self.batch.push(Op::Show(*regs, Box::new(then)));
        self.batch.send();
    }

    /// The display's picture, waiting for it (see [`Gs::show`]).
    pub fn picture(&mut self, regs: &crate::Privileged) -> crate::Frame {
        let (sent, received) = mpsc::channel();
        self.show(regs, move |frame| {
            let _ = sent.send(frame);
        });
        self.flush();
        received.recv().unwrap_or_default()
    }

    /// When only the tap draws: brings local memory here up to date from
    /// it (for whatever reads local memory here).
    pub fn sync_memory(&mut self) {
        if !self.gpu_only() {
            return;
        }
        let (sent, received) = mpsc::channel();
        self.batch.push(Op::Read(sent));
        self.flush();
        if let Ok(words) = received.recv() {
            self.mem.load_words(&words);
        }
    }

    /// After local memory here changed otherwise than by drawing or
    /// transfers (a local-to-local copy): hands it to the tap again.
    pub(crate) fn resync_tap(&mut self) {
        if self.tapped() {
            self.tap_resyncs += 1;
            self.batch.push(Op::Memory(self.mem.to_words()));
        }
    }

    /// Draws everything queued: when it returns, local memory holds it (or
    /// the tap does).
    pub fn flush(&mut self) {
        self.batch.flush();
        let s = &self.batch.stats;
        (self.alone, self.batches, self.parallel_batches) =
            (s.alone, s.batches, s.parallel_batches);
    }

    /// Hands the batching thread, if there is one, what is queued for it:
    /// for when the GS thread has nothing else to do.
    pub(crate) fn hand_over_queued(&mut self) {
        self.batch.send();
    }

    /// The drawer's time on batches drawn on the pool, batches drawn one
    /// primitive after another, and uploads, as of the last
    /// [`Gs::flush`] (for reports).
    #[must_use]
    pub const fn drawer_times(&self) -> [Duration; 3] {
        self.batch.stats.times
    }

    /// Time batching has waited for the drawer, as of the last
    /// [`Gs::flush`] (for reports).
    #[must_use]
    pub const fn drawer_waits(&self) -> Duration {
        self.batch.stats.waited
    }

    /// The batching thread's time busy, as of the last [`Gs::flush`], and
    /// the GS thread's time waiting for it (for reports; zero without
    /// one).
    #[must_use]
    pub const fn batching_times(&self) -> (Duration, Duration) {
        (self.batch.stats.busy, self.batch.waited)
    }
}
