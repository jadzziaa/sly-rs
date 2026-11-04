//! Drawing: primitives to fragments, fragments through the pixel pipeline.
//!
//! Coordinates are 12.4 fixed point less XYOFFSET; a pixel's sample point
//! is its integer coordinate, and an edge on a sample point belongs to the
//! primitive on its right and below (top-left rule). Sprites take the second
//! vertex's colour, depth and fog; flat-shaded triangles and lines the last
//! vertex's.
//!
//! The pipeline per fragment: texture (wrap, CLUT, TEXA, nearest or
//! bilinear) and the texture function, fog, the alpha test, the destination
//! alpha test, the depth test, blending, COLCLAMP, FBA, then the masked
//! frame and depth writes.
//!
//! A primitive is drawn from a [`Prim`]: its vertices and a snapshot of
//! every register it reads ([`Pipe`]), so it can be drawn later, and in
//! parts. [`Raster`] draws the part inside a clip rectangle; the parts of
//! one primitive in rectangles that tile its bounding box are the whole of
//! it ([`crate::batch`] draws tiles on several threads).

use std::sync::Arc;

pub(crate) mod encode;

use crate::batch::{Snapshot, own_texture};
use crate::fixed;
use crate::regs::{Tex0, bits};
use crate::{Gs, Layout, Memory, Psm, Vertex, clut_color, expand16, expand24};

/// Everything a primitive's fragments need, decoded once.
#[allow(clippy::struct_excessive_bools, reason = "PRIM's attribute bits")]
#[derive(Clone)]
pub(crate) struct Pipe {
    tme: bool,
    fge: bool,
    abe: bool,
    /// Antialiasing (`AA1`), for lines and triangles: coverage alpha.
    aa1: bool,
    fst: bool,
    iip: bool,
    /// Scissor, inclusive, in pixels: x0, x1, y0, y1.
    pub(crate) scissor: [i32; 4],
    offset: (i32, i32),
    /// The frame buffer: format, base block, width in 64-pixel units.
    pub(crate) frame: (Psm, u32, u32),
    fbmsk: u32,
    /// The depth buffer when the depth test is on: format, base block.
    pub(crate) zbuf: Option<(Psm, u32)>,
    zmsk: bool,
    test: u64,
    alpha: u64,
    fba: bool,
    pub(crate) tex: Tex0,
    pub(crate) tex_psm: Option<Psm>,
    /// Where the frame's, the depth buffer's and the texture's pixels live.
    frame_layout: Layout,
    depth_layout: Option<Layout>,
    tex_layout: Option<Layout>,
    /// CLAMP for U and V.
    wrap: [Wrap; 2],
    clamp: u64,
    linear: bool,
    texa: u64,
    fogcol: u64,
    colclamp: bool,
    pabe: bool,
    scanmsk: u32,
    clut: Arc<[u16; 512]>,
    /// The CLUT's version (see [`crate::export::Clut`]).
    pub(crate) clut_version: u32,
    /// The state most of Sly's pixels are drawn in (see [`Pipe::is_hot`]):
    /// its triangles take the pipeline specialised to it.
    hot: bool,
}

impl Pipe {
    /// Whether this is the state the specialised pipeline (`HOT`) is for:
    /// a CLUT texture, bilinear, modulated with its alpha; blending, fog
    /// and Gouraud colour; a 32-bit frame written whole; no destination
    /// alpha test, antialiasing or scan mask. Depth, the alpha test, the
    /// blend's inputs and the CLUT's format stay as they are.
    fn is_hot(&self) -> bool {
        self.tme
            && self.linear
            && self.tex.tfx == 0
            && self.tex.tcc
            && matches!(
                self.tex_psm,
                Some(Psm::T8 | Psm::T4 | Psm::T8h | Psm::T4hl | Psm::T4hh)
            )
            && self.tex_layout.is_some()
            && bits(self.test, 14, 1) == 0
            && self.abe
            && self.fge
            && !self.aa1
            && self.iip
            && self.frame.0 == Psm::Ct32
            && self.fbmsk == 0
            && self.scanmsk == 0
    }

    /// The state bits that decide which parts of the pipeline run, for
    /// reports.
    pub(crate) fn state(&self, kind: u64) -> String {
        let kinds = [
            "point", "line", "strip", "tri", "tristrip", "fan", "sprite", "?",
        ];
        let ztst = if self.zbuf.is_some() {
            bits(self.test, 17, 2)
        } else {
            9
        };
        let ate = if bits(self.test, 0, 1) != 0 {
            format!("{}/{}", bits(self.test, 1, 3), bits(self.test, 12, 2))
        } else {
            "-".to_owned()
        };
        format!(
            "{} tex {} {} tfx{}{} z{}{} ate{} date{} abe{} fge{} aa{} iip{} fb {:?}{}",
            kinds[kind as usize & 7],
            if self.tme {
                format!("{:?}", self.tex_psm)
            } else {
                "-".to_owned()
            },
            match (self.tme, self.linear, self.fst) {
                (false, _, _) => "",
                (true, true, true) => "lin uv",
                (true, true, false) => "lin stq",
                (true, false, true) => "uv",
                (true, false, false) => "stq",
            },
            self.tex.tfx,
            if self.tex.tcc { "a" } else { "" },
            ztst,
            if self.zmsk { "m" } else { "" },
            ate,
            bits(self.test, 14, 1),
            u8::from(self.abe),
            u8::from(self.fge),
            u8::from(self.aa1),
            u8::from(self.iip),
            self.frame.0,
            if self.fbmsk == 0 { "" } else { " masked" },
        )
    }

    /// Whether the primitive reads its texture.
    pub(crate) const fn textured(&self) -> bool {
        self.tme
    }

    /// Whether the depth test uses the depth buffer's values, and whether
    /// fragments may write it.
    pub(crate) fn depth_use(&self) -> (bool, bool) {
        let ztst = bits(self.test, 17, 2);
        let on = self.zbuf.is_some() && ztst != 0;
        (on && ztst >= 2, on && !self.zmsk)
    }
}

/// How CLAMP wraps texture coordinates on one axis.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Wrap {
    mode: u32,
    size: i32,
    min: i32,
    max: i32,
}

impl Wrap {
    fn new(clamp: u64, axis: u32, size: i32) -> Self {
        let (min, max) = if axis == 0 { (4, 14) } else { (24, 34) };
        Self {
            mode: bits(clamp, 2 * axis, 2),
            size,
            min: bits(clamp, min, 10) as i32,
            max: bits(clamp, max, 10) as i32,
        }
    }

    /// Coordinate `c` wrapped.
    #[inline]
    fn apply(self, c: i32) -> i32 {
        match self.mode {
            0 => c & (self.size - 1),
            1 => c.clamp(0, self.size - 1),
            2 => c.clamp(self.min, self.max),
            _ => (c & self.min) | self.max,
        }
    }
}

/// A primitive ready to draw: its kind (PRIM's low bits), vertices, the
/// registers it reads, and the pixels it may touch (x0, x1, y0, y1,
/// inclusive; inside the scissor).
#[derive(Clone)]
pub(crate) struct Prim {
    pub(crate) pipe: Arc<Pipe>,
    kind: u64,
    v: [Vertex; 3],
    pub(crate) bbox: [i32; 4],
}

impl Prim {
    /// The texels the primitive may read (u0, u1, v0, v1, inclusive),
    /// after CLAMP's wrapping; `None` when that is not known to be less
    /// than the whole texture.
    ///
    /// Every pixel's texture coordinates lie within the vertices': UV and
    /// sprites interpolate linearly, and perspective (S, T, Q) gives a
    /// weighted average of the vertices' S/Q and T/Q when every Q is
    /// positive. A sprite's are taken at its first and last pixels.
    /// Bilinear filtering reaches half a texel either side; an eighth more
    /// covers truncation and rounding.
    pub(crate) fn texels(&self) -> Option<[i32; 4]> {
        let p = &self.pipe;
        let n = match self.kind {
            0 => 1,
            1 | 2 | 6 => 2,
            _ => 3,
        };
        let vertices = &self.v[..n];
        if !p.fst && vertices.iter().any(|v| v.q.is_nan() || v.q <= 0.0) {
            return None;
        }
        let mut coords = [[0.0f32; 2]; 3];
        for (c, v) in coords.iter_mut().zip(vertices) {
            let uv = attrs(p, v).uv;
            *c = [uv.0, uv.1];
        }
        let mut count = n;
        if self.kind == 6 {
            // A sprite samples at its pixels only: its coordinates at the
            // first and last pixels it may draw, as the rasteriser gets
            // them.
            let at = |v: &Vertex| [v.x as i32 - p.offset.0, v.y as i32 - p.offset.1];
            let (a, b) = (at(&self.v[0]), at(&self.v[1]));
            let mut sampled = [[0.0; 2]; 2];
            for k in 0..2 {
                let ((s0, c0), (s1, c1)) = if a[k] <= b[k] {
                    ((a[k], coords[0][k]), (b[k], coords[1][k]))
                } else {
                    ((b[k], coords[1][k]), (a[k], coords[0][k]))
                };
                if s0 == s1 {
                    return None;
                }
                for (end, pixel) in [self.bbox[2 * k], self.bbox[2 * k + 1]]
                    .into_iter()
                    .enumerate()
                {
                    sampled[end][k] = sprite_coord(p.fst, c0, c1, s0, s1, pixel * 16);
                }
            }
            coords[..2].copy_from_slice(&sampled);
            count = 2;
        }
        let (mut lo, mut hi) = ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]);
        for c in &coords[..count] {
            for k in 0..2 {
                if !c[k].is_finite() {
                    return None;
                }
                lo[k] = lo[k].min(c[k]);
                hi[k] = hi[k].max(c[k]);
            }
        }
        let cl = p.clamp;
        let axis = |k: usize, mode: u32, size: i32, min: i32, max: i32| -> [i32; 2] {
            // Saturating: far outside the texture only widens the range.
            let (c0, c1) = (
                (lo[k] - 0.625).floor() as i32,
                (hi[k] + 0.625).floor() as i32,
            );
            match mode {
                0 if c0 >= 0 && c1 < size => [c0, c1],
                0 => [0, size - 1],
                1 => [c0.clamp(0, size - 1), c1.clamp(0, size - 1)],
                2 if min <= max => [c0.clamp(min, max), c1.clamp(min, max)],
                // REGION_REPEAT: (c & min) | max lies within max..=min | max.
                3 => [max, min | max],
                _ => [0, 0x7FF],
            }
        };
        let [u0, u1] = axis(
            0,
            bits(cl, 0, 2),
            1 << p.tex.tw.min(10),
            bits(cl, 4, 10) as i32,
            bits(cl, 14, 10) as i32,
        );
        let [v0, v1] = axis(
            1,
            bits(cl, 2, 2),
            1 << p.tex.th.min(10),
            bits(cl, 24, 10) as i32,
            bits(cl, 34, 10) as i32,
        );
        Some([u0, u1, v0, v1])
    }
}

/// A fragment's interpolated attributes.
#[derive(Clone, Copy)]
struct Frag {
    z: u32,
    rgba: [i32; 4],
    /// Texture coordinates, in texels, as primitives other than
    /// triangles interpolate them.
    uv: (f32, f32),
    /// Texture coordinates as sampling takes them: in 1/16 texel, nearest
    /// sampling at `tex >> 4`, bilinear from `tex - 8` (see [`texcoord`]
    /// and `fixed`).
    tex: (i32, i32),
    /// S, T and Q, for perspective interpolation.
    stq: [f32; 3],
    f: i32,
    /// On an antialiased edge, the share of the pixel covered (0 to
    /// 0xFFFF).
    coverage: Option<u32>,
}

impl Gs {
    /// Whether the texture filter depends on RGBAQ's Q (LOD from Q, not
    /// LCM, with STQ coordinates): see [`Gs::pipe`].
    fn filter_uses_q(&self) -> bool {
        let attrs = if self.prmodecont & 1 != 0 {
            self.prim
        } else {
            self.prmode
        };
        let c = &self.ctx[bits(attrs, 9, 1) as usize];
        bits(c.tex1, 0, 1) == 0 && bits(attrs, 8, 1) == 0
    }

    fn pipe(&mut self) -> Pipe {
        let attrs = if self.prmodecont & 1 != 0 {
            self.prim
        } else {
            self.prmode
        };
        let c = &self.ctx[bits(attrs, 9, 1) as usize];
        let tex = Tex0::new(c.tex0);
        let fpsm = Psm::from_bits(u64::from(bits(c.frame, 24, 6))).unwrap_or(Psm::Ct32);
        let zpsm = Psm::from_bits(u64::from(bits(c.zbuf, 24, 4) | 0x30)).unwrap_or(Psm::Z32);
        let tex1 = c.tex1;
        let lod = if bits(tex1, 0, 1) != 0 || bits(attrs, 8, 1) != 0 {
            // LCM, or UV coordinates: LOD is K alone (sign-extended 7.4).
            ((bits(tex1, 32, 12) << 20) as i32 >> 20) as f32 / 16.0
        } else {
            let q = f32::from_bits((self.rgbaq >> 32) as u32)
                .abs()
                .max(f32::MIN_POSITIVE);
            (1.0 / q).log2() * f32::from(1u16 << bits(tex1, 19, 2))
                + ((bits(tex1, 32, 12) << 20) as i32 >> 20) as f32 / 16.0
        };
        let linear = if lod <= 0.0 {
            bits(tex1, 5, 1) != 0
        } else {
            matches!(bits(tex1, 6, 3), 1 | 3 | 5)
        };
        Pipe {
            tme: bits(attrs, 4, 1) != 0,
            fge: bits(attrs, 5, 1) != 0,
            abe: bits(attrs, 6, 1) != 0,
            // Lines and triangles only: `draw` clears it for the others.
            aa1: bits(attrs, 7, 1) != 0,
            fst: bits(attrs, 8, 1) != 0,
            iip: bits(attrs, 3, 1) != 0,
            scissor: [
                bits(c.scissor, 0, 11) as i32,
                bits(c.scissor, 16, 11) as i32,
                bits(c.scissor, 32, 11) as i32,
                bits(c.scissor, 48, 11) as i32,
            ],
            offset: (
                bits(c.xyoffset, 0, 16) as i32,
                bits(c.xyoffset, 32, 16) as i32,
            ),
            frame: (fpsm, bits(c.frame, 0, 9) * 32, bits(c.frame, 16, 6)),
            frame_layout: Layout::new(fpsm, bits(c.frame, 0, 9) * 32, bits(c.frame, 16, 6)),
            depth_layout: (bits(c.test, 16, 1) != 0)
                .then(|| Layout::new(zpsm, bits(c.zbuf, 0, 9) * 32, bits(c.frame, 16, 6))),
            tex_layout: Psm::from_bits(u64::from(tex.psm))
                .map(|psm| Layout::new(psm, tex.tbp, tex.tbw)),
            fbmsk: (c.frame >> 32) as u32,
            // ZTE off (which the manual forbids) leaves depth alone.
            zbuf: (bits(c.test, 16, 1) != 0).then_some((zpsm, bits(c.zbuf, 0, 9) * 32)),
            zmsk: bits(c.zbuf, 32, 1) != 0,
            test: c.test,
            alpha: c.alpha,

            fba: bits(c.fba, 0, 1) != 0,
            tex,
            tex_psm: Psm::from_bits(u64::from(tex.psm)),
            clamp: c.clamp,
            wrap: [
                Wrap::new(c.clamp, 0, 1 << tex.tw.min(10)),
                Wrap::new(c.clamp, 1, 1 << tex.th.min(10)),
            ],
            linear,
            texa: self.texa,
            fogcol: self.fogcol,
            colclamp: bits(self.colclamp, 0, 1) != 0,
            pabe: bits(self.pabe, 0, 1) != 0,
            scanmsk: bits(self.scanmsk, 0, 2),
            clut: Arc::clone(self.clut_shared.get_or_insert_with(|| Arc::new(self.clut))),
            clut_version: self.clut_version,
            hot: false,
        }
    }

    /// Draws a primitive from its vertices: at once, or queued in a batch
    /// when the GS draws with several threads.
    pub(crate) fn draw(&mut self, v: &[Vertex]) {
        self.drawn += 1;
        let started = std::time::Instant::now();
        let kind = self.prim & 7;
        let q = (self.rgbaq >> 32) as u32;
        let pipe = match &self.pipe_cache {
            Some((pipe, uses)) if uses.is_none_or(|used| used == q) => Arc::clone(pipe),
            _ => {
                let mut pipe = self.pipe();
                pipe.aa1 &= matches!(kind, 1..=5);
                pipe.hot = self.specialise && pipe.is_hot();
                let pipe = Arc::new(pipe);
                let uses = self.filter_uses_q().then_some(q);
                self.pipe_cache = Some((Arc::clone(&pipe), uses));
                pipe
            }
        };
        let mut vertices = [Vertex::default(); 3];
        vertices[..v.len()].copy_from_slice(v);
        if let Some(bbox) = bounds(&pipe, kind, &vertices) {
            if let Some(states) = &mut self.states {
                let [x0, x1, y0, y1] = bbox;
                let area = (x1 - x0 + 1) as u64 * (y1 - y0 + 1) as u64;
                let e = states.entry(pipe.state(kind)).or_default();
                e.0 += 1;
                e.1 += if (3..=5).contains(&kind) {
                    area / 2
                } else {
                    area
                };
                // Facts for the GPU's arithmetic ('#' keys, primitives only):
                // the depth format, and for STQ texturing Q's sign and
                // exponent and the spread of S, T, Q exponents.
                let mut fact = |k: String| states.entry(k).or_default().0 += 1;
                if let Some((zpsm, _)) = pipe.zbuf {
                    fact(format!("#zbuf {zpsm:?}"));
                }
                if pipe.tme && !pipe.fst {
                    let n = match kind {
                        0 => 1,
                        1 | 2 | 6 => 2,
                        _ => 3,
                    };
                    let exp = |f: f32| ((f.to_bits() >> 23) & 0xFF) as i32 - 127;
                    for v in &vertices[..n] {
                        if v.q <= 0.0 || !v.q.is_finite() {
                            let sign = if v.q == 0.0 {
                                "zero"
                            } else if v.q < 0.0 {
                                "negative"
                            } else {
                                "not finite"
                            };
                            fact(format!("#q {sign} (kind {kind})"));
                        } else {
                            fact(format!("#q exponent {:+03}", exp(v.q)));
                        }
                        let e: Vec<i32> = [v.s, v.t, v.q]
                            .iter()
                            .filter(|f| **f != 0.0)
                            .map(|f| exp(*f))
                            .collect();
                        if let (Some(lo), Some(hi)) = (e.iter().min(), e.iter().max()) {
                            fact(format!("#S T Q exponent spread {:02}", hi - lo));
                        }
                    }
                    if kind == 6 {
                        fact("#STQ sprite".to_owned());
                    }
                }
            }
            let prim = Prim {
                pipe,
                kind,
                v: vertices,
                bbox,
            };
            if self.batching() {
                self.queue(prim);
            } else {
                let snapshot = own_texture(&prim).map(|r| Snapshot::of(&self.mem, r));
                Raster {
                    mem: &self.mem,
                    tex: snapshot.as_ref(),
                }
                .prim(&prim, prim.bbox);
            }
        }
        self.draw_time += started.elapsed();
    }
}

/// The pixel a point at `pos` (1/16 pixel) lights: its position plus half
/// a pixel, truncated toward zero (as PCSX2's software renderer).
#[allow(
    clippy::cast_possible_truncation,
    reason = "the truncation is the rule"
)]
fn point(pos: (i32, i32)) -> (i32, i32) {
    let at = |c: i32| (c as f32 / 16.0 + 0.5) as i32;
    (at(pos.0), at(pos.1))
}

/// The pixels a primitive may touch (x0, x1, y0, y1, inclusive), inside
/// its scissor; `None` if there are none.
fn bounds(p: &Pipe, kind: u64, v: &[Vertex; 3]) -> Option<[i32; 4]> {
    let at = |v: &Vertex| (v.x as i32 - p.offset.0, v.y as i32 - p.offset.1);
    let all = [at(&v[0]), at(&v[1]), at(&v[2])];
    let pos = match kind {
        0 => &all[..1],
        1 | 2 | 6 => &all[..2],
        _ => &all[..],
    };
    let (min_x, max_x) = (
        pos.iter().map(|q| q.0).min()?,
        pos.iter().map(|q| q.0).max()?,
    );
    let (min_y, max_y) = (
        pos.iter().map(|q| q.1).min()?,
        pos.iter().map(|q| q.1).max()?,
    );
    // The sample points each rasteriser can reach.
    let [x0, x1, y0, y1] = match kind {
        0 => {
            let (x, y) = point((min_x, min_y));
            [x, x, y, y]
        }
        1 | 2 => [
            (min_x + 8) >> 4,
            (max_x + 8) >> 4,
            (min_y + 8) >> 4,
            (max_y + 8) >> 4,
        ],
        6 => [
            (min_x + 15) >> 4,
            ((max_x + 15) >> 4) - 1,
            (min_y + 15) >> 4,
            ((max_y + 15) >> 4) - 1,
        ],
        _ => [(min_x + 15) >> 4, max_x >> 4, (min_y + 15) >> 4, max_y >> 4],
    };
    // Antialiased edges reach a pixel further on every side.
    let [x0, x1, y0, y1] = if p.aa1 {
        [x0 - 1, x1 + 1, y0 - 1, y1 + 1]
    } else {
        [x0, x1, y0, y1]
    };
    let [sx0, sx1, sy0, sy1] = p.scissor;
    let b = [
        x0.max(sx0).max(0),
        x1.min(sx1),
        y0.max(sy0).max(0),
        y1.min(sy1),
    ];
    (b[0] <= b[1] && b[2] <= b[3]).then_some(b)
}

/// Draws primitives, or the parts of them inside a clip rectangle, into
/// local memory.
pub(crate) struct Raster<'a> {
    pub(crate) mem: &'a Memory,
    /// For a primitive that textures from blocks it writes: those blocks
    /// as they were before it, which it reads its texture from.
    pub(crate) tex: Option<&'a Snapshot>,
}

impl Raster<'_> {
    /// Draws the part of `prim` inside `clip` (x0, x1, y0, y1, inclusive;
    /// within its bounding box).
    pub(crate) fn prim(&self, prim: &Prim, clip: [i32; 4]) {
        let p = &prim.pipe;
        let v = &prim.v;
        let at = |v: &Vertex| (v.x as i32 - p.offset.0, v.y as i32 - p.offset.1);
        match prim.kind {
            0 => {
                let (x, y) = point(at(&v[0]));
                if (clip[0]..=clip[1]).contains(&x) && (clip[2]..=clip[3]).contains(&y) {
                    self.fragment::<false>(p, x, y, attrs(p, &v[0]));
                }
            }
            1 | 2 if p.aa1 => self.aa_line(p, clip, at(&v[0]), at(&v[1]), &v[0], &v[1]),
            1 | 2 => self.line(p, clip, at(&v[0]), at(&v[1]), &v[0], &v[1]),
            3..=5 => {
                let pos = [at(&v[0]), at(&v[1]), at(&v[2])];
                if p.hot {
                    self.triangle::<true>(p, clip, pos, v);
                } else {
                    self.triangle::<false>(p, clip, pos, v);
                }
            }
            _ => self.sprite(p, clip, at(&v[0]), at(&v[1]), &v[0], &v[1]),
        }
    }

    fn sprite(
        &self,
        p: &Pipe,
        clip: [i32; 4],
        a: (i32, i32),
        b: (i32, i32),
        va: &Vertex,
        vb: &Vertex,
    ) {
        let fb = attrs(p, vb);
        let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
        let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
        if x0 == x1 || y0 == y1 {
            return;
        }
        if let Some(fill) = Fill::of(p, &fb) {
            let [cx0, cx1, cy0, cy1] = clip;
            let px0 = ((x0 + 15) >> 4).max(cx0);
            let px1 = ((x1 + 15) >> 4).min(cx1 + 1);
            let py0 = ((y0 + 15) >> 4).max(cy0);
            let py1 = ((y1 + 15) >> 4).min(cy1 + 1);
            for y in py0..py1 {
                if y < 0 || matches!((p.scanmsk, y & 1), (2, 0) | (3, 1)) {
                    continue;
                }
                for x in px0.max(0)..px1 {
                    self.fill(p, &fill, x as u32, y as u32);
                }
            }
            return;
        }
        sprite_frags(p, clip, a, b, va, vb, &mut |x, y, f| {
            self.fragment::<false>(p, x, y, f);
        });
    }

    /// A triangle; `HOT` when `p` is in the specialised state.
    fn triangle<const HOT: bool>(
        &self,
        p: &Pipe,
        clip: [i32; 4],
        pos: [(i32, i32); 3],
        v: &[Vertex; 3],
    ) {
        // The last vertex gives flat colour; keep it before reordering.
        let f = [attrs(p, &v[0]), attrs(p, &v[1]), attrs(p, &v[2])];
        let last = f[2];
        // As given, for antialiased edges.
        let (given_pos, given) = (pos, f);
        let edge = |a: (i32, i32), b: (i32, i32), x: i64, y: i64| {
            i64::from(b.0 - a.0) * (y - i64::from(a.1))
                - i64::from(b.1 - a.1) * (x - i64::from(a.0))
        };
        let area = edge(pos[0], pos[1], i64::from(pos[2].0), i64::from(pos[2].1));
        if area == 0 {
            return;
        }
        // Make the winding clockwise in screen space (y down): positive area.
        let (pos, f, area) = if area < 0 {
            ([pos[0], pos[2], pos[1]], [f[0], f[2], f[1]], -area)
        } else {
            (pos, f, area)
        };
        let [cx0, cx1, cy0, cy1] = clip;
        let left = (pos.iter().map(|q| q.0).min().unwrap_or(0) + 15) >> 4;
        let right = pos.iter().map(|q| q.0).max().unwrap_or(0) >> 4;
        let top = (pos.iter().map(|q| q.1).min().unwrap_or(0) + 15) >> 4;
        let bottom = pos.iter().map(|q| q.1).max().unwrap_or(0) >> 4;
        // Top-left rule: an edge going up (in this winding) or a horizontal
        // top edge owns the samples on it.
        let owns = |a: (i32, i32), b: (i32, i32)| {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            dy < 0 || (dy == 0 && dx > 0)
        };
        let edges = [(pos[1], pos[2]), (pos[2], pos[0]), (pos[0], pos[1])];
        let owned = edges.map(|(a, b)| owns(a, b));
        let flat_z = f[0].z == f[1].z && f[1].z == f[2].z;
        // Texture coordinates by the shared integer arithmetic (`fixed`):
        // UV in 1/16 texel at the vertices, or S, T, Q scaled.
        let tex = if !(HOT || p.tme) {
            Tex::None
        } else if p.fst {
            Tex::Uv(
                f.map(|v| (v.uv.0 * 16.0) as i64),
                f.map(|v| (v.uv.1 * 16.0) as i64),
            )
        } else {
            Tex::Stq(fixed::Stq::new(f.map(|v| v.stq), p.tex.tw, p.tex.th))
        };
        let (x0, x1) = (left.max(cx0), right.min(cx1));
        for y in top.max(cy0)..=bottom.min(cy1) {
            let sy = i64::from(y) * 16;
            // Edge values at the row's first sample; each step right by a
            // pixel changes them by a constant.
            let e0 = edges.map(|(a, b)| edge(a, b, i64::from(x0) * 16, sy));
            let step = edges.map(|(a, b)| -i64::from(b.1 - a.1) * 16);
            let Some((from, to)) = row_span(e0, step, owned, i64::from(x1 - x0)) else {
                continue;
            };
            let mut e = std::array::from_fn::<i64, 3, _>(|k| e0[k] + step[k] * from);
            for x in x0 + from as i32..=x0 + to as i32 {
                let w = e;
                for k in 0..3 {
                    e[k] += step[k];
                }
                if (0..3).any(|k| w[k] < 0 || (w[k] == 0 && !owned[k])) {
                    continue;
                }
                // Depth, colour and fog from the exact integer weights
                // (they sum to the area), so a value all three vertices
                // share comes out exactly.
                let exact = |a: i64, b: i64, c: i64| (w[0] * a + w[1] * b + w[2] * c) / area;
                let z = if flat_z {
                    f[0].z
                } else {
                    weighted_depth(w, f.map(|v| v.z), area)
                };
                let rgba = if HOT || p.iip {
                    std::array::from_fn(|k| {
                        exact(
                            f[0].rgba[k].into(),
                            f[1].rgba[k].into(),
                            f[2].rgba[k].into(),
                        ) as i32
                    })
                } else {
                    last.rgba
                };
                let fog = exact(f[0].f.into(), f[1].f.into(), f[2].f.into()) as i32;
                let tex = match &tex {
                    Tex::None => (0, 0),
                    Tex::Uv(u, v) => {
                        let at = |c: &[i64; 3]| {
                            (w[0] * c[0] + w[1] * c[1] + w[2] * c[2]).div_euclid(area) as i32
                        };
                        (at(u), at(v))
                    }
                    Tex::Stq(stq) => stq.at(w),
                };
                self.fragment::<HOT>(
                    p,
                    x,
                    y,
                    Frag {
                        z,
                        rgba,
                        tex,
                        f: fog,
                        ..last
                    },
                );
            }
        }
        if !HOT && p.aa1 {
            self.aa_edges(p, clip, given_pos, &given, &last);
        }
    }

    /// An antialiased triangle's edges (`AA1`), after its inside.
    fn aa_edges(
        &self,
        p: &Pipe,
        clip: [i32; 4],
        pos: [(i32, i32); 3],
        given: &[Frag; 3],
        last: &Frag,
    ) {
        aa_edge_frags(p, clip, pos, given, last, &mut |x, y, f| {
            self.fragment::<false>(p, x, y, f);
        });
    }

    /// An antialiased line (`AA1`).
    fn aa_line(
        &self,
        p: &Pipe,
        clip: [i32; 4],
        a: (i32, i32),
        b: (i32, i32),
        va: &Vertex,
        vb: &Vertex,
    ) {
        aa_line_frags(p, clip, a, b, va, vb, &mut |x, y, f| {
            self.fragment::<false>(p, x, y, f);
        });
    }

    fn line(
        &self,
        p: &Pipe,
        clip: [i32; 4],
        a: (i32, i32),
        b: (i32, i32),
        va: &Vertex,
        vb: &Vertex,
    ) {
        line_frags(p, clip, a, b, va, vb, &mut |x, y, f| {
            self.fragment::<false>(p, x, y, f);
        });
    }

    /// The texel at (`u`, `v`), already wrapped, as RGBA8888.
    fn texel<const HOT: bool>(&self, p: &Pipe, u: i32, v: i32) -> u32 {
        let (Some(psm), Some(layout)) = (p.tex_psm, &p.tex_layout) else {
            return 0;
        };
        let at = layout.locate((u & 0x7FF) as u32, (v & 0x7FF) as u32);
        let raw = match self.tex {
            Some(snapshot) => snapshot.get(self.mem, at),
            None => self.mem.get(at),
        };
        if HOT {
            // A CLUT format.
            return clut_color(&p.clut, p.texa, &p.tex, raw);
        }
        match psm {
            Psm::Ct32 | Psm::Z32 => raw,
            Psm::Ct24 | Psm::Z24 => expand24(p.texa, raw),
            Psm::Ct16 | Psm::Ct16s | Psm::Z16 | Psm::Z16s => expand16(p.texa, raw),
            _ => clut_color(&p.clut, p.texa, &p.tex, raw),
        }
    }

    fn sample<const HOT: bool>(&self, p: &Pipe, (u, v): (i32, i32)) -> [i32; 4] {
        let channels =
            |c: u32| [c & 0xFF, (c >> 8) & 0xFF, (c >> 16) & 0xFF, c >> 24].map(|x| x as i32);
        let [wu, wv] = &p.wrap;
        if !HOT && !p.linear {
            let (u, v) = (wu.apply(u >> 4), wv.apply(v >> 4));
            return channels(self.texel::<HOT>(p, u, v));
        }
        // Bilinear, with 4-bit weights: across, then down, each step
        // truncated (as PCSX2's software renderer does).
        let (u, v) = (u.wrapping_sub(8), v.wrapping_sub(8));
        let (iu, iv, fu, fv) = (u >> 4, v >> 4, u & 15, v & 15);
        // Each column and row wrapped once.
        let (u0, u1, v0, v1) = (
            wu.apply(iu),
            wu.apply(iu + 1),
            wv.apply(iv),
            wv.apply(iv + 1),
        );
        // A zero weight takes nothing from the texel it weighs (`lerp(a, b,
        // 0)` is `a`), so that texel is not fetched.
        let t00 = channels(self.texel::<HOT>(p, u0, v0));
        let t10 = if fu == 0 {
            t00
        } else {
            channels(self.texel::<HOT>(p, u1, v0))
        };
        let (t01, t11) = if fv == 0 {
            (t00, t10)
        } else {
            let t01 = channels(self.texel::<HOT>(p, u0, v1));
            let t11 = if fu == 0 {
                t01
            } else {
                channels(self.texel::<HOT>(p, u1, v1))
            };
            (t01, t11)
        };
        let t = [t00, t10, t01, t11];
        let lerp = |a: i32, b: i32, f: i32| a + (((b - a) * f) >> 4);
        std::array::from_fn(|k| {
            let top = lerp(t[0][k], t[1][k], fu);
            let bottom = lerp(t[2][k], t[3][k], fu);
            lerp(top, bottom, fv)
        })
    }

    /// A pixel of a [`Fill`]: what [`Raster::fragment`] writes there.
    #[inline]
    fn fill(&self, p: &Pipe, fill: &Fill, x: u32, y: u32) {
        if let (Some(z), Some(layout)) = (fill.depth, &p.depth_layout) {
            self.mem.set(layout.locate(x, y), z);
        }
        let at = p.frame_layout.locate(x, y);
        if fill.mask == 0 {
            self.mem.set(at, fill.value);
        } else {
            let dst = self.mem.get(at);
            self.mem
                .set(at, (fill.value & !fill.mask) | (dst & fill.mask));
        }
    }

    /// One fragment through the pixel pipeline.
    #[allow(clippy::too_many_lines, reason = "the pipeline in hardware order")]
    ///
    /// `HOT` when `p` is in the specialised state ([`Pipe::is_hot`]): what
    /// that state decides is a constant, the rest as for any state.
    fn fragment<const HOT: bool>(&self, p: &Pipe, x: i32, y: i32, frag: Frag) {
        if x < 0 || y < 0 {
            return;
        }
        if !HOT {
            match p.scanmsk {
                2 if y & 1 == 0 => return,
                3 if y & 1 == 1 => return,
                _ => {}
            }
        }
        let (x, y) = (x as u32, y as u32);
        // The tests that need no texture first, so a fragment they reject
        // costs no sampling; the result is the same in either order.
        let t = p.test;
        let fpsm = if HOT { Psm::Ct32 } else { p.frame.0 };
        let at = p.frame_layout.locate(x, y);
        let dst = self.mem.get(at);
        // Destination alpha test.
        if !HOT && bits(t, 14, 1) != 0 {
            let bit = match fpsm {
                Psm::Ct32 => dst >> 31,
                Psm::Ct16 | Psm::Ct16s => (dst >> 15) & 1,
                _ => bits(t, 15, 1),
            };
            if bit != bits(t, 15, 1) {
                return;
            }
        }
        // Depth test; the depth is written once the alpha test allows.
        let mut depth = None;
        if let (Some((zpsm, _)), Some(layout)) = (p.zbuf, &p.depth_layout) {
            let max = match zpsm {
                Psm::Z24 => 0x00FF_FFFF,
                Psm::Z16 | Psm::Z16s => 0xFFFF,
                _ => u32::MAX,
            };
            let z = frag.z.min(max);
            let z_at = layout.locate(x, y);
            let old = self.mem.get(z_at);
            let pass = match bits(t, 17, 2) {
                0 => false,
                1 => true,
                2 => z >= old,
                _ => z > old,
            };
            if !pass {
                return;
            }
            depth = Some((z_at, z));
        }
        let mut c = frag.rgba;
        if HOT || p.tme {
            let t = self.sample::<HOT>(p, frag.tex);
            let (tcc, va) = (HOT || p.tex.tcc, c[3]);
            let modulate = |t: i32, v: i32| ((t * v) >> 7).min(255);
            match if HOT { 0 } else { p.tex.tfx } {
                0 => {
                    for k in 0..3 {
                        c[k] = modulate(t[k], c[k]);
                    }
                    if tcc {
                        c[3] = modulate(t[3], va);
                    }
                }
                1 => {
                    c[..3].copy_from_slice(&t[..3]);
                    if tcc {
                        c[3] = t[3];
                    }
                }
                tfx => {
                    for k in 0..3 {
                        c[k] = (modulate(t[k], c[k]) + va).min(255);
                    }
                    if tcc {
                        c[3] = if tfx == 2 { (t[3] + va).min(255) } else { t[3] };
                    }
                }
            }
        }
        if !HOT && p.aa1 {
            // Coverage alpha (as PCSX2's software renderer): an edge's
            // share of the pixel, else 0x80; it replaces alpha, or with
            // blending on only an alpha of 0x80.
            let a = frag.coverage.map_or(0x80, |c| (c >> 9) as i32);
            if !p.abe || c[3] == 0x80 {
                c[3] = a;
            }
        }
        if HOT || p.fge {
            let fc = [
                bits(p.fogcol, 0, 8),
                bits(p.fogcol, 8, 8),
                bits(p.fogcol, 16, 8),
            ];
            for k in 0..3 {
                c[k] = (frag.f * c[k] + (255 - frag.f) * fc[k] as i32) >> 8;
            }
        }
        // Alpha test: which writes survive.
        // Antialiased edges test depth but never write it.
        let (mut write_fb, mut write_z, mut write_alpha) =
            (true, !p.zmsk && frag.coverage.is_none(), true);
        if bits(t, 0, 1) != 0 {
            let (a, aref) = (c[3], bits(t, 4, 8) as i32);
            let pass = match bits(t, 1, 3) {
                0 => false,
                1 => true,
                2 => a < aref,
                3 => a <= aref,
                4 => a == aref,
                5 => a >= aref,
                6 => a > aref,
                _ => a != aref,
            };
            if !pass {
                match bits(t, 12, 2) {
                    0 => return,
                    1 => write_z = false,
                    2 => write_fb = false,
                    _ => {
                        write_z = false;
                        write_alpha = false;
                    }
                }
            }
        }
        if write_z && let Some((z_at, z)) = depth {
            self.mem.set(z_at, z);
        }
        if !write_fb {
            return;
        }
        let d = match fpsm {
            Psm::Ct16 | Psm::Ct16s => {
                let c = expand16(p.texa, dst & 0x7FFF);
                (c & 0x00FF_FFFF) | (if dst & 0x8000 != 0 { 0x8000_0000 } else { 0 })
            }
            Psm::Ct24 => dst | 0x8000_0000,
            _ => dst,
        };
        let dc = [d & 0xFF, (d >> 8) & 0xFF, (d >> 16) & 0xFF, d >> 24].map(|v| v as i32);
        // AA1 blends (with ALPHA) even without ABE, as PCSX2 does.
        if (HOT || p.abe || p.aa1) && (!p.pabe || c[3] & 0x80 != 0) {
            let a = p.alpha;
            let cs = c;
            let pick = |sel: u32, k: usize| match sel {
                0 => cs[k],
                1 => dc[k],
                _ => 0,
            };
            let coeff = match bits(a, 4, 2) {
                0 => cs[3],
                1 => dc[3],
                _ => bits(a, 32, 8) as i32,
            };
            for (k, ck) in c.iter_mut().take(3).enumerate() {
                *ck = (((pick(bits(a, 0, 2), k) - pick(bits(a, 2, 2), k)) * coeff) >> 7)
                    + pick(bits(a, 6, 2), k);
            }
        }
        for ck in c.iter_mut().take(3) {
            *ck = if p.colclamp {
                (*ck).clamp(0, 255)
            } else {
                *ck & 0xFF
            };
        }
        if !write_alpha {
            c[3] = dc[3];
        }
        let mut a = c[3] as u32 & 0xFF;
        if p.fba {
            a |= 0x80;
        }
        let rgba = c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16 | a << 24;
        let fbmsk = if HOT { 0 } else { p.fbmsk };
        let (new, mask) = match fpsm {
            Psm::Ct16 | Psm::Ct16s => (pack16(rgba), pack16(fbmsk)),
            Psm::Ct24 => (rgba & 0x00FF_FFFF, fbmsk | 0xFF00_0000),
            _ => (rgba, fbmsk),
        };
        self.mem.set(at, (new & !mask) | (dst & mask));
    }
}

/// An antialiased triangle's edges (`AA1`), inside `clip`: PCSX2's walk,
/// each pixel with the share of it the triangle covers, handed to `emit`.
fn aa_edge_frags(
    p: &Pipe,
    clip: [i32; 4],
    pos: [(i32, i32); 3],
    given: &[Frag; 3],
    last: &Frag,
    emit: &mut impl FnMut(i32, i32, Frag),
) {
    crate::aa::triangle(pos, p.scissor, &mut |e| {
        if (clip[0]..=clip[1]).contains(&e.x) && (clip[2]..=clip[3]).contains(&e.y) {
            let frag = edge_frag(p, &given[e.from], &given[e.to], e.t, last);
            emit(
                e.x,
                e.y,
                Frag {
                    coverage: Some(e.coverage),
                    ..frag
                },
            );
        }
    });
}

/// An antialiased line (`AA1`), inside `clip`: PCSX2's walk, two pixels a
/// step, every one an edge pixel, handed to `emit`.
fn aa_line_frags(
    p: &Pipe,
    clip: [i32; 4],
    a: (i32, i32),
    b: (i32, i32),
    va: &Vertex,
    vb: &Vertex,
    emit: &mut impl FnMut(i32, i32, Frag),
) {
    let (fa, fb) = (attrs(p, va), attrs(p, vb));
    crate::aa::line(a, b, p.scissor, &mut |e| {
        if (clip[0]..=clip[1]).contains(&e.x) && (clip[2]..=clip[3]).contains(&e.y) {
            let frag = edge_frag(p, &fa, &fb, e.t, &fb);
            emit(
                e.x,
                e.y,
                Frag {
                    coverage: Some(e.coverage),
                    ..frag
                },
            );
        }
    });
}

/// A line's fragments inside `clip`, handed to `emit`.
fn line_frags(
    p: &Pipe,
    clip: [i32; 4],
    a: (i32, i32),
    b: (i32, i32),
    va: &Vertex,
    vb: &Vertex,
    emit: &mut impl FnMut(i32, i32, Frag),
) {
    let (fa, fb) = (attrs(p, va), attrs(p, vb));
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let steps = (dx.abs().max(dy.abs()) + 15) >> 4;
    let [cx0, cx1, cy0, cy1] = clip;
    for i in 0..steps.max(1) {
        let t = i as f32 / steps.max(1) as f32;
        let x = (a.0 as f32 + dx as f32 * t) as i32;
        let y = (a.1 as f32 + dy as f32 * t) as i32;
        let (px, py) = ((x + 8) >> 4, (y + 8) >> 4);
        if px < cx0 || px > cx1 || py < cy0 || py > cy1 {
            continue;
        }
        let mix = |a: i32, b: i32| a + ((b - a) as f32 * t) as i32;
        let rgba = if p.iip {
            std::array::from_fn(|k| mix(fa.rgba[k], fb.rgba[k]))
        } else {
            fb.rgba
        };
        let z = (f64::from(fa.z) + (f64::from(fb.z) - f64::from(fa.z)) * f64::from(t)) as u32;
        let uv = (
            fa.uv.0 + (fb.uv.0 - fa.uv.0) * t,
            fa.uv.1 + (fb.uv.1 - fa.uv.1) * t,
        );
        emit(
            px,
            py,
            Frag {
                z,
                rgba,
                uv,
                tex: texcoord(p, uv),
                f: mix(fa.f, fb.f),
                ..fb
            },
        );
    }
}

/// A textured sprite's fragments inside `clip` (corners `a` and `b`),
/// handed to `emit`.
fn sprite_frags(
    p: &Pipe,
    clip: [i32; 4],
    a: (i32, i32),
    b: (i32, i32),
    va: &Vertex,
    vb: &Vertex,
    emit: &mut impl FnMut(i32, i32, Frag),
) {
    let (fa, fb) = (attrs(p, va), attrs(p, vb));
    let ((x0, u0), (x1, u1)) = if a.0 <= b.0 {
        ((a.0, fa.uv.0), (b.0, fb.uv.0))
    } else {
        ((b.0, fb.uv.0), (a.0, fa.uv.0))
    };
    let ((y0, v0), (y1, v1)) = if a.1 <= b.1 {
        ((a.1, fa.uv.1), (b.1, fb.uv.1))
    } else {
        ((b.1, fb.uv.1), (a.1, fa.uv.1))
    };
    if x0 == x1 || y0 == y1 {
        return;
    }
    let [cx0, cx1, cy0, cy1] = clip;
    let px0 = ((x0 + 15) >> 4).max(cx0);
    let px1 = ((x1 + 15) >> 4).min(cx1 + 1);
    let py0 = ((y0 + 15) >> 4).max(cy0);
    let py1 = ((y1 + 15) >> 4).min(cy1 + 1);
    let at = |c0, c1, s0, s1, s| sprite_coord(p.fst, c0, c1, s0, s1, s);
    // U depends on the column alone: once per column, not per pixel.
    let us: Vec<i32> = (px0..px1)
        .map(|x| texcoord(p, (at(u0, u1, x0, x1, x * 16), 0.0)).0)
        .collect();
    for y in py0..py1 {
        let v = texcoord(p, (0.0, at(v0, v1, y0, y1, y * 16))).1;
        for (x, &u) in (px0..px1).zip(&us) {
            emit(x, y, Frag { tex: (u, v), ..fb });
        }
    }
}

/// A primitive whose every pixel the pipeline writes alike: no texture,
/// fog, blending, alpha or destination-alpha test, and a depth test that
/// passes everything (or none), so each pixel gets the same frame value
/// (merged under FBMSK) and the same depth.
struct Fill {
    value: u32,
    mask: u32,
    /// The depth written, if it is.
    depth: Option<u32>,
}

impl Fill {
    /// The fill `frag`'s pixels make, if they make one.
    fn of(p: &Pipe, frag: &Frag) -> Option<Self> {
        let t = p.test;
        let ztst = bits(t, 17, 2);
        if p.tme
            || p.fge
            || p.abe
            || p.aa1
            || bits(t, 0, 1) != 0
            || bits(t, 14, 1) != 0
            || (p.zbuf.is_some() && ztst != 1)
        {
            return None;
        }
        let (fpsm, _, _) = p.frame;
        let depth = p.zbuf.filter(|_| !p.zmsk).map(|(zpsm, _)| {
            let max = match zpsm {
                Psm::Z24 => 0x00FF_FFFF,
                Psm::Z16 | Psm::Z16s => 0xFFFF,
                _ => u32::MAX,
            };
            frag.z.min(max)
        });
        // As `fragment` with every stage off: COLCLAMP or wrapping, FBA.
        let c = frag.rgba.map(|v| {
            if p.colclamp {
                v.clamp(0, 255)
            } else {
                v & 0xFF
            }
        });
        let mut a = frag.rgba[3] as u32 & 0xFF;
        if p.fba {
            a |= 0x80;
        }
        let rgba = c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16 | a << 24;
        let (value, mask) = match fpsm {
            Psm::Ct16 | Psm::Ct16s => (pack16(rgba), pack16(p.fbmsk)),
            Psm::Ct24 => (rgba & 0x00FF_FFFF, p.fbmsk | 0xFF00_0000),
            _ => (rgba, p.fbmsk),
        };
        Some(Self { value, mask, depth })
    }
}

/// A sprite's texture coordinate at sample position `s` (1/16 pixel),
/// between corners at `s0` and `s1` (`s0 < s1`) with coordinates `c0` and
/// `c1`. UV (FST) is fixed point, 1/16 texel, and interpolates exactly;
/// S/T/Q in floats.
fn sprite_coord(fst: bool, c0: f32, c1: f32, s0: i32, s1: i32, s: i32) -> f32 {
    if fst {
        let (c0, c1) = ((c0 * 16.0) as i64, (c1 * 16.0) as i64);
        let c = c0 + ((c1 - c0) * i64::from(s - s0)).div_euclid(i64::from(s1 - s0));
        c as f32 / 16.0
    } else {
        c0 + (c1 - c0) / (s1 - s0) as f32 * (s - s0) as f32
    }
}

/// Attributes `t` of the way from `a` to `b` along an antialiased edge
/// (held to the edge's ends); flat colour from `last`.
fn edge_frag(p: &Pipe, a: &Frag, b: &Frag, t: f32, last: &Frag) -> Frag {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: i32, b: i32| a + ((b - a) as f32 * t) as i32;
    let rgba = if p.iip {
        std::array::from_fn(|k| mix(a.rgba[k], b.rgba[k]))
    } else {
        last.rgba
    };
    let z = (f64::from(a.z) + (f64::from(b.z) - f64::from(a.z)) * f64::from(t)) as u32;
    let uv = if p.tme {
        let (tw, th) = (
            f64::from(1u16 << p.tex.tw.min(10)),
            f64::from(1u16 << p.tex.th.min(10)),
        );
        let t = f64::from(t);
        tex_coords(p, &[*a, *b, *b], [1.0 - t, t, 0.0], (tw, th))
    } else {
        (0.0, 0.0)
    };
    Frag {
        z,
        rgba,
        uv,
        tex: texcoord(p, uv),
        f: mix(a.f, b.f),
        coverage: None,
        ..*last
    }
}

/// Texture coordinates in texels as sampling takes them, in 1/16 texel:
/// what the float coordinates gave before, for each sampling mode
/// (nearest: the texel `u` lies in; bilinear: `u·16` truncated).
fn texcoord(p: &Pipe, (u, v): (f32, f32)) -> (i32, i32) {
    if p.linear {
        ((u * 16.0) as i32, (v * 16.0) as i32)
    } else {
        (
            (u.floor() as i32).saturating_mul(16),
            (v.floor() as i32).saturating_mul(16),
        )
    }
}

/// How a triangle's texture coordinates are interpolated.
enum Tex {
    None,
    /// UV (FST): the vertices' U and V in 1/16 texel.
    Uv([i64; 3], [i64; 3]),
    /// STQ.
    Stq(fixed::Stq),
}

/// A vertex's attributes as a fragment's.
fn attrs(p: &Pipe, v: &Vertex) -> Frag {
    let (w, h) = (
        f32::from(1u16 << p.tex.tw.min(10)),
        f32::from(1u16 << p.tex.th.min(10)),
    );
    let uv = if p.fst {
        (v.u as f32 / 16.0, v.v as f32 / 16.0)
    } else {
        let q = if v.q == 0.0 { f32::MIN_POSITIVE } else { v.q };
        (v.s / q * w, v.t / q * h)
    };
    Frag {
        z: v.z,
        rgba: v.rgba.map(i32::from),
        uv,
        tex: texcoord(p, uv),
        stq: [v.s, v.t, v.q],
        f: i32::from(v.f),
        coverage: None,
    }
}

/// The pixels of a triangle's row worth testing, relative to its first:
/// from the edge equations (an edge owned or not needs a value of at least
/// 0 or 1), one pixel wider on each side; the per-pixel test decides
/// exactly.
/// Depth from the exact integer weights `w` (summing to `area`) of the
/// vertices' depths `z`: in 64 bits when the sum fits (depths of 24 bits or
/// less always do), else in 128.
fn weighted_depth(w: [i64; 3], z: [u32; 3], area: i64) -> u32 {
    let sum64 = (0..3).try_fold(0i64, |sum, k| {
        w[k].checked_mul(i64::from(z[k]))
            .and_then(|t| sum.checked_add(t))
    });
    if let Some(sum) = sum64 {
        (sum / area) as u32
    } else {
        let sum: i128 = (0..3).map(|k| i128::from(w[k]) * i128::from(z[k])).sum();
        (sum / i128::from(area)) as u32
    }
}

fn row_span(e0: [i64; 3], step: [i64; 3], owned: [bool; 3], last: i64) -> Option<(i64, i64)> {
    let (mut from, mut to) = (0, last);
    for k in 0..3 {
        let need = i64::from(!owned[k]) - e0[k];
        match step[k].cmp(&0) {
            std::cmp::Ordering::Greater => from = from.max(need.div_euclid(step[k]) - 1),
            std::cmp::Ordering::Less => to = to.min((-need).div_euclid(-step[k]) + 1),
            std::cmp::Ordering::Equal if need > 0 => return None,
            std::cmp::Ordering::Equal => {}
        }
    }
    (from <= to).then_some((from, to))
}

/// Texture coordinates at a triangle's sample with barycentric weights `b`:
/// UV interpolated, or S, T and Q interpolated then divided (perspective).
fn tex_coords(p: &Pipe, f: &[Frag; 3], b: [f64; 3], (tw, th): (f64, f64)) -> (f32, f32) {
    let lerp =
        |g: [f32; 3]| b[0] * f64::from(g[0]) + b[1] * f64::from(g[1]) + b[2] * f64::from(g[2]);
    if p.fst {
        (
            lerp([f[0].uv.0, f[1].uv.0, f[2].uv.0]) as f32,
            lerp([f[0].uv.1, f[1].uv.1, f[2].uv.1]) as f32,
        )
    } else {
        let s = lerp([f[0].stq[0], f[1].stq[0], f[2].stq[0]]);
        let t = lerp([f[0].stq[1], f[1].stq[1], f[2].stq[1]]);
        let q = lerp([f[0].stq[2], f[1].stq[2], f[2].stq[2]]);
        let q = if q == 0.0 { f64::MIN_POSITIVE } else { q };
        ((s / q * tw) as f32, (t / q * th) as f32)
    }
}

/// RGBA8888 as RGB5A1.
fn pack16(c: u32) -> u32 {
    ((c >> 3) & 0x1F) | (((c >> 11) & 0x1F) << 5) | (((c >> 19) & 0x1F) << 10) | ((c >> 31) << 15)
}

#[cfg(test)]
mod tests {
    use crate::Gs;
    use crate::Psm;
    use crate::regs::addr;

    /// A [`super::Fill`] writes, pixel for pixel, what the pipeline does:
    /// frames of 32, 24 and 16 bits, FBMSK, FBA, COLCLAMP, depth of 32 and
    /// 16 bits written or masked, over memory full of other values.
    #[test]
    fn fills_write_what_the_pipeline_writes() {
        use super::{Fill, Frag, Raster};
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for frame_psm in [0x00u64, 0x01, 0x02, 0x0A] {
            for zbuf in [
                None,
                Some((0x00u64, false)),
                Some((0x02, true)),
                Some((0x02, false)),
            ] {
                for (fbmsk, fba, colclamp) in [(0, false, true), (0x00F0_F00F, true, false)] {
                    let mut gs = Gs::default();
                    let random: Vec<u8> = (0..crate::mem::SIZE).map(|_| next() as u8).collect();
                    gs.mem.load_bytes(&random);
                    gs.write(addr::FRAME_1, 4 | 2 << 16 | frame_psm << 24 | fbmsk << 32);
                    if let Some((zpsm, zmsk)) = zbuf {
                        gs.write(addr::ZBUF_1, 0x28 | zpsm << 24 | u64::from(zmsk) << 32);
                        gs.write(addr::TEST_1, 1 << 16 | 1 << 17);
                    }
                    gs.write(addr::FBA_1, u64::from(fba));
                    gs.write(addr::COLCLAMP, u64::from(colclamp));
                    let p = gs.pipe();
                    let frag = Frag {
                        z: next() as u32,
                        rgba: [0x12, 0xF0, 0x7F, 0x81],
                        uv: (0.0, 0.0),
                        tex: (0, 0),
                        stq: [0.0; 3],
                        f: 0,
                        coverage: None,
                    };
                    let fill = Fill::of(&p, &frag).expect("a fill");
                    let reference = Gs::default();
                    reference.mem.load_bytes(&random);
                    for _ in 0..200 {
                        let (x, y) = ((next() % 128) as u32, (next() % 64) as u32);
                        Raster {
                            mem: &gs.mem,
                            tex: None,
                        }
                        .fill(&p, &fill, x, y);
                        Raster {
                            mem: &reference.mem,
                            tex: None,
                        }
                        .fragment::<false>(&p, x as i32, y as i32, frag);
                    }
                    assert!(
                        gs.mem.to_bytes() == reference.mem.to_bytes(),
                        "frame {frame_psm:#x} zbuf {zbuf:?} fbmsk {fbmsk:#x} fba {fba}"
                    );
                }
            }
        }
    }

    /// Triangles in the specialised state ([`super::Pipe::is_hot`]) draw
    /// what the general pipeline draws, over random memory and CLUTs: every
    /// CLUT texture format and CLUT format, wrap modes, UV and STQ, depth
    /// formats and tests, alpha tests, blend inputs, PABE, COLCLAMP, FBA.
    #[test]
    fn the_specialised_pipeline_draws_what_the_general_one_draws() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut changed = 0;
        for case in 0..48u64 {
            let random: Vec<u8> = (0..crate::mem::SIZE).map(|_| next() as u8).collect();
            let clut: Vec<u16> = (0..512).map(|_| next() as u16).collect();
            let tex_psm = [0x13u64, 0x14, 0x1B, 0x24, 0x2C][(case % 5) as usize];
            let cpsm = if case % 3 == 0 { 2u64 } else { 0 };
            let zpsm = [0u64, 1, 2, 0xA][(case % 4) as usize];
            let ztst = 1 + next() % 3;
            let ate = match case % 4 {
                0 => 0,
                1 => 1 | 3 << 12,
                2 => 1 | 5 << 1 | 0x40 << 4 | 1 << 12,
                _ => 1 | (next() % 8) << 1 | (next() % 256) << 4 | (next() % 4) << 12,
            };
            let regs = [
                (addr::FRAME_1, 1 << 16),
                (addr::SCISSOR_1, 63 << 16 | 63 << 48),
                (addr::ZBUF_1, 0x40 | zpsm << 24 | (next() % 2) << 32),
                (addr::TEST_1, ate | 1 << 16 | ztst << 17),
                (addr::ALPHA_1, next() & 0xFF | (next() % 256) << 32),
                (
                    addr::TEX0_1,
                    0x80 | 2 << 14
                        | tex_psm << 20
                        | 5 << 26
                        | 5 << 30
                        | 1 << 34
                        | 0x100 << 37
                        | cpsm << 51
                        | (next() % 32) << 56,
                ),
                (addr::TEX1_1, 1 | 1 << 5),
                // Region bounds in order (min above max is another matter).
                (addr::CLAMP_1, {
                    let (u0, v0) = (next() % 16, next() % 16);
                    next() & 0xF
                        | u0 << 4
                        | (u0 + next() % 16) << 14
                        | v0 << 24
                        | (v0 + next() % 16) << 34
                }),
                (
                    addr::TEXA,
                    next() & 0xFF | (next() % 2) << 15 | (next() % 256) << 32,
                ),
                (addr::FOGCOL, next() & 0xFF_FFFF),
                (addr::PABE, next() % 2),
                (addr::COLCLAMP, next() % 2),
                (addr::FBA_1, next() % 2),
            ];
            let fst = next() % 2;
            let prim = 3 | 1 << 3 | 1 << 4 | 1 << 5 | 1 << 6 | fst << 8;
            let vertices: Vec<[u64; 5]> = (0..3 * 8)
                .map(|_| {
                    let q = 0.5 + (next() % 100) as f32 / 50.0;
                    [
                        next() & 0xFFFF_FFFF | u64::from(q.to_bits()) << 32,
                        u64::from(((next() % 64) as f32 * q / 32.0).to_bits())
                            | u64::from(((next() % 64) as f32 * q / 32.0).to_bits()) << 32,
                        (next() % (40 * 16)) | (next() % (40 * 16)) << 16,
                        (next() % 256) << 56,
                        (next() % (64 * 16))
                            | (next() % (64 * 16)) << 16
                            | (next() & 0xFFFF_FFFF) << 32,
                    ]
                })
                .collect();
            let draw = |specialise: bool| {
                let mut gs = Gs::default();
                gs.mem.load_bytes(&random);
                gs.clut.copy_from_slice(&clut);
                gs.specialise = specialise;
                for &(reg, value) in &regs {
                    gs.write(reg, value);
                }
                gs.write(addr::PRIM, prim);
                assert!(!specialise || gs.pipe().is_hot(), "case {case}");
                for v in &vertices {
                    gs.write(addr::RGBAQ, v[0]);
                    gs.write(addr::ST, v[1]);
                    gs.write(addr::UV, v[2]);
                    gs.write(addr::FOG, v[3]);
                    gs.write(addr::XYZ2, v[4]);
                }
                gs.mem.to_bytes()
            };
            let drawn = draw(true);
            assert!(drawn == draw(false), "case {case}");
            changed += usize::from(drawn != random);
        }
        // Depth and alpha tests reject some cases whole, but not most.
        assert!(changed >= 36, "{changed} of 48 cases drew anything");
    }

    /// A GS drawing to a 64-pixel-wide CT32 frame at block 0, scissor wide
    /// open, no depth test.
    fn gs() -> Gs {
        let mut gs = Gs::default();
        gs.write(addr::FRAME_1, 1 << 16);
        gs.write(addr::SCISSOR_1, 63 << 16 | 63 << 48);
        gs.write(addr::RGBAQ, 0x8011_2233);
        gs
    }

    fn xyz(gs: &mut Gs, x: u32, y: u32) {
        gs.write(addr::XYZ2, u64::from(x * 16) | u64::from(y * 16) << 16);
    }

    fn pixel(gs: &Gs, x: u32, y: u32) -> u32 {
        gs.mem.read(Psm::Ct32, 0, 1, x, y)
    }

    /// An antialiased triangle (AA1, no ABE) draws its inside as a plain
    /// one does, then blends its edge pixels by their coverage (with
    /// ALPHA, `(Cs - Cd) * As + Cd`): they land between the background and
    /// its colour. Nothing further out changes.
    #[test]
    fn antialiased_triangles_blend_their_edges_by_coverage() {
        let draw = |aa1: bool| {
            let mut gs = gs();
            gs.write(addr::ALPHA_1, 0x44);
            gs.write(addr::RGBAQ, 0x0040_4040);
            gs.write(addr::PRIM, 6);
            xyz(&mut gs, 0, 0);
            xyz(&mut gs, 64, 64);
            gs.write(addr::RGBAQ, 0x80C0_C0C0);
            gs.write(addr::PRIM, 3 | u64::from(aa1) << 7);
            for (x, y) in [(10, 10), (50, 14), (20, 50)] {
                xyz(&mut gs, x, y);
            }
            gs
        };
        let (plain, aa) = (draw(false), draw(true));
        let mut between = 0;
        for y in 0..64 {
            for x in 0..64 {
                let (p, a) = (
                    pixel(&plain, x, y) & 0xFF_FFFF,
                    pixel(&aa, x, y) & 0xFF_FFFF,
                );
                if p == 0xC0_C0C0 {
                    assert_eq!(a, p, "inside at ({x}, {y})");
                } else if a != p {
                    let r = a & 0xFF;
                    assert!((0x40..=0xC0).contains(&r), "({x}, {y}): {a:#x}");
                    between += 1;
                }
            }
        }
        assert!(between > 40, "{between} edge pixels");
        assert_eq!(pixel(&aa, 2, 2) & 0xFF_FFFF, 0x40_4040);
    }

    /// A sprite that textures from its own frame reads it as it was before
    /// the sprite, whatever order its pixels are drawn in: shifted right by
    /// a texel, a row moves along whole instead of smearing its first pixel.
    #[test]
    fn a_primitive_reads_its_own_frame_as_it_was_before_it() {
        for threads in [1, 4] {
            let mut gs = gs();
            gs.set_threads(threads);
            let value = |x: u32| 0x8000_0000 | (x * 0x01_0101);
            for x in 0..8 {
                gs.mem.write(Psm::Ct32, 0, 1, x, 0, value(x));
            }
            // TEX0: the frame itself (block 0, 64 wide, CT32), 64 × 64,
            // decal with its alpha.
            gs.write(
                addr::TEX0_1,
                1 << 14 | 6 << 26 | 6 << 30 | 1 << 34 | 1 << 35,
            );
            gs.write(addr::PRIM, 6 | 1 << 4 | 1 << 8);
            gs.write(addr::UV, 0);
            xyz(&mut gs, 1, 0);
            gs.write(addr::UV, (7 * 16) | (16 << 16));
            xyz(&mut gs, 8, 1);
            gs.flush();
            for x in 1..8 {
                assert_eq!(pixel(&gs, x, 0), value(x - 1), "{threads} threads, x {x}");
            }
        }
    }

    #[test]
    fn triangles_fill_their_inside_by_the_top_left_rule() {
        for winding in [[(0, 0), (8, 0), (0, 8)], [(0, 0), (0, 8), (8, 0)]] {
            let mut gs = gs();
            gs.write(addr::PRIM, 3);
            for (x, y) in winding {
                xyz(&mut gs, x, y);
            }
            assert_eq!(pixel(&gs, 1, 1), 0x8011_2233, "{winding:?}");
            // The top and left edges are in, the hypotenuse is out.
            assert_eq!(pixel(&gs, 0, 0), 0x8011_2233);
            assert_eq!(pixel(&gs, 4, 0), 0x8011_2233);
            assert_eq!(pixel(&gs, 4, 4), 0, "on the hypotenuse");
            assert_eq!(pixel(&gs, 8, 0), 0);
        }
    }

    #[test]
    fn sprites_cover_left_and_top_but_not_right_and_bottom() {
        let mut gs = gs();
        gs.write(addr::PRIM, 6);
        xyz(&mut gs, 2, 2);
        xyz(&mut gs, 6, 5);
        assert_eq!(pixel(&gs, 2, 2), 0x8011_2233);
        assert_eq!(pixel(&gs, 5, 4), 0x8011_2233);
        assert_eq!(pixel(&gs, 6, 4), 0);
        assert_eq!(pixel(&gs, 5, 5), 0);
    }

    #[test]
    fn gouraud_keeps_a_value_all_vertices_share() {
        // Sly 1 marks pixels with alpha 0x80 from a shaded fan, then tests
        // the destination alpha's top bit: 0x7F would be a different mark.
        let mut gs = gs();
        gs.write(addr::PRIM, 3 | 1 << 3);
        for (x, y, c) in [(3, 1, 0x10u64), (61, 7, 0x20), (17, 59, 0x30)] {
            gs.write(addr::RGBAQ, c | 0x80 << 24);
            xyz(&mut gs, x, y);
        }
        let mut covered = 0;
        for y in 0..64 {
            for x in 0..64 {
                let p = pixel(&gs, x, y);
                if p != 0 {
                    covered += 1;
                    assert_eq!(p >> 24, 0x80, "alpha at {x},{y}");
                }
            }
        }
        assert!(covered > 1000);
    }

    #[test]
    fn gouraud_interpolates_colour() {
        let mut gs = gs();
        gs.write(addr::PRIM, 3 | 1 << 3);
        for (x, y, c) in [(0, 0, 0u64), (32, 0, 0xFF), (0, 32, 0)] {
            gs.write(addr::RGBAQ, c | 0x8000_0000);
            xyz(&mut gs, x, y);
        }
        let r = |x| pixel(&gs, x, 0) & 0xFF;
        assert_eq!(r(0), 0);
        assert!(r(16) > 0x70 && r(16) < 0x90, "{:#x}", r(16));
    }
}
