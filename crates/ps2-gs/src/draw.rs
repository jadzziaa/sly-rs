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

use crate::regs::{Tex0, bits};
use crate::{Gs, Psm, Vertex};

/// Everything a primitive's fragments need, decoded once.
#[allow(clippy::struct_excessive_bools, reason = "PRIM's attribute bits")]
struct Pipe {
    tme: bool,
    fge: bool,
    abe: bool,
    fst: bool,
    iip: bool,
    /// Scissor, inclusive, in pixels.
    scissor: [i32; 4],
    offset: (i32, i32),
    frame: (Psm, u32, u32),
    fbmsk: u32,
    zbuf: Option<(Psm, u32)>,
    zmsk: bool,
    test: u64,
    alpha: u64,
    fba: bool,
    tex: Tex0,
    tex_psm: Option<Psm>,
    clamp: u64,
    linear: bool,
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
    fn pipe(&self) -> Pipe {
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
        }
    }

    /// Draws a primitive from its vertices.
    pub(crate) fn draw(&mut self, v: &[Vertex]) {
        self.drawn += 1;
        let p = self.pipe();
        let kind = self.prim & 7;
        let at = |v: &Vertex| (v.x as i32 - p.offset.0, v.y as i32 - p.offset.1);
        match kind {
            0 => {
                let (x, y) = at(&v[0]);
                let frag = Self::attrs(&p, &v[0]);
                self.fragment(&p, (x + 15) >> 4, (y + 15) >> 4, frag);
            }
            1 | 2 => self.line(&p, at(&v[0]), at(&v[1]), &v[0], &v[1]),
            3..=5 => self.triangle(&p, [at(&v[0]), at(&v[1]), at(&v[2])], v),
            _ => self.sprite(&p, at(&v[0]), at(&v[1]), &v[0], &v[1]),
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

    fn sprite(&mut self, p: &Pipe, a: (i32, i32), b: (i32, i32), va: &Vertex, vb: &Vertex) {
        let (fa, fb) = (Self::attrs(p, va), Self::attrs(p, vb));
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
        let [sx0, sx1, sy0, sy1] = p.scissor;
        let px0 = ((x0 + 15) >> 4).max(sx0);
        let px1 = ((x1 + 15) >> 4).min(sx1 + 1);
        let py0 = ((y0 + 15) >> 4).max(sy0);
        let py1 = ((y1 + 15) >> 4).min(sy1 + 1);
        let du = (u1 - u0) / (x1 - x0) as f32;
        let dv = (v1 - v0) / (y1 - y0) as f32;
        for y in py0..py1 {
            let v = v0 + dv * (y * 16 - y0) as f32;
            for x in px0..px1 {
                let u = u0 + du * (x * 16 - x0) as f32;
                self.fragment(p, x, y, Frag { uv: (u, v), ..fb });
            }
        }
    }

    fn triangle(&mut self, p: &Pipe, pos: [(i32, i32); 3], v: &[Vertex]) {
        // The last vertex gives flat colour; keep it before reordering.
        let f = [
            Self::attrs(p, &v[0]),
            Self::attrs(p, &v[1]),
            Self::attrs(p, &v[2]),
        ];
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
        let [sx0, sx1, sy0, sy1] = p.scissor;
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
        let inv = 1.0 / area as f64;
        for y in top.max(sy0)..=bottom.min(sy1) {
            for x in left.max(sx0)..=right.min(sx1) {
                let (sx, sy) = (i64::from(x) * 16, i64::from(y) * 16);
                let mut w = [0i64; 3];
                let mut inside = true;
                for (k, &(a, b)) in edges.iter().enumerate() {
                    let e = edge(a, b, sx, sy);
                    w[k] = e;
                    if e < 0 || (e == 0 && !owns(a, b)) {
                        inside = false;
                        break;
                    }
                }
                if !inside {
                    continue;
                }
                let b = w.map(|e| e as f64 * inv);
                let lerp =
                    |g: &dyn Fn(&Frag) -> f64| b[0] * g(&f[0]) + b[1] * g(&f[1]) + b[2] * g(&f[2]);
                let z = lerp(&|q| f64::from(q.z)).clamp(0.0, f64::from(u32::MAX)) as u32;
                let rgba = if p.iip {
                    std::array::from_fn(|k| lerp(&|q| f64::from(q.rgba[k])) as i32)
                } else {
                    last.rgba
                };
                let uv = if p.fst {
                    (
                        lerp(&|q| f64::from(q.uv.0)) as f32,
                        lerp(&|q| f64::from(q.uv.1)) as f32,
                    )
                } else {
                    // Perspective: interpolate S, T and Q, then divide.
                    let (tw, th) = (
                        f64::from(1u16 << p.tex.tw.min(10)),
                        f64::from(1u16 << p.tex.th.min(10)),
                    );
                    let s = lerp(&|q| f64::from(q.stq[0]));
                    let t = lerp(&|q| f64::from(q.stq[1]));
                    let q = lerp(&|q| f64::from(q.stq[2]));
                    let q = if q == 0.0 { f64::MIN_POSITIVE } else { q };
                    ((s / q * tw) as f32, (t / q * th) as f32)
                };
                let fog = lerp(&|q| f64::from(q.f)) as i32;
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

    fn line(&mut self, p: &Pipe, a: (i32, i32), b: (i32, i32), va: &Vertex, vb: &Vertex) {
        let (fa, fb) = (Self::attrs(p, va), Self::attrs(p, vb));
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let steps = (dx.abs().max(dy.abs()) + 15) >> 4;
        let [sx0, sx1, sy0, sy1] = p.scissor;
        for i in 0..steps.max(1) {
            let t = i as f32 / steps.max(1) as f32;
            let x = (a.0 as f32 + dx as f32 * t) as i32;
            let y = (a.1 as f32 + dy as f32 * t) as i32;
            let (px, py) = ((x + 8) >> 4, (y + 8) >> 4);
            if px < sx0 || px > sx1 || py < sy0 || py > sy1 {
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
            Psm::Ct24 | Psm::Z24 => self.expand24(raw),
            Psm::Ct16 | Psm::Ct16s | Psm::Z16 | Psm::Z16s => self.expand16(raw),
            _ => self.clut_color(&p.tex, raw),
        }
    }

    fn sample(&self, p: &Pipe, (u, v): (f32, f32)) -> [i32; 4] {
        let channels =
            |c: u32| [c & 0xFF, (c >> 8) & 0xFF, (c >> 16) & 0xFF, c >> 24].map(|x| x as i32);
        if !p.linear {
            return channels(self.texel(p, u.floor() as i32, v.floor() as i32));
        }
        // Bilinear, with 4-bit weights.
        let (u, v) = ((u * 16.0) as i32 - 8, (v * 16.0) as i32 - 8);
        let (iu, iv, fu, fv) = (u >> 4, v >> 4, u & 15, v & 15);
        let t = [
            channels(self.texel(p, iu, iv)),
            channels(self.texel(p, iu + 1, iv)),
            channels(self.texel(p, iu, iv + 1)),
            channels(self.texel(p, iu + 1, iv + 1)),
        ];
        std::array::from_fn(|k| {
            let top = t[0][k] * (16 - fu) + t[1][k] * fu;
            let bottom = t[2][k] * (16 - fu) + t[3][k] * fu;
            (top * (16 - fv) + bottom * fv) >> 8
        })
    }

    /// One fragment through the pixel pipeline.
    #[allow(clippy::too_many_lines, reason = "the pipeline in hardware order")]
    fn fragment(&mut self, p: &Pipe, x: i32, y: i32, frag: Frag) {
        if x < 0 || y < 0 {
            return;
        }
        match bits(self.scanmsk, 0, 2) {
            2 if y & 1 == 0 => return,
            3 if y & 1 == 1 => return,
            _ => {}
        }
        let (x, y) = (x as u32, y as u32);
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
                bits(self.fogcol, 0, 8),
                bits(self.fogcol, 8, 8),
                bits(self.fogcol, 16, 8),
            ];
            for k in 0..3 {
                c[k] = (frag.f * c[k] + (255 - frag.f) * fc[k] as i32) >> 8;
            }
        }
        // Alpha test: which writes survive.
        let (mut write_fb, mut write_z, mut write_alpha) = (true, !p.zmsk, true);
        let t = p.test;
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
        let (fpsm, fbp, fbw) = p.frame;
        let dst = self.mem.read(fpsm, fbp, fbw, x, y);
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
        // Depth test.
        if let Some((zpsm, zbp)) = p.zbuf {
            let max = match zpsm {
                Psm::Z24 => 0x00FF_FFFF,
                Psm::Z16 | Psm::Z16s => 0xFFFF,
                _ => u32::MAX,
            };
            let z = frag.z.min(max);
            let old = self.mem.read(zpsm, zbp, fbw, x, y);
            let pass = match bits(t, 17, 2) {
                0 => false,
                1 => true,
                2 => z >= old,
                _ => z > old,
            };
            if !pass {
                return;
            }
            if write_z {
                self.mem.write(zpsm, zbp, fbw, x, y, z);
            }
        }
        if !write_fb {
            return;
        }
        let d = match fpsm {
            Psm::Ct16 | Psm::Ct16s => {
                let c = self.expand16(dst & 0x7FFF);
                (c & 0x00FF_FFFF) | (if dst & 0x8000 != 0 { 0x8000_0000 } else { 0 })
            }
            Psm::Ct24 => dst | 0x8000_0000,
            _ => dst,
        };
        let dc = [d & 0xFF, (d >> 8) & 0xFF, (d >> 16) & 0xFF, d >> 24].map(|v| v as i32);
        if p.abe && (bits(self.pabe, 0, 1) == 0 || c[3] & 0x80 != 0) {
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
        let clamp = bits(self.colclamp, 0, 1) != 0;
        for ck in c.iter_mut().take(3) {
            *ck = if clamp {
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
        self.mem
            .write(fpsm, fbp, fbw, x, y, (new & !mask) | (dst & mask));
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
