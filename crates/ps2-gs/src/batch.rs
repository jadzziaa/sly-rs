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
//! - a primitive with another frame or depth buffer;
//! - an upload into blocks the batch reads or writes, a CLUT load from
//!   blocks it writes, a copy between buffers, a read back;
//! - anything that reads local memory from outside (pictures, dumps).
//!
//! A primitive whose texture is its own frame or depth buffer, that reaches
//! past its frame's width (where pixels of one row share pages with the
//! next), or whose depth buffer overlaps its frame, is drawn alone, in order.
//! Regions are conservative: whole pages of the rows a primitive covers.

use rayon::prelude::*;

use crate::draw::{Prim, Raster};
use crate::mem::page_shape;
use crate::regs::{Tex0, bits};
use crate::{Gs, Psm};

/// Blocks of local memory.
const BLOCKS: u32 = 16384;
/// Tile size, in pixels.
const TILE: i32 = 32;
/// Tiles across (and down) the GS's 2048 × 2048 coordinate space.
const TILES: usize = 2048 / TILE as usize;
/// Pixels a batch must cover before it is worth spreading over threads.
const PARALLEL_MIN: u64 = 16 * 1024;
/// Primitives a batch holds at most.
const MAX_PRIMS: usize = 16 * 1024;

/// Blocks `start .. start + count` (modulo the memory's size).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    start: u32,
    count: u32,
}

impl Region {
    /// All of local memory.
    pub(crate) const ALL: Self = Self {
        start: 0,
        count: BLOCKS,
    };

    /// The blocks a rectangle (x0, x1, y0, y1, inclusive) of a buffer may
    /// touch: every page from the first row's to the last pixel's.
    pub(crate) fn of(psm: Psm, bp: u32, bw: u32, rect: [i32; 4]) -> Self {
        let (w_shift, h_shift, pages_per_row) = page_shape(psm, bw);
        let [x0, x1, y0, y1] = rect.map(|c| c.max(0) as u32);
        let _ = x0;
        let first = (y0 >> h_shift) * pages_per_row;
        let last = (y1 >> h_shift) * pages_per_row + (x1 >> w_shift);
        Self {
            start: (bp + first * 32) % BLOCKS,
            count: ((last - first + 1) * 32).min(BLOCKS),
        }
    }

    /// The region as ranges that do not wrap.
    fn ranges(self) -> impl Iterator<Item = (u32, u32)> {
        let end = self.start + self.count;
        let first = (self.start, end.min(BLOCKS));
        let second = (end > BLOCKS).then(|| (0, end - BLOCKS));
        std::iter::once(first).chain(second)
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
}

/// What a batch draws into: the frame and the depth buffer.
type Target = ((Psm, u32, u32), Option<(Psm, u32)>);

/// Primitives waiting to be drawn, and what they read and write.
#[derive(Default)]
pub(crate) struct Batch {
    pool: Option<rayon::ThreadPool>,
    prims: Vec<Prim>,
    written: Blocks,
    read: Blocks,
    target: Option<Target>,
    pixels: u64,
    tiles: Vec<Vec<u32>>,
}

/// The blocks a primitive reads (its texture) and writes (frame, depth).
struct Footprint {
    frame: Region,
    depth: Option<Region>,
    texture: Option<Region>,
}

fn footprint(prim: &Prim) -> Footprint {
    let p = &prim.pipe;
    let (fpsm, fbp, fbw) = p.frame;
    let texture = p.tex_psm.filter(|_| p.textured()).map(|psm| {
        let (w, h) = (1i32 << p.tex.tw.min(10), 1i32 << p.tex.th.min(10));
        Region::of(psm, p.tex.tbp, p.tex.tbw, [0, w - 1, 0, h - 1])
    });
    Footprint {
        frame: Region::of(fpsm, fbp, fbw, prim.bbox),
        depth: p
            .zbuf
            .map(|(zpsm, zbp)| Region::of(zpsm, zbp, fbw, prim.bbox)),
        texture,
    }
}

impl Gs {
    /// Draws with `threads` threads (1: one primitive after another, as it
    /// comes). The result is the same either way.
    ///
    /// # Panics
    ///
    /// If the threads cannot be started.
    pub fn set_threads(&mut self, threads: usize) {
        self.flush();
        self.batch.pool = (threads > 1).then(|| {
            match rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .thread_name(|i| format!("gs-draw-{i}"))
                .build()
            {
                Ok(pool) => pool,
                Err(e) => panic!("starting the GS's drawing threads: {e}"),
            }
        });
    }

    pub(crate) const fn batching(&self) -> bool {
        self.batch.pool.is_some()
    }

    /// Queues a primitive, drawing the batch first when it must.
    pub(crate) fn queue(&mut self, prim: Prim) {
        let f = footprint(&prim);
        let p = &prim.pipe;
        let (_, _, fbw) = p.frame;
        let alone = fbw == 0
            || prim.bbox[1] >= (fbw * 64) as i32
            || f.depth.is_some_and(|d| d.overlaps(f.frame))
            || f.texture
                .is_some_and(|t| t.overlaps(f.frame) || f.depth.is_some_and(|d| t.overlaps(d)));
        if alone {
            self.flush();
            Raster { mem: &self.mem }.prim(&prim, prim.bbox);
            return;
        }
        let target = (p.frame, p.zbuf);
        let b = &self.batch;
        if b.target.is_some_and(|t| t != target)
            || f.texture.is_some_and(|t| b.written.hits(t))
            || b.read.hits(f.frame)
            || f.depth.is_some_and(|d| b.read.hits(d))
            || b.prims.len() >= MAX_PRIMS
        {
            self.flush();
        }
        let b = &mut self.batch;
        b.target = Some(target);
        b.written.mark(f.frame);
        if let Some(d) = f.depth {
            b.written.mark(d);
        }
        if let Some(t) = f.texture {
            b.read.mark(t);
        }
        let [x0, x1, y0, y1] = prim.bbox;
        b.pixels += ((x1 - x0 + 1) * (y1 - y0 + 1)) as u64;
        b.prims.push(prim);
    }

    /// Draws the primitives waiting in the batch.
    pub fn flush(&mut self) {
        let Self { batch: b, mem, .. } = self;
        if b.prims.is_empty() {
            return;
        }
        let raster = Raster { mem };
        match &b.pool {
            Some(pool) if b.pixels >= PARALLEL_MIN => {
                if b.tiles.is_empty() {
                    b.tiles = vec![Vec::new(); TILES * TILES];
                }
                let mut used = Vec::new();
                for (i, prim) in b.prims.iter().enumerate() {
                    let [x0, x1, y0, y1] = prim.bbox.map(|c| c / TILE);
                    for ty in y0..=y1 {
                        for tx in x0..=x1 {
                            let t = ty as usize * TILES + tx as usize;
                            if b.tiles[t].is_empty() {
                                used.push(t);
                            }
                            b.tiles[t].push(i as u32);
                        }
                    }
                }
                let (prims, tiles) = (&b.prims, &b.tiles);
                pool.install(|| {
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
                });
                for t in used {
                    b.tiles[t].clear();
                }
            }
            _ => {
                for prim in &b.prims {
                    raster.prim(prim, prim.bbox);
                }
            }
        }
        b.prims.clear();
        b.written.clear();
        b.read.clear();
        b.target = None;
        b.pixels = 0;
    }

    /// Before an upload writes `region`: the batch is drawn first if it
    /// reads or writes any of it.
    pub(crate) fn before_upload(&mut self, region: Region) {
        if self.batch.read.hits(region) || self.batch.written.hits(region) {
            self.flush();
        }
    }

    /// Before the CLUT is loaded: the batch is drawn first if it writes
    /// what the CLUT is read from.
    pub(crate) fn before_clut_load(&mut self, t: Tex0) {
        let region = if t.csm {
            let (cbw, cou, cov) = (
                bits(self.texclut, 0, 6),
                bits(self.texclut, 6, 6) as i32,
                bits(self.texclut, 12, 10) as i32,
            );
            Region::of(Psm::Ct16, t.cbp, cbw, [cou * 16, cou * 16 + 255, cov, cov])
        } else {
            let psm = if t.cpsm < 2 { Psm::Ct32 } else { Psm::Ct16 };
            Region::of(psm, t.cbp, 1, [0, 15, 0, 15])
        };
        if self.batch.written.hits(region) {
            self.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regs::addr;

    #[test]
    fn regions_cover_whole_pages_and_wrap() {
        // A 640-wide CT32 buffer at block 0: rows 0..31 are pages 0..9.
        let r = Region::of(Psm::Ct32, 0, 10, [0, 639, 0, 31]);
        assert_eq!((r.start, r.count), (0, 320));
        // Rows 32..63 start at page 10.
        assert_eq!(Region::of(Psm::Ct32, 0, 10, [0, 0, 32, 63]).start, 320);
        // A region past the end wraps to block 0.
        let w = Region::of(Psm::Ct32, BLOCKS - 32, 1, [0, 63, 0, 63]);
        assert!(w.overlaps(Region { start: 0, count: 1 }));
        assert!(!w.overlaps(Region {
            start: 32,
            count: 1
        }));
        let mut set = Blocks::default();
        set.mark(w);
        assert!(set.hits(Region {
            start: 31,
            count: 1
        }));
        assert!(!set.hits(Region {
            start: 32,
            count: 64
        }));
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
                gs.write(addr::PRIM, 3 | 1 << 3 | 1 << 6);
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
