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

use crate::regs::{Tex0, bits};
use crate::{Gs, Memory, Psm, Vertex, clut_color, expand16, expand24};

/// Everything a primitive's fragments need, decoded once.
#[allow(clippy::struct_excessive_bools, reason = "PRIM's attribute bits")]
#[derive(Clone)]
pub(crate) struct Pipe {
    tme: bool,
    fge: bool,
    abe: bool,
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
    clamp: u64,
    linear: bool,
    texa: u64,
    fogcol: u64,
    colclamp: bool,
    pabe: bool,
    scanmsk: u32,
    clut: Arc<[u16; 512]>,
}

impl Pipe {
    /// Whether the primitive reads its texture.
    pub(crate) const fn textured(&self) -> bool {
        self.tme
    }
}

/// A primitive ready to draw: its kind (PRIM's low bits), vertices, the
/// registers it reads, and the pixels it may touch (x0, x1, y0, y1,
/// inclusive; inside the scissor).
#[derive(Clone)]
pub(crate) struct Prim {
    pub(crate) pipe: Pipe,
    kind: u64,
    v: [Vertex; 3],
    pub(crate) bbox: [i32; 4],
}

/// A fragment's interpolated attributes.
#[derive(Clone, Copy)]
struct Frag {
    z: u32,
    rgba: [i32; 4],
    /// Texture coordinates, in texels.
    uv: (f32, f32),
    /// S, T and Q, for perspective interpolation.
    stq: [f32; 3],
    f: i32,
}

impl Gs {
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
            linear,
            texa: self.texa,
            fogcol: self.fogcol,
            colclamp: bits(self.colclamp, 0, 1) != 0,
            pabe: bits(self.pabe, 0, 1) != 0,
            scanmsk: bits(self.scanmsk, 0, 2),
            clut: Arc::clone(self.clut_shared.get_or_insert_with(|| Arc::new(self.clut))),
        }
    }

    /// Draws a primitive from its vertices: at once, or queued in a batch
    /// when the GS draws with several threads.
    pub(crate) fn draw(&mut self, v: &[Vertex]) {
        self.drawn += 1;
        let started = std::time::Instant::now();
        let pipe = self.pipe();
        let kind = self.prim & 7;
        let mut vertices = [Vertex::default(); 3];
        vertices[..v.len()].copy_from_slice(v);
        if let Some(bbox) = bounds(&pipe, kind, &vertices) {
            let prim = Prim {
                pipe,
                kind,
                v: vertices,
                bbox,
            };
            if self.batching() {
                self.queue(prim);
            } else {
                Raster { mem: &self.mem }.prim(&prim, prim.bbox);
            }
        }
        self.draw_time += started.elapsed();
    }
}

/// The pixels a primitive may touch (x0, x1, y0, y1, inclusive), inside
/// its scissor; `None` if there are none.
fn bounds(p: &Pipe, kind: u64, v: &[Vertex; 3]) -> Option<[i32; 4]> {
    let at = |v: &Vertex| (v.x as i32 - p.offset.0, v.y as i32 - p.offset.1);
    let pos: Vec<(i32, i32)> = match kind {
        0 => vec![at(&v[0])],
        1 | 2 | 6 => vec![at(&v[0]), at(&v[1])],
        _ => vec![at(&v[0]), at(&v[1]), at(&v[2])],
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
        0 => [
            (min_x + 15) >> 4,
            (min_x + 15) >> 4,
            (min_y + 15) >> 4,
            (min_y + 15) >> 4,
        ],
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
                let (x, y) = at(&v[0]);
                let frag = attrs(p, &v[0]);
                self.fragment(p, (x + 15) >> 4, (y + 15) >> 4, frag);
            }
            1 | 2 => self.line(p, clip, at(&v[0]), at(&v[1]), &v[0], &v[1]),
            3..=5 => self.triangle(p, clip, [at(&v[0]), at(&v[1]), at(&v[2])], v),
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
        // Texture coordinates at each sample point. UV (FST) is fixed
        // point, 1/16 texel, and interpolates exactly; S/T/Q in floats.
        let fst = p.fst;
        let at = |c0: f32, c1: f32, s0: i32, s1: i32, s: i32| -> f32 {
            if fst {
                let (c0, c1) = ((c0 * 16.0) as i64, (c1 * 16.0) as i64);
                let c = c0 + ((c1 - c0) * i64::from(s - s0)).div_euclid(i64::from(s1 - s0));
                c as f32 / 16.0
            } else {
                c0 + (c1 - c0) / (s1 - s0) as f32 * (s - s0) as f32
            }
        };
        for y in py0..py1 {
            let v = at(v0, v1, y0, y1, y * 16);
            for x in px0..px1 {
                let u = at(u0, u1, x0, x1, x * 16);
                self.fragment(p, x, y, Frag { uv: (u, v), ..fb });
            }
        }
    }

    fn triangle(&self, p: &Pipe, clip: [i32; 4], pos: [(i32, i32); 3], v: &[Vertex; 3]) {
        // The last vertex gives flat colour; keep it before reordering.
        let f = [attrs(p, &v[0]), attrs(p, &v[1]), attrs(p, &v[2])];
        let last = f[2];
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
        let inv = 1.0 / area as f64;
        let flat_z = f[0].z == f[1].z && f[1].z == f[2].z;
        let (tw, th) = (
            f64::from(1u16 << p.tex.tw.min(10)),
            f64::from(1u16 << p.tex.th.min(10)),
        );
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
                    let sum: i128 = (0..3).map(|k| i128::from(w[k]) * i128::from(f[k].z)).sum();
                    (sum / i128::from(area)) as u32
                };
                let rgba = if p.iip {
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
                let uv = if p.tme {
                    tex_coords(p, &f, w.map(|e| e as f64 * inv), (tw, th))
                } else {
                    (0.0, 0.0)
                };
                self.fragment(
                    p,
                    x,
                    y,
                    Frag {
                        z,
                        rgba,
                        uv,
                        f: fog,
                        ..last
                    },
                );
            }
        }
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
            self.fragment(
                p,
                px,
                py,
                Frag {
                    z,
                    rgba,
                    uv,
                    f: mix(fa.f, fb.f),
                    ..fb
                },
            );
        }
    }

    /// A texel of the current texture as RGBA8888, wrapped as CLAMP says.
    fn texel(&self, p: &Pipe, u: i32, v: i32) -> u32 {
        let Some(psm) = p.tex_psm else {
            return 0;
        };
        let (w, h) = (1i32 << p.tex.tw.min(10), 1i32 << p.tex.th.min(10));
        let wrap = |c: i32, mode: u32, size: i32, min: i32, max: i32| match mode {
            0 => c & (size - 1),
            1 => c.clamp(0, size - 1),
            2 => c.clamp(min, max),
            _ => (c & min) | max,
        };
        let cl = p.clamp;
        let u = wrap(
            u,
            bits(cl, 0, 2),
            w,
            bits(cl, 4, 10) as i32,
            bits(cl, 14, 10) as i32,
        );
        let v = wrap(
            v,
            bits(cl, 2, 2),
            h,
            bits(cl, 24, 10) as i32,
            bits(cl, 34, 10) as i32,
        );
        let raw = self.mem.read(
            psm,
            p.tex.tbp,
            p.tex.tbw,
            (u & 0x7FF) as u32,
            (v & 0x7FF) as u32,
        );
        match psm {
            Psm::Ct32 | Psm::Z32 => raw,
            Psm::Ct24 | Psm::Z24 => expand24(p.texa, raw),
            Psm::Ct16 | Psm::Ct16s | Psm::Z16 | Psm::Z16s => expand16(p.texa, raw),
            _ => clut_color(&p.clut, p.texa, &p.tex, raw),
        }
    }

    fn sample(&self, p: &Pipe, (u, v): (f32, f32)) -> [i32; 4] {
        let channels =
            |c: u32| [c & 0xFF, (c >> 8) & 0xFF, (c >> 16) & 0xFF, c >> 24].map(|x| x as i32);
        if !p.linear {
            return channels(self.texel(p, u.floor() as i32, v.floor() as i32));
        }
        // Bilinear, with 4-bit weights: across, then down, each step
        // truncated (as PCSX2's software renderer does).
        let (u, v) = ((u * 16.0) as i32 - 8, (v * 16.0) as i32 - 8);
        let (iu, iv, fu, fv) = (u >> 4, v >> 4, u & 15, v & 15);
        // A zero weight takes nothing from the texel it weighs (`lerp(a, b,
        // 0)` is `a`), so that texel is not fetched.
        let t00 = channels(self.texel(p, iu, iv));
        let t10 = if fu == 0 {
            t00
        } else {
            channels(self.texel(p, iu + 1, iv))
        };
        let (t01, t11) = if fv == 0 {
            (t00, t10)
        } else {
            let t01 = channels(self.texel(p, iu, iv + 1));
            let t11 = if fu == 0 {
                t01
            } else {
                channels(self.texel(p, iu + 1, iv + 1))
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

    /// One fragment through the pixel pipeline.
    #[allow(clippy::too_many_lines, reason = "the pipeline in hardware order")]
    fn fragment(&self, p: &Pipe, x: i32, y: i32, frag: Frag) {
        if x < 0 || y < 0 {
            return;
        }
        match p.scanmsk {
            2 if y & 1 == 0 => return,
            3 if y & 1 == 1 => return,
            _ => {}
        }
        let (x, y) = (x as u32, y as u32);
        // The tests that need no texture first, so a fragment they reject
        // costs no sampling; the result is the same in either order.
        let t = p.test;
        let (fpsm, fbp, fbw) = p.frame;
        let at = Memory::locate(fpsm, fbp, fbw, x, y);
        let dst = self.mem.get(at);
        // Destination alpha test.
        if bits(t, 14, 1) != 0 {
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
        if let Some((zpsm, zbp)) = p.zbuf {
            let max = match zpsm {
                Psm::Z24 => 0x00FF_FFFF,
                Psm::Z16 | Psm::Z16s => 0xFFFF,
                _ => u32::MAX,
            };
            let z = frag.z.min(max);
            let z_at = Memory::locate(zpsm, zbp, fbw, x, y);
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
        if p.tme {
            let t = self.sample(p, frag.uv);
            let (tcc, va) = (p.tex.tcc, c[3]);
            let modulate = |t: i32, v: i32| ((t * v) >> 7).min(255);
            match p.tex.tfx {
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
        if p.fge {
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
        let (mut write_fb, mut write_z, mut write_alpha) = (true, !p.zmsk, true);
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
        if p.abe && (!p.pabe || c[3] & 0x80 != 0) {
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
        let (new, mask) = match fpsm {
            Psm::Ct16 | Psm::Ct16s => (pack16(rgba), pack16(p.fbmsk)),
            Psm::Ct24 => (rgba & 0x00FF_FFFF, p.fbmsk | 0xFF00_0000),
            _ => (rgba, p.fbmsk),
        };
        self.mem.set(at, (new & !mask) | (dst & mask));
    }
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
        stq: [v.s, v.t, v.q],
        f: i32::from(v.f),
    }
}

/// The pixels of a triangle's row worth testing, relative to its first:
/// from the edge equations (an edge owned or not needs a value of at least
/// 0 or 1), one pixel wider on each side; the per-pixel test decides
/// exactly.
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
