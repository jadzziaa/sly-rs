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
//! frame, is drawn alone, in order.
//!
//! Footprints are conservative: on each page row a rectangle covers, the
//! pages from its first column's to its last's. A texture's rectangle is
//! the texels the primitive can read ([`Prim::texels`]); a depth buffer
//! counts only where the depth test compares or depth is written.

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
}

/// What a batch draws into: the frame and the depth buffer.
type Target = ((Psm, u32, u32), Option<(Psm, u32)>);

/// Primitives waiting to be drawn, and what they read and write.
#[derive(Default)]
pub(crate) struct Batch {
    pool: Option<rayon::ThreadPool>,
    prims: Vec<Prim>,
    /// Blocks written as frame, written as depth, used as depth (tested or
    /// written), read as texture.
    frame: Blocks,
    depth_written: Blocks,
    depth: Blocks,
    texture: Blocks,
    target: Option<Target>,
    pixels: u64,
    tiles: Vec<Vec<u32>>,
}

impl Batch {
    /// Whether a primitive with this footprint must wait for the batch: it
    /// reads what the batch writes or writes what it reads, other than the
    /// same frame or depth pixels, which stay in one tile.
    fn conflicts(&self, f: &Footprint) -> bool {
        let written = |r: Region| self.frame.hits(r) || self.depth_written.hits(r);
        f.texture.is_some_and(written)
            || self.texture.hits(f.frame)
            || self.depth.hits(f.frame)
            || f.depth
                .is_some_and(|d| self.frame.hits(d) || (f.depth_written && self.texture.hits(d)))
    }

    fn clear(&mut self) {
        self.prims.clear();
        for set in [
            &mut self.frame,
            &mut self.depth_written,
            &mut self.depth,
            &mut self.texture,
        ] {
            set.clear();
        }
        self.target = None;
        self.pixels = 0;
    }
}

/// The blocks a primitive reads (its texture, the depth it tests) and
/// writes (frame, depth).
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
            || f.texture.is_some_and(|t| {
                t.overlaps(f.frame) || f.depth.is_some_and(|d| f.depth_written && t.overlaps(d))
            });
        if alone {
            self.alone += 1;
            self.flush();
            Raster { mem: &self.mem }.prim(&prim, prim.bbox);
            return;
        }
        let target = (p.frame, p.zbuf);
        let b = &self.batch;
        if b.target.is_some_and(|t| t != target) || b.conflicts(&f) || b.prims.len() >= MAX_PRIMS {
            self.flush();
        }
        let b = &mut self.batch;
        b.target = Some(target);
        b.frame.mark(f.frame);
        if let Some(d) = f.depth {
            b.depth.mark(d);
            if f.depth_written {
                b.depth_written.mark(d);
            }
        }
        if let Some(t) = f.texture {
            b.texture.mark(t);
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
        self.batches += 1;
        match &b.pool {
            Some(pool) if b.pixels >= PARALLEL_MIN => {
                self.parallel_batches += 1;
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
        b.clear();
    }

    /// Before an upload writes `region`: the batch is drawn first if it
    /// reads or writes any of it.
    pub(crate) fn before_upload(&mut self, region: Region) {
        let b = &self.batch;
        if [&b.frame, &b.depth, &b.texture]
            .iter()
            .any(|set| set.hits(region))
        {
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
        if self.batch.frame.hits(region) || self.batch.depth_written.hits(region) {
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
