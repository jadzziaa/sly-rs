//! Drawing on several threads, with the same result as drawing one
//! primitive after another.
//!
//! Primitives are queued in a batch instead of drawn at once. When the batch
//! is drawn, each primitive is split into 32 × 32-pixel tiles of its frame;
//! tiles are drawn in parallel, each tile's primitives in the order they
//! came. A tile covers whole blocks of local memory in every format, so
//! tiles never share a word, and the order inside a tile is the only order
//! that matters, unless one primitive reads what another writes elsewhere.
//! So a batch ends (is drawn) before:
//! - a primitive that textures from blocks the batch writes, or writes
//!   blocks it textures from;
//! - a primitive whose depth test or depth writes touch blocks the batch
//!   writes as frame, or whose frame touches blocks the batch uses as
//!   depth;
//! - a primitive with another frame or depth buffer;
//! - an upload into blocks the batch uses, a CLUT load from blocks it
//!   writes, a copy between buffers, a read back;
//! - anything that reads local memory from outside (pictures, dumps).
//!
//! A primitive whose texture overlaps its own frame or the depth it writes,
//! that reaches past its frame's width (where pixels of one row share pages
//! with the next), or whose depth buffer, where it uses it, overlaps its
//! frame, is drawn alone, in order. One that textures from blocks it
//! writes reads its texture as those blocks were before it ([`Snapshot`]),
//! not as its own pixels leave them: the same whatever order its pixels
//! are drawn in (so a GPU can draw them at once), and nearer PCSX2's
//! pictures than drawing order (Sly's 64 × 64 sprite that textures from its
//! own frame).
//!
//! Batches go to a drawer thread, which draws them in order, each on the
//! pool of drawing threads or one primitive after another, while the GS
//! parses the next ones. Transfers' pixels go through the drawer too, in
//! order, coalesced; a CLUT load, a read back, a copy or a look at local
//! memory waits for what it depends on.
//!
//! When only a tap (the GPU) draws, this batching runs on a thread of its
//! own between the GS thread and the drawer ([`stage`]).
//!
//! Footprints are conservative: on each page row a rectangle covers, the
//! pages from its first column's to its last's. A texture's rectangle is
//! the texels the primitive can read ([`Prim::texels`]); a depth buffer
//! counts only where the depth test compares or depth is written.

mod stage;

use std::collections::VecDeque;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use rayon::prelude::*;

use crate::draw::{Prim, Raster};
use crate::export;
use crate::mem::Pixel;
use crate::mem::page_shape;
use crate::regs::{Tex0, bits};
use crate::transfer::Upload;
use crate::{Memory, Psm};

pub(crate) use stage::Batcher;

/// Blocks of local memory.
const BLOCKS: u32 = 16384;
/// Tile size, in pixels.
const TILE: i32 = 32;
/// Tiles across (and down) the GS's 2048 × 2048 coordinate space.
const TILES: usize = 2048 / TILE as usize;
/// Pixels a batch must cover before it is worth spreading over threads.
const PARALLEL_MIN: u64 = 4 * 1024;
/// Primitives a batch holds at most.
const MAX_PRIMS: usize = 16 * 1024;

/// Blocks of local memory: `rows` runs of `span` blocks, the first at
/// `start`, each `stride` blocks after the one before (all modulo the
/// memory's size). A rectangle of a buffer covers, on each page row, the
/// pages from its left column's to its right column's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    start: u32,
    span: u32,
    stride: u32,
    rows: u32,
}

impl Region {
    /// All of local memory.
    pub(crate) const ALL: Self = Self::run(0, BLOCKS);

    /// Blocks `start .. start + count`.
    const fn run(start: u32, count: u32) -> Self {
        Self {
            start,
            span: count,
            stride: 0,
            rows: 1,
        }
    }

    /// The blocks a rectangle (x0, x1, y0, y1, inclusive) of a buffer may
    /// touch: on each of its page rows, the pages from its first column's to
    /// its last's.
    pub(crate) fn of(psm: Psm, bp: u32, bw: u32, rect: [i32; 4]) -> Self {
        let (w_shift, h_shift, pages_per_row) = page_shape(psm, bw);
        let [x0, x1, y0, y1] = rect.map(|c| c.max(0) as u32);
        let (x0, x1) = (x0.min(x1), x0.max(x1));
        let (y0, y1) = (y0.min(y1), y0.max(y1));
        let (r0, r1) = (y0 >> h_shift, y1 >> h_shift);
        let (p0, p1) = (x0 >> w_shift, x1 >> w_shift);
        let first = r0 * pages_per_row + p0;
        let start = (bp + first * 32) % BLOCKS;
        let span = (p1 - p0 + 1) * 32;
        let stride = pages_per_row * 32;
        if r0 == r1 || span >= stride {
            // One run: from the first row's first page to the last's last.
            let last = r1 * pages_per_row + p1;
            return Self::run(start, ((last - first + 1) * 32).min(BLOCKS));
        }
        Self {
            start,
            span,
            stride,
            rows: r1 - r0 + 1,
        }
    }

    /// The region as ranges that do not wrap.
    fn ranges(self) -> impl Iterator<Item = (u32, u32)> {
        (0..self.rows).flat_map(move |r| {
            let start = (self.start + r * self.stride) % BLOCKS;
            let end = start + self.span.min(BLOCKS);
            let first = (start, end.min(BLOCKS));
            let second = (end > BLOCKS).then(|| (0, end - BLOCKS));
            std::iter::once(first).chain(second)
        })
    }

    fn overlaps(self, other: Self) -> bool {
        self.ranges()
            .any(|(a0, a1)| other.ranges().any(|(b0, b1)| a0 < b1 && b0 < a1))
    }
}

/// A set of blocks.
struct Blocks {
    bits: Box<[u64; (BLOCKS / 64) as usize]>,
    empty: bool,
}

impl Default for Blocks {
    fn default() -> Self {
        Self {
            bits: Box::new([0; (BLOCKS / 64) as usize]),
            empty: true,
        }
    }
}

impl Blocks {
    fn words(r: (u32, u32)) -> impl Iterator<Item = (usize, u64)> {
        let (from, to) = r;
        (from / 64..to.div_ceil(64)).map(move |w| {
            let lo = (w * 64).max(from) - w * 64;
            let hi = ((w + 1) * 64).min(to) - w * 64;
            let mask = if hi - lo == 64 {
                u64::MAX
            } else {
                ((1u64 << (hi - lo)) - 1) << lo
            };
            (w as usize, mask)
        })
    }

    fn mark(&mut self, region: Region) {
        for r in region.ranges() {
            for (w, mask) in Self::words(r) {
                self.bits[w] |= mask;
            }
        }
        self.empty = false;
    }

    fn hits(&self, region: Region) -> bool {
        !self.empty
            && region
                .ranges()
                .any(|r| Self::words(r).any(|(w, mask)| self.bits[w] & mask != 0))
    }

    fn clear(&mut self) {
        if !self.empty {
            self.bits.fill(0);
            self.empty = true;
        }
    }

    /// Takes block `block` out.
    fn unmark(&mut self, block: u32) {
        self.bits[block as usize / 64] &= !(1 << (block % 64));
    }
}

/// What a batch draws into: the frame and the depth buffer.
type Target = ((Psm, u32, u32), Option<(Psm, u32)>);

/// What a batch's primitives use: blocks written as frame, written as
/// depth, used as depth (tested or written), read as texture.
#[derive(Default)]
struct Uses {
    frame: Blocks,
    depth_written: Blocks,
    depth: Blocks,
    texture: Blocks,
}

impl Uses {
    /// Whether a primitive with this footprint must wait for these uses: it
    /// reads what they write or writes what they read, other than the same
    /// frame or depth pixels, which stay in one tile.
    fn conflict(&self, f: &Footprint) -> bool {
        let written = |r: Region| self.frame.hits(r) || self.depth_written.hits(r);
        f.texture.is_some_and(written)
            || self.texture.hits(f.frame)
            || self.depth.hits(f.frame)
            || f.depth
                .is_some_and(|d| self.frame.hits(d) || (f.depth_written && self.texture.hits(d)))
    }

    fn mark(&mut self, f: &Footprint) {
        self.frame.mark(f.frame);
        if let Some(d) = f.depth {
            self.depth.mark(d);
            if f.depth_written {
                self.depth_written.mark(d);
            }
        }
        if let Some(t) = f.texture {
            self.texture.mark(t);
        }
    }

    /// Whether an upload into `region` must wait.
    fn uploads_conflict(&self, region: Region) -> bool {
        [&self.frame, &self.depth, &self.texture]
            .iter()
            .any(|set| set.hits(region))
    }

    /// Whether reading `region` (a CLUT load) must wait.
    fn reads_conflict(&self, region: Region) -> bool {
        self.frame.hits(region) || self.depth_written.hits(region)
    }

    fn clear(&mut self) {
        for set in [
            &mut self.frame,
            &mut self.depth_written,
            &mut self.depth,
            &mut self.texture,
        ] {
            set.clear();
        }
    }
}

/// A batch's primitives and the buffers drawing them takes, which go to
/// the drawing threads and come back.
#[derive(Default)]
struct Work {
    prims: Vec<Prim>,
    tiles: Vec<Vec<u32>>,
    /// Drawn one primitive after another (a primitive drawn alone).
    serial: bool,
    /// For a primitive drawn alone that textures from blocks it writes:
    /// its texture's blocks, taken before it is drawn.
    snapshot: Option<Region>,
    pixels: u64,
    /// Local memory, while the batch is with the drawer (it draws there).
    memory: Option<Arc<Memory>>,
}

/// Batches the drawer may hold, drawn or waiting, before the GS waits for
/// the oldest.
const DEPTH: usize = 16;
/// Transfer pixels held before they are handed to the drawer.
const UPLOAD_MAX: usize = 1 << 18;

/// Primitives waiting to be drawn and what they use, and the batches handed
/// to the drawer, which draws them in order (each on the pool or one
/// primitive after another) while the GS parses on.
#[derive(Default)]
pub(crate) struct Batch {
    pool: Option<Arc<rayon::ThreadPool>>,
    work: Work,
    uses: Uses,
    target: Option<Target>,
    drawer: Option<Drawer>,
    /// Batches handed to the drawer, oldest first: number and uses.
    drawing: VecDeque<(u64, Uses)>,
    /// Batches handed over and drawn so far.
    submitted: u64,
    drawn: u64,
    /// Transfers' pixels not yet handed over, with the blocks they write.
    /// They come before the primitives queued: those that read them were
    /// queued after them, and the others do not touch them.
    upload: Option<(Upload, Uses)>,
    /// The footprint marked last in `uses`. A primitive with the same one
    /// needs no check and no marking: every primitive marked since passed
    /// the check against it (the check is symmetric), and marking again
    /// changes nothing.
    marked: Option<Footprint>,
    /// Buffers of drawn batches, for the next.
    spare_work: Vec<Work>,
    spare_uses: Vec<Uses>,
    /// Local memory, where the CPU draws.
    mem: Arc<Memory>,
    /// What has been done so far (for reports).
    stats: Stats,
    /// A tap is set (it lives on the drawer's thread).
    tapped: bool,
    /// Only the tap draws (not the CPU).
    gpu_only: bool,
    /// CLUT changes for the tap, handed over before the next item.
    cluts: Vec<export::Clut>,
    /// The CLUT version of the batch's first primitive.
    first_clut: Option<u32>,
    /// With a tap: blocks drawn into since a transfer last wrote them.
    frames: Blocks,
}

/// What batching has done so far (for reports).
#[derive(Clone, Copy, Default)]
pub(crate) struct Stats {
    /// Primitives drawn alone, batches, and batches drawn on the pool.
    pub(crate) alone: u64,
    pub(crate) batches: u64,
    pub(crate) parallel_batches: u64,
    /// The drawer's time on batches drawn on the pool, batches drawn one
    /// primitive after another, and everything else (uploads, the tap's
    /// other work).
    pub(crate) times: [Duration; 3],
    /// Time spent waiting for the drawer.
    pub(crate) waited: Duration,
    /// On a batching thread of its own: its time on what the GS handed it,
    /// waits for the drawer included.
    pub(crate) busy: Duration,
}

/// What the drawer does, in order: draw a batch (on the pool or not),
/// write a transfer's pixels, or for a tap, hand it local memory, read
/// out the display or read local memory back.
enum Item {
    Batch(Work, bool),
    Upload(Upload, Arc<Memory>),
    Memory(Vec<u32>),
    Display(crate::Privileged, Box<dyn FnOnce(crate::Frame) + Send>),
    Read(mpsc::Sender<Vec<u32>>),
}

/// The thread that draws batches in order.
/// The tap, as it lives on the drawer's thread.
type Tap = Option<Box<dyn export::Tap>>;

struct Drawer {
    /// Items, each after the CLUT changes before it.
    batches: mpsc::SyncSender<(Vec<export::Clut>, Item)>,
    /// Each item's end, with a batch's buffers for the next, what kind of
    /// item it was (see [`Batch::times`]) and how long it took.
    done: mpsc::Receiver<(Option<Work>, usize, Duration)>,
    /// The thread, which gives the tap back when it ends.
    thread: Option<std::thread::JoinHandle<Tap>>,
}

impl Drawer {
    /// The drawer: drawing on the CPU (on `pool` when a batch is worth it)
    /// if `software`, and handing everything to `tap` if there is one.
    fn start(pool: Option<Arc<rayon::ThreadPool>>, mut tap: Tap, software: bool) -> Option<Self> {
        let (batches, todo) = mpsc::sync_channel::<(Vec<export::Clut>, Item)>(DEPTH);
        let (finished, done) = mpsc::sync_channel(DEPTH);
        let thread = std::thread::Builder::new()
            .name("gs-drawer".to_owned())
            .spawn(move || {
                for (cluts, item) in todo {
                    let started = std::time::Instant::now();
                    if let Some(tap) = &mut tap
                        && !cluts.is_empty()
                    {
                        tap.cluts(&cluts);
                    }
                    let (back, kind) = match item {
                        Item::Batch(mut work, parallel) => {
                            let mem = work.memory.take().unwrap_or_default();
                            if software {
                                match &pool {
                                    Some(pool) if parallel => {
                                        pool.install(|| draw(&mem, &mut work, true));
                                    }
                                    _ => draw(&mem, &mut work, false),
                                }
                            }
                            if let Some(tap) = &mut tap {
                                let scale = tap.scale();
                                tap.batch(&crate::draw::encode::batch(
                                    &work.prims,
                                    work.snapshot.is_some(),
                                    scale,
                                ));
                            }
                            (Some(work), usize::from(!parallel))
                        }
                        Item::Upload(upload, mem) => {
                            if software {
                                upload.apply(&mem);
                            }
                            if let Some(tap) = &mut tap {
                                tap.writes(&upload.word_writes());
                            }
                            (None, 2)
                        }
                        Item::Memory(words) => {
                            if let Some(tap) = &mut tap {
                                tap.memory(&words);
                            }
                            (None, 2)
                        }
                        Item::Display(regs, then) => {
                            if let Some(tap) = &mut tap {
                                then(tap.display(&regs));
                            }
                            (None, 2)
                        }
                        Item::Read(back) => {
                            if let Some(tap) = &mut tap {
                                let _ = back.send(tap.read());
                            }
                            (None, 2)
                        }
                    };
                    if finished.send((back, kind, started.elapsed())).is_err() {
                        break;
                    }
                }
                tap
            })
            .ok()?;
        Some(Self {
            batches,
            done,
            thread: Some(thread),
        })
    }
}

impl Batch {
    /// Waits until batch `n` (counted from 1) has been drawn.
    fn wait_for(&mut self, n: u64) {
        while self.drawn < n {
            let Some(drawer) = &self.drawer else {
                return;
            };
            let started = std::time::Instant::now();
            let received = drawer.done.recv();
            self.stats.waited += started.elapsed();
            let Ok((back, kind, took)) = received else {
                // The drawer panicked: memory is not what it should be.
                assert!(std::thread::panicking(), "the GS's drawer has stopped");
                return;
            };
            self.drawn += 1;
            self.stats.times[kind] += took;
            if let Some(mut work) = back {
                work.prims.clear();
                work.serial = false;
                work.snapshot = None;
                work.pixels = 0;
                self.spare_work.push(work);
            }
            if let Some((_, mut uses)) = self.drawing.pop_front() {
                uses.clear();
                self.spare_uses.push(uses);
            }
        }
    }

    /// Gives the drawer the transfers' pixels not yet handed over.
    fn hand_over_upload(&mut self) {
        if let Some((upload, uses)) = self.upload.take() {
            self.hand_over(Item::Upload(upload, Arc::clone(&self.mem)), uses);
        }
    }

    /// Gives the drawer `item`, which uses `uses`, after the CLUT changes
    /// so far.
    fn hand_over(&mut self, item: Item, uses: Uses) {
        if self.drawing.len() >= DEPTH {
            self.wait_for(self.submitted + 1 - DEPTH as u64);
        }
        let Some(drawer) = &self.drawer else {
            return;
        };
        let cluts = std::mem::take(&mut self.cluts);
        assert!(
            drawer.batches.send((cluts, item)).is_ok(),
            "the GS's drawer has stopped"
        );
        self.submitted += 1;
        self.drawing.push_back((self.submitted, uses));
    }

    /// Waits until every batch handed over has been drawn.
    fn wait(&mut self) {
        self.wait_for(self.submitted);
    }

    /// Waits for the batches handed over that `conflict` says matter.
    fn wait_if(&mut self, conflict: impl Fn(&Uses) -> bool) {
        if let Some(&(n, _)) = self.drawing.iter().rev().find(|(_, uses)| conflict(uses)) {
            self.wait_for(n);
        }
    }

    /// Waits for everything handed over, then stops the drawer, giving
    /// back its tap.
    fn stop(&mut self) -> Tap {
        self.wait();
        let mut drawer = self.drawer.take()?;
        let thread = drawer.thread.take();
        // Closing the queue ends the drawer.
        drop(drawer);
        thread.and_then(|t| t.join().ok()).flatten()
    }

    /// A batch for an item that is not one: spare uses.
    fn no_uses(&mut self) -> Uses {
        self.spare_uses.pop().unwrap_or_default()
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// The blocks a primitive reads (its texture, the depth it tests) and
/// writes (frame, depth).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Footprint {
    frame: Region,
    /// The depth buffer, if the primitive uses it at all.
    depth: Option<Region>,
    /// Whether it writes the depth buffer.
    depth_written: bool,
    texture: Option<Region>,
}

fn footprint(prim: &Prim) -> Footprint {
    let p = &prim.pipe;
    let (fpsm, fbp, fbw) = p.frame;
    let texture = p.tex_psm.filter(|_| p.textured()).map(|psm| {
        // The texels it reads, or every texel it could address.
        let rect = prim.texels().unwrap_or([0, 0x7FF, 0, 0x7FF]);
        Region::of(psm, p.tex.tbp, p.tex.tbw, rect)
    });
    let (depth_read, depth_written) = p.depth_use();
    Footprint {
        frame: Region::of(fpsm, fbp, fbw, prim.bbox),
        depth: p
            .zbuf
            .filter(|_| depth_read || depth_written)
            .map(|(zpsm, zbp)| Region::of(zpsm, zbp, fbw, prim.bbox)),
        depth_written,
        texture,
    }
}

/// Blocks of local memory as they were at some moment: what a primitive
/// that textures from blocks it writes reads its texture from.
pub(crate) struct Snapshot {
    /// Each block's place in `words`, or `u32::MAX` if it was not taken.
    index: Vec<u32>,
    words: Vec<u32>,
}

impl Snapshot {
    /// The blocks of `region`, as `mem` holds them now.
    pub(crate) fn of(mem: &Memory, region: Region) -> Self {
        let mut index = vec![u32::MAX; BLOCKS as usize];
        let mut words = Vec::new();
        for (from, to) in region.ranges() {
            for block in from..to {
                if index[block as usize] == u32::MAX {
                    index[block as usize] = (words.len() / 64) as u32;
                    let first = block as usize * 64;
                    words.extend((first..first + 64).map(|w| mem.word(w)));
                }
            }
        }
        Self { index, words }
    }

    /// The pixel at `p` as it was, or as `mem` holds it if its block was not
    /// taken.
    #[inline]
    pub(crate) fn get(&self, mem: &Memory, p: Pixel) -> u32 {
        let (word, block) = (p.word(), p.word() / 64);
        match self.index[block] {
            u32::MAX => mem.get(p),
            at => p.of(self.words[at as usize * 64 + word % 64]),
        }
    }
}

/// The blocks a primitive's texture takes up, if it textures from blocks
/// it writes (its frame, or the depth it writes).
pub(crate) fn own_texture(prim: &Prim) -> Option<Region> {
    let f = footprint(prim);
    f.texture.filter(|&t| reads_own_writes(&f, t))
}

/// Whether texture `t` overlaps what footprint `f` writes.
fn reads_own_writes(f: &Footprint, t: Region) -> bool {
    t.overlaps(f.frame) || f.depth.is_some_and(|d| f.depth_written && t.overlaps(d))
}

/// Draws a batch's primitives: split into tiles drawn in parallel, or one
/// after another.
fn draw(mem: &Memory, work: &mut Work, parallel: bool) {
    let raster = Raster { mem, tex: None };
    if !parallel {
        let snapshot = work.snapshot.map(|r| Snapshot::of(mem, r));
        let raster = Raster {
            mem,
            tex: snapshot.as_ref(),
        };
        for prim in &work.prims {
            raster.prim(prim, prim.bbox);
        }
        return;
    }
    if work.tiles.is_empty() {
        work.tiles = vec![Vec::new(); TILES * TILES];
    }
    let mut used = Vec::new();
    for (i, prim) in work.prims.iter().enumerate() {
        let [x0, x1, y0, y1] = prim.bbox.map(|c| c / TILE);
        for ty in y0..=y1 {
            for tx in x0..=x1 {
                let t = ty as usize * TILES + tx as usize;
                if work.tiles[t].is_empty() {
                    used.push(t);
                }
                work.tiles[t].push(i as u32);
            }
        }
    }
    let (prims, tiles) = (&work.prims, &work.tiles);
    used.par_iter().for_each(|&t| {
        let (tx, ty) = ((t % TILES) as i32 * TILE, (t / TILES) as i32 * TILE);
        for &i in &tiles[t] {
            let prim = &prims[i as usize];
            let [x0, x1, y0, y1] = prim.bbox;
            let clip = [
                x0.max(tx),
                x1.min(tx + TILE - 1),
                y0.max(ty),
                y1.min(ty + TILE - 1),
            ];
            raster.prim(prim, clip);
        }
    });
    for t in used {
        work.tiles[t].clear();
    }
}

/// Transfers' pixels (runs into `psm` buffers at `bp`, `bw` wide) as writes
/// to words of local memory, each word once (the last write to a pixel
/// wins, as in memory).
pub(crate) fn word_writes<'a>(
    runs: impl IntoIterator<Item = (Psm, u32, u32, &'a [(u32, u32, u32)])>,
) -> Vec<export::Write> {
    thread_local! {
        /// Each word's place in the writes plus one, 0 for none; only the
        /// words written are set, and cleared again.
        static PLACE: std::cell::RefCell<Vec<u32>> = std::cell::RefCell::new(vec![0; 1 << 20]);
    }
    PLACE.with_borrow_mut(|place| {
        let mut out: Vec<export::Write> = Vec::new();
        for (psm, bp, bw, writes) in runs {
            let layout = crate::Layout::new(psm, bp, bw);
            for &(x, y, value) in writes {
                let at = layout.locate(x, y);
                let (bits, new) = at.bits(value);
                let word = at.word();
                match place[word] {
                    0 => {
                        out.push([word as u32, bits, new]);
                        place[word] = out.len() as u32;
                    }
                    i => {
                        let w = &mut out[i as usize - 1];
                        w[1] |= bits;
                        w[2] = (w[2] & !bits) | new;
                    }
                }
            }
        }
        for w in &out {
            place[w[0] as usize] = 0;
        }
        out
    })
}

/// The blocks a CLUT load reads, by TEX0 and TEXCLUT.
pub(crate) fn clut_region(t: Tex0, texclut: u64) -> Region {
    if t.csm {
        let (cbw, cou, cov) = (
            bits(texclut, 0, 6),
            bits(texclut, 6, 6) as i32,
            bits(texclut, 12, 10) as i32,
        );
        Region::of(Psm::Ct16, t.cbp, cbw, [cou * 16, cou * 16 + 255, cov, cov])
    } else {
        let psm = if t.cpsm < 2 { Psm::Ct32 } else { Psm::Ct16 };
        Region::of(psm, t.cbp, 1, [0, 15, 0, 15])
    }
}

/// What the GS hands its batching, in order.
pub(crate) enum Op {
    Prim(Prim),
    /// A transfer's pixels into a `psm` buffer at `bp`, `bw` wide, all
    /// inside `region`.
    Upload {
        psm: Psm,
        bp: u32,
        bw: u32,
        writes: Vec<(u32, u32, u32)>,
        region: Region,
    },
    /// The CLUT is loaded from `region`: for a tap, `load`, which makes
    /// it version `version`.
    Clut {
        region: Region,
        version: u32,
        load: Option<[u32; 16]>,
    },
    /// For a tap: the display's picture, once it has drawn everything
    /// before.
    Show(crate::Privileged, Box<dyn FnOnce(crate::Frame) + Send>),
    /// For a tap: local memory as it has it.
    Read(mpsc::Sender<Vec<u32>>),
    /// For a tap: all of local memory again.
    Memory(Vec<u32>),
    /// Everything before drawn; then what has been done.
    Flush(mpsc::SyncSender<Stats>),
}

impl Batch {
    /// Does `op`.
    fn apply(&mut self, op: Op) {
        match op {
            Op::Prim(prim) => self.queue(prim),
            Op::Upload {
                psm,
                bp,
                bw,
                writes,
                region,
            } => self.upload(psm, bp, bw, writes, region),
            Op::Clut {
                region,
                version,
                load,
            } => {
                self.before_clut_load(region);
                if let Some(words) = load {
                    self.clut_loaded(export::Clut::Load(words), version);
                }
            }
            Op::Show(regs, then) => {
                self.submit();
                let uses = self.no_uses();
                self.hand_over(Item::Display(regs, then), uses);
            }
            Op::Read(back) => {
                self.submit();
                let uses = self.no_uses();
                self.hand_over(Item::Read(back), uses);
            }
            Op::Memory(words) => {
                let uses = self.no_uses();
                self.hand_over(Item::Memory(words), uses);
            }
            Op::Flush(reply) => {
                self.flush();
                let _ = reply.send(self.stats);
            }
        }
    }

    /// Starts the drawer, if there is a pool or a tap.
    fn start_drawer(&mut self, tap: Tap) {
        if self.pool.is_some() || tap.is_some() {
            self.drawer = Drawer::start(self.pool.clone(), tap, !self.gpu_only);
            assert!(self.drawer.is_some(), "starting the GS's drawer thread");
        }
    }

    /// A CLUT load, for the tap: handed over before the next item. A batch
    /// whose CLUTs the loads since its first primitive could overwrite is
    /// drawn first.
    fn clut_loaded(&mut self, load: export::Clut, version: u32) {
        if self
            .first_clut
            .is_some_and(|v| version.wrapping_sub(v) >= export::CLUT_SLOTS - 1)
        {
            self.submit();
        }
        self.cluts.push(load);
    }

    /// Queues a primitive, handing the batch to the drawing threads first
    /// when it must.
    fn queue(&mut self, mut prim: Prim) {
        let f = footprint(&prim);
        let p = &prim.pipe;
        let (_, _, fbw) = p.frame;
        let alone = fbw == 0
            || prim.bbox[1] >= (fbw * 64) as i32
            || f.depth.is_some_and(|d| d.overlaps(f.frame))
            || f.texture.is_some_and(|t| reads_own_writes(&f, t));
        let target = (p.frame, p.zbuf);
        let again = self.marked == Some(f);
        if alone
            || self.target.is_some_and(|t| t != target)
            || (!again && self.uses.conflict(&f))
            || self.work.prims.len() >= MAX_PRIMS
        {
            self.submit();
        }
        self.target = Some(target);
        self.first_clut.get_or_insert(prim.pipe.clut_version);
        if self.tapped {
            prim.rendered = f.texture.is_some_and(|t| self.frames.hits(t));
        }
        if self.marked != Some(f) {
            self.uses.mark(&f);
            self.marked = Some(f);
            if self.tapped {
                self.frames.mark(f.frame);
                if let (Some(d), true) = (f.depth, f.depth_written) {
                    self.frames.mark(d);
                }
            }
        }
        let [x0, x1, y0, y1] = prim.bbox;
        self.work.pixels += ((x1 - x0 + 1) * (y1 - y0 + 1)) as u64;
        self.work.prims.push(prim);
        if alone {
            // A batch of its own, drawn as one primitive after another.
            self.stats.alone += 1;
            self.work.serial = true;
            self.work.snapshot = f.texture.filter(|&t| reads_own_writes(&f, t));
            self.submit();
        }
    }

    /// Draws everything queued: when it returns, local memory holds it.
    fn flush(&mut self) {
        self.submit();
        self.wait();
    }

    /// Hands the queued primitives to the drawer and returns without
    /// waiting for them (unless the drawer already holds [`DEPTH`]
    /// batches).
    fn submit(&mut self) {
        self.hand_over_upload();
        if self.work.prims.is_empty() || self.drawer.is_none() {
            return;
        }
        let parallel = self.pool.is_some() && !self.work.serial && self.work.pixels >= PARALLEL_MIN;
        let spare = self.spare_work.pop().unwrap_or_default();
        let mut work = std::mem::replace(&mut self.work, spare);
        work.memory = Some(Arc::clone(&self.mem));
        self.first_clut = None;
        let spare = self.spare_uses.pop().unwrap_or_default();
        let uses = std::mem::replace(&mut self.uses, spare);
        self.marked = None;
        self.target = None;
        self.hand_over(Item::Batch(work, parallel), uses);
        self.stats.batches += 1;
        self.stats.parallel_batches += u64::from(parallel);
    }

    /// Writes a transfer's pixels, into `region`, after what is queued
    /// that uses those blocks, in the drawer's order.
    fn upload(&mut self, psm: Psm, bp: u32, bw: u32, writes: Vec<(u32, u32, u32)>, region: Region) {
        if self.uses.uploads_conflict(region) {
            // After the primitives queued, which use those blocks.
            self.submit();
        }
        if self.tapped {
            // No longer a frame: the blocks the pixels land in.
            let layout = crate::Layout::new(psm, bp, bw);
            let mut last = u32::MAX;
            for &(x, y, _) in &writes {
                let block = (layout.locate(x, y).word() / 64) as u32;
                if block != last {
                    self.frames.unmark(block);
                    last = block;
                }
            }
        }
        let spare = &mut self.spare_uses;
        let (upload, uses) = self
            .upload
            .get_or_insert_with(|| (Upload::default(), spare.pop().unwrap_or_default()));
        upload.add(psm, bp, bw, writes);
        // Written, for a CLUT load from it.
        uses.frame.mark(region);
        if upload.pixels >= UPLOAD_MAX {
            self.hand_over_upload();
        }
    }

    /// Before the CLUT is loaded from `region`: the batch is drawn first
    /// if it writes there. When only the tap draws, only the order matters
    /// (the tap loads the CLUT after what is handed over before it): the
    /// batch is handed over, not waited for.
    fn before_clut_load(&mut self, region: Region) {
        if self.uses.reads_conflict(region) {
            if self.gpu_only {
                self.submit();
            } else {
                self.flush();
            }
            return;
        }
        if self
            .upload
            .as_ref()
            .is_some_and(|(_, uses)| uses.reads_conflict(region))
        {
            // It comes before the primitives queued: it can go alone.
            self.hand_over_upload();
        }
        if !self.gpu_only {
            self.wait_if(|uses| uses.reads_conflict(region));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Gs;
    use crate::regs::addr;

    #[test]
    fn regions_cover_whole_pages_and_wrap() {
        // A 640-wide CT32 buffer at block 0: rows 0..31 are pages 0..9.
        let r = Region::of(Psm::Ct32, 0, 10, [0, 639, 0, 31]);
        assert_eq!(r, Region::run(0, 320));
        // Rows 32..63 start at page 10.
        assert_eq!(Region::of(Psm::Ct32, 0, 10, [0, 0, 32, 63]).start, 320);
        // A region past the end wraps to block 0.
        let w = Region::of(Psm::Ct32, BLOCKS - 32, 1, [0, 63, 0, 63]);
        assert!(w.overlaps(Region::run(0, 1)));
        assert!(!w.overlaps(Region::run(32, 1)));
        let mut set = Blocks::default();
        set.mark(w);
        assert!(set.hits(Region::run(31, 1)));
        assert!(!set.hits(Region::run(32, 64)));
        // A rectangle two pages wide, over three page rows of a buffer
        // ten pages wide: pages 4-5, 14-15 and 24-25 only.
        let r = Region::of(Psm::Ct32, 0, 10, [256, 383, 0, 95]);
        let pages: Vec<(u32, u32)> = r.ranges().collect();
        assert_eq!(pages, [(128, 192), (448, 512), (768, 832)]);
        assert!(!r.overlaps(Region::run(192, 256)));
        assert!(r.overlaps(Region::run(500, 1)));
    }

    /// Every pixel of a rectangle lies in the rectangle's region, in every
    /// format, for any base and width (odd ones and 0 included).
    #[test]
    fn regions_hold_every_pixel_of_their_rectangle() {
        use crate::Memory;
        let formats = [
            Psm::Ct32,
            Psm::Ct24,
            Psm::Ct16,
            Psm::Ct16s,
            Psm::T8,
            Psm::T4,
            Psm::T8h,
            Psm::T4hl,
            Psm::T4hh,
            Psm::Z32,
            Psm::Z24,
            Psm::Z16,
            Psm::Z16s,
        ];
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |n: u32| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % u64::from(n)) as u32
        };
        for psm in formats {
            for _ in 0..40 {
                let (bp, bw) = (next(BLOCKS), next(21));
                let (x0, y0) = (next(1800), next(1800));
                let (x1, y1) = (x0 + next(200), y0 + next(200));
                let region = Region::of(psm, bp, bw, [x0, x1, y0, y1].map(|c| c as i32));
                let mut set = Blocks::default();
                set.mark(region);
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let block = Memory::locate(psm, bp, bw, x, y).block();
                        assert!(
                            set.hits(Region::run(block, 1)),
                            "{psm:?} bp {bp} bw {bw}: ({x}, {y}) in block {block}, outside {region:?}"
                        );
                    }
                }
            }
        }
    }

    /// Textures uploaded again and again into the same blocks, each drawn
    /// from before the next replaces it, and a palette uploaded then loaded
    /// as the CLUT of an 8-bit texture.
    fn uploads(gs: &mut Gs) {
        gs.write(addr::TEST_1, 0);
        gs.write(addr::FRAME_1, 10 << 16);
        for k in 0..6u8 {
            gs.write(addr::BITBLTBUF, 0x1F40 << 32 | 1 << 48);
            gs.write(addr::TRXPOS, 0);
            gs.write(addr::TRXREG, 64 | 64 << 32);
            gs.write(addr::TRXDIR, 0);
            let texels: Vec<u8> = (0..64 * 64 * 4)
                .map(|i| (i as u8).wrapping_mul(k + 1) ^ k)
                .collect();
            // In pieces, as IMAGE packets come.
            for piece in texels.chunks(4000) {
                gs.image(piece);
            }
            gs.write(addr::TEX0_1, 0x1F40 | 1 << 14 | 6 << 26 | 6 << 30 | 1 << 35);
            gs.write(addr::PRIM, 6 | 1 << 4 | 1 << 8);
            gs.write(addr::UV, 0);
            gs.write(addr::XYZ2, (u64::from(k) * 80 * 16) | (16 * 16) << 16);
            gs.write(addr::UV, (64 * 16) | (64 * 16) << 16);
            gs.write(
                addr::XYZ2,
                ((u64::from(k) * 80 + 300) * 16) | (220 * 16) << 16,
            );
        }
        gs.write(addr::BITBLTBUF, 0x3E80 << 32 | 1 << 48);
        gs.write(addr::TRXPOS, 0);
        gs.write(addr::TRXREG, 16 | 16 << 32);
        gs.write(addr::TRXDIR, 0);
        let palette: Vec<u8> = (0..16 * 16 * 4).map(|i| (i * 7) as u8).collect();
        gs.image(&palette);
        gs.write(
            addr::TEX0_1,
            0x1F40 | 1 << 14 | 0x13 << 20 | 6 << 26 | 6 << 30 | 1 << 35 | 0x3E80 << 37 | 1 << 61,
        );
        gs.write(addr::PRIM, 6 | 1 << 4 | 1 << 8);
        gs.write(addr::UV, 0);
        gs.write(addr::XYZ2, (100 * 16) | (10 * 16) << 16);
        gs.write(addr::UV, (64 * 16) | (64 * 16) << 16);
        gs.write(addr::XYZ2, (600 * 16) | (200 * 16) << 16);
    }

    /// The cases the footprints were narrowed for, drawn on 4 threads and
    /// on 1: full-screen strips textured from the other half of a double
    /// buffer, with the depth buffer just after it and masked; a depth
    /// buffer at the frame's own address, tested ALWAYS and masked; and a
    /// 4-bit texture stored inside the depth buffer's pages while depth is
    /// tested and written.
    #[test]
    fn narrowed_footprints_draw_what_one_thread_draws() {
        let draw = |threads: usize| {
            let mut gs = Gs::default();
            gs.set_threads(threads);
            let tri = |gs: &mut Gs, k: u64, w: u64, h: u64, uv: bool| {
                gs.write(addr::RGBAQ, 0x8000_0000 | (k * 0x0103_0507));
                for (j, (x, y)) in [
                    (k * 13 % w, k * 7 % h),
                    (k * 31 % w, h - 1 - k % h),
                    (w - 1 - k * 5 % w, k * 11 % h),
                ]
                .into_iter()
                .enumerate()
                {
                    if uv {
                        gs.write(addr::UV, ((x % 64) * 16) | (((y % 64) * 16) << 16));
                    }
                    let z = (k * 977 + j as u64 * 131) & 0xFFFF;
                    gs.write(addr::XYZ2, (x * 16) | (y * 16) << 16 | z << 32);
                }
            };
            gs.write(addr::SCISSOR_1, 639 << 16 | 223 << 48);
            // The second buffer (block 2240), then strips into the first
            // textured from it; depth at 4480, masked, tested ALWAYS.
            gs.write(addr::FRAME_1, 0x46 | 10 << 16);
            gs.write(addr::PRIM, 3 | 1 << 3);
            for k in 0..30 {
                tri(&mut gs, k, 640, 224, false);
            }
            gs.write(addr::FRAME_1, 10 << 16);
            gs.write(addr::ZBUF_1, 0x8C | 1 << 32);
            gs.write(addr::TEST_1, 1 << 16 | 1 << 17);
            gs.write(
                addr::TEX0_1,
                0x8C0 | 10 << 14 | 10 << 26 | 10 << 30 | 1 << 35,
            );
            gs.write(addr::TEX1_1, 1 << 5);
            gs.write(addr::PRIM, 6 | 1 << 4 | 1 << 8);
            for k in 0..20u64 {
                gs.write(addr::UV, (k * 32 * 16 + 5) | 3 << 16);
                gs.write(addr::XYZ2, (k * 32 * 16) | 16 << 16);
                gs.write(addr::UV, ((k + 1) * 32 * 16 + 5) | (224 * 16) << 16);
                gs.write(addr::XYZ2, ((k + 1) * 32 * 16) | (223 * 16) << 16);
            }
            // A 64-wide buffer whose depth buffer is itself.
            gs.write(addr::FRAME_1, 0x12C | 1 << 16);
            gs.write(addr::ZBUF_1, 0x12C | 1 << 32);
            gs.write(addr::PRIM, 3 | 1 << 3);
            for k in 0..30 {
                tri(&mut gs, k, 64, 64, false);
            }
            // Depth tested and written while texturing (T4, CLUT from the
            // frame) from blocks inside the depth buffer's pages.
            gs.write(addr::FRAME_1, 10 << 16);
            gs.write(addr::ZBUF_1, 0x8C);
            gs.write(addr::TEST_1, 1 << 16 | 2 << 17);
            gs.write(
                addr::TEX0_1,
                0x1680 | 8 << 14 | 0x14 << 20 | 6 << 26 | 6 << 30 | 1 << 35 | 1 << 61,
            );
            gs.write(addr::PRIM, 3 | 1 << 3 | 1 << 4 | 1 << 8);
            for k in 0..40 {
                tri(&mut gs, k, 640, 224, true);
            }
            uploads(&mut gs);
            gs.flush();
            // The strips and the self-depth buffer batch now.
            assert!(
                threads == 1 || gs.parallel_batches >= 3,
                "{}",
                gs.parallel_batches
            );
            gs.mem.to_bytes()
        };
        let same = draw(4) == draw(1);
        assert!(same, "the memory differs");
    }

    /// The same packets drawn serially and on threads leave the same
    /// memory: overlapping textured sprites (one reading what another
    /// drew) and triangles over many tiles.
    #[test]
    fn threads_draw_what_one_thread_draws() {
        let draw = |threads: usize| {
            let mut gs = Gs::default();
            gs.set_threads(threads);
            gs.write(addr::FRAME_1, 10 << 16);
            gs.write(addr::SCISSOR_1, 639 << 16 | 447 << 48);
            gs.write(addr::ALPHA_1, 0x44);
            for k in 0..40u64 {
                gs.write(addr::RGBAQ, 0x4000_0000 | (k * 0x0105_0709));
                // Every third antialiased: its edges reach past its box.
                gs.write(addr::PRIM, 3 | 1 << 3 | 1 << 6 | u64::from(k % 3 == 0) << 7);
                for (x, y) in [
                    (k * 13 % 600, k * 7 % 400),
                    (k * 31 % 640, 447 - k),
                    (639 - k * 5, k * 11 % 448),
                ] {
                    gs.write(addr::XYZ2, (x * 16) | (y * 16) << 16);
                }
                // A sprite textured from the frame's top-left corner,
                // drawn elsewhere: it reads what earlier primitives drew.
                gs.write(addr::TEX0_1, 10 << 14 | 9 << 26 | 9 << 30 | 1 << 35);
                gs.write(addr::PRIM, 6 | 1 << 4 | 1 << 8);
                gs.write(addr::UV, 0);
                gs.write(addr::XYZ2, ((300 + k) * 16) | ((200 + k) * 16) << 16);
                gs.write(addr::UV, (64 * 16) | (64 * 16) << 16);
                gs.write(addr::XYZ2, ((364 + k) * 16) | ((264 + k) * 16) << 16);
            }
            gs.flush();
            gs.mem.to_bytes()
        };
        // Not assert_eq: a mismatch would print 4 MiB.
        let same = draw(4) == draw(1);
        assert!(same, "the memory differs");
    }
}
