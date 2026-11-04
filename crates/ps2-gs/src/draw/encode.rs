//! Primitives encoded for a GPU ([`crate::export`]).

use super::{
    Frag, Pipe, Prim, Vertex, aa_edge_frags, aa_line_frags, attrs, line_frags, point, sprite_frags,
};
use crate::export::{self, FRAG_WORDS, PRIM_WORDS, flag, word};
use crate::fixed;
use crate::regs::bits;
use rayon::prelude::*;

/// `prims` as a batch for a GPU drawing at internal resolution `scale`;
/// their textures read from a snapshot when `snapshot` (a primitive drawn
/// alone that textures from its own writes). A large batch is encoded in
/// parts on several threads.
pub(crate) fn batch(prims: &[Prim], snapshot: bool, scale: u32) -> export::Batch {
    /// Primitives a part takes.
    const PART: usize = 1024;
    let part = |prims: &[Prim]| {
        let mut out = export::Batch::default();
        for prim in prims {
            let clut = prim.pipe.clut_version % export::CLUT_SLOTS;
            encode(prim, clut, snapshot, scale, &mut out.prims, &mut out.frags);
        }
        out
    };
    if prims.len() <= PART {
        return part(prims);
    }
    let parts: Vec<export::Batch> = prims.par_chunks(PART).map(part).collect();
    let mut out = export::Batch {
        prims: Vec::with_capacity(parts.iter().map(|p| p.prims.len()).sum()),
        frags: Vec::with_capacity(parts.iter().map(|p| p.frags.len()).sum()),
    };
    for mut p in parts {
        // Its fragments now start further on.
        let before = (out.frags.len() / FRAG_WORDS) as u32;
        for prim in p.prims.as_chunks_mut::<PRIM_WORDS>().0 {
            prim[word::FRAGS] += before;
        }
        out.prims.append(&mut p.prims);
        out.frags.append(&mut p.frags);
    }
    out
}

/// A sample plane's offset in the pixel at internal resolution `scale`, in
/// 1/16 pixel (as the GPU's draw shader takes it).
fn offset(plane: u32, scale: u32) -> (i32, i32) {
    (
        ((plane % scale) * 16 / scale) as i32,
        ((plane / scale) * 16 / scale) as i32,
    )
}

/// The pixels a plane's samples may light: the primitive's, and for planes
/// other than the first one more to the left and top, within the scissor
/// (as the GPU's `bounds`).
fn plane_clip(prim: &Prim, plane: u32) -> [i32; 4] {
    let [x0, x1, y0, y1] = prim.bbox;
    if plane == 0 {
        return prim.bbox;
    }
    let [sx0, _, sy0, _] = prim.pipe.scissor;
    [(x0 - 1).max(sx0).max(0), x1, (y0 - 1).max(sy0).max(0), y1]
}

/// Appends `prim`'s words to `out`, and its fragments, if it has any, to
/// `frags`: for each of the `scale`² sample planes, walked with the
/// primitive moved by minus the plane's offset, so that each plane's
/// samples sit where the GPU's do. A primitive that draws nothing appends
/// nothing.
fn encode(
    prim: &Prim,
    clut: u32,
    snapshot: bool,
    scale: u32,
    out: &mut Vec<u32>,
    frags: &mut Vec<u32>,
) {
    let p = &prim.pipe;
    let v = &prim.v;
    let mut w = [0u32; PRIM_WORDS];
    let first = frags.len() / FRAG_WORDS;
    let corners = [at(p, &v[0]), at(p, &v[1]), at(p, &v[2])];
    // Points, lines and sprites with STQ textures are walked on the CPU
    // alone; triangles and other sprites are drawn inside on the GPU, an
    // antialiased triangle's edges walked here too.
    let mut listed = false;
    let drawn = match prim.kind {
        3..=5 => triangle(p, v, &mut w),
        6 | 7 => sprite(p, v, &mut w).unwrap_or_else(|| {
            listed = true;
            true
        }),
        _ => {
            listed = true;
            true
        }
    };
    if drawn && (listed || (matches!(prim.kind, 3..=5) && p.aa1)) {
        for plane in 0..scale * scale {
            let (ox, oy) = offset(plane, scale);
            let clip = plane_clip(prim, plane);
            let [a, b, c] = corners.map(|(x, y)| (x - ox, y - oy));
            let mut emit = |x: i32, y: i32, f: Frag| {
                frags.extend_from_slice(&[
                    x as u32 | (y as u32) << 16,
                    f.z,
                    rgba(f.rgba),
                    f.tex.0 as u32,
                    f.tex.1 as u32,
                    f.f as u32,
                    f.coverage.unwrap_or(u32::MAX),
                    plane,
                ]);
            };
            match prim.kind {
                0 => {
                    let (x, y) = point(a);
                    if (clip[0]..=clip[1]).contains(&x) && (clip[2]..=clip[3]).contains(&y) {
                        emit(x, y, attrs(p, &v[0]));
                    }
                }
                1 | 2 if p.aa1 => aa_line_frags(p, clip, a, b, &v[0], &v[1], &mut emit),
                1 | 2 => line_frags(p, clip, a, b, &v[0], &v[1], &mut emit),
                3..=5 => {
                    let given = [attrs(p, &v[0]), attrs(p, &v[1]), attrs(p, &v[2])];
                    aa_edge_frags(p, clip, [a, b, c], &given, &given[2], &mut emit);
                }
                _ => sprite_frags(p, clip, a, b, &v[0], &v[1], &mut emit),
            }
        }
    }
    if listed {
        w[word::KIND] |= flag::LISTED;
    }
    let count = frags.len() / FRAG_WORDS - first;
    if !drawn || (listed && count == 0) {
        return;
    }
    w[word::FRAGS] = first as u32;
    w[word::FRAG_COUNT] = count as u32;
    common(prim, clut, snapshot, &mut w);
    out.extend_from_slice(&w);
}

/// The pipeline's state, the same for every kind.
fn common(prim: &Prim, clut: u32, snapshot: bool, w: &mut [u32; PRIM_WORDS]) {
    let p = &prim.pipe;
    let flags = [
        (p.tme, flag::TME),
        (p.fge, flag::FGE),
        (p.abe, flag::ABE),
        (p.fst, flag::FST),
        (p.iip, flag::IIP),
        (p.zmsk, flag::ZMSK),
        (p.fba, flag::FBA),
        (p.colclamp, flag::COLCLAMP),
        (p.pabe, flag::PABE),
        (p.linear, flag::LINEAR),
        (p.tex.tcc, flag::TCC),
        (p.zbuf.is_some() && p.depth_layout.is_some(), flag::ZBUF),
        (p.tex_psm.is_some() && p.tex_layout.is_some(), flag::TEX),
        (snapshot, flag::SNAPSHOT),
        (p.aa1, flag::AA1),
        (prim.rendered, flag::RENDERED),
    ];
    w[word::KIND] |= (prim.kind as u32 & 7)
        | flags
            .iter()
            .filter(|(on, _)| *on)
            .fold(0, |all, (_, f)| all | f);
    let [x0, x1, y0, y1] = prim.bbox.map(|c| c as u32);
    w[word::BBOX_X] = x0 | x1 << 16;
    w[word::BBOX_Y] = y0 | y1 << 16;
    let [sx0, sx1, sy0, sy1] = p.scissor.map(|c| c as u32);
    w[word::SCISSOR_X] = sx0 | sx1 << 16;
    w[word::SCISSOR_Y] = sy0 | sy1 << 16;
    w[word::FRAME_PSM] = p.frame.0.code();
    w[word::FBMSK] = p.fbmsk;
    w[word::ZBUF_PSM] = p.zbuf.map_or(u32::MAX, |(psm, _)| psm.code());
    w[word::TEST] = p.test as u32;
    w[word::ALPHA] = bits(p.alpha, 0, 8) | bits(p.alpha, 32, 8) << 8;
    w[word::TEXA] = bits(p.texa, 0, 8) | bits(p.texa, 15, 1) << 8 | bits(p.texa, 32, 8) << 16;
    w[word::FOGCOL] = bits(p.fogcol, 0, 24);
    w[word::TEX_PSM] = p.tex_psm.map_or(u32::MAX, crate::Psm::code);
    w[word::TEX_FUNC] = p.tex.tfx | p.tex.cpsm << 4 | p.tex.csa << 8;
    let [u, v] = p.wrap;
    w[word::WRAP] = u.mode | v.mode << 2;
    w[word::WRAP_U] = (u.min as u32 & 0xFFFF) | (u.max as u32) << 16;
    w[word::WRAP_V] = (v.min as u32 & 0xFFFF) | (v.max as u32) << 16;
    w[word::TEX_SIZE] = u.size as u32 | (v.size as u32) << 16;
    w[word::CLUT] = clut;
    w[word::SCANMSK] = p.scanmsk;
    let layouts = [
        (word::FRAME_LAYOUT, Some(p.frame_layout)),
        (word::DEPTH_LAYOUT, p.depth_layout),
        (word::TEX_LAYOUT, p.tex_layout),
    ];
    for (at, layout) in layouts {
        if let Some(layout) = layout {
            w[at..at + 7].copy_from_slice(&layout.gpu());
        }
    }
}

/// A position less XYOFFSET.
fn at(p: &Pipe, v: &Vertex) -> (i32, i32) {
    (v.x as i32 - p.offset.0, v.y as i32 - p.offset.1)
}

fn rgba(c: [i32; 4]) -> u32 {
    c.iter()
        .enumerate()
        .fold(0, |all, (k, &v)| all | (v as u32 & 0xFF) << (8 * k))
}

/// A triangle as `Raster::triangle` sets it up; false if it has no area.
fn triangle(p: &Pipe, v: &[Vertex; 3], w: &mut [u32; PRIM_WORDS]) -> bool {
    let pos = [at(p, &v[0]), at(p, &v[1]), at(p, &v[2])];
    let f = [attrs(p, &v[0]), attrs(p, &v[1]), attrs(p, &v[2])];
    let last = f[2];
    let edge = |a: (i32, i32), b: (i32, i32), c: (i32, i32)| {
        i64::from(b.0 - a.0) * i64::from(c.1 - a.1) - i64::from(b.1 - a.1) * i64::from(c.0 - a.0)
    };
    let area = edge(pos[0], pos[1], pos[2]);
    if area == 0 {
        return false;
    }
    let (pos, f) = if area < 0 {
        ([pos[0], pos[2], pos[1]], [f[0], f[2], f[1]])
    } else {
        (pos, f)
    };
    let owns = |a: (i32, i32), b: (i32, i32)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        dy < 0 || (dy == 0 && dx > 0)
    };
    let edges = [(pos[1], pos[2]), (pos[2], pos[0]), (pos[0], pos[1])];
    for (k, (a, b)) in edges.into_iter().enumerate() {
        if owns(a, b) {
            w[word::KIND] |= flag::OWNED << k;
        }
    }
    if f[0].z == f[1].z && f[1].z == f[2].z {
        w[word::KIND] |= flag::FLAT_Z;
    }
    for k in 0..3 {
        w[word::POS + 2 * k] = pos[k].0 as u32;
        w[word::POS + 2 * k + 1] = pos[k].1 as u32;
        w[word::Z + k] = f[k].z;
        w[word::RGBA + k] = rgba(f[k].rgba);
        w[word::FOG + k] = f[k].f as u32;
    }
    w[word::FLAT_RGBA] = rgba(last.rgba);
    if p.tme {
        if p.fst {
            for k in 0..3 {
                w[word::U + k] = (f[k].uv.0 * 16.0) as i64 as u32;
                w[word::V + k] = (f[k].uv.1 * 16.0) as i64 as u32;
            }
        } else {
            let stq = fixed::Stq::new(f.map(|v| v.stq), p.tex.tw, p.tex.th);
            let ([s, t, q], [shift_u, shift_v]) = stq.parts();
            for k in 0..3 {
                w[word::U + k] = s[k] as u32;
                w[word::V + k] = t[k] as u32;
                w[word::Q + k] = q[k] as u32;
            }
            w[word::SHIFT_U] = shift_u as u32;
            w[word::SHIFT_V] = shift_v as u32;
            w[word::KIND] |= flag::STQ;
        }
    }
    true
}

/// A sprite as `Raster::sprite` sets it up: `Some(false)` if it has no
/// area, `None` if its pixels are walked on the CPU (an STQ texture).
fn sprite(p: &Pipe, v: &[Vertex; 3], w: &mut [u32; PRIM_WORDS]) -> Option<bool> {
    let (a, b) = (at(p, &v[0]), at(p, &v[1]));
    let (fa, fb) = (attrs(p, &v[0]), attrs(p, &v[1]));
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
        return Some(false);
    }
    if p.tme && !p.fst {
        return None;
    }
    for (k, c) in [x0, y0, x1, y1].into_iter().enumerate() {
        w[word::POS + k] = c as u32;
    }
    // As `sprite_coord` takes them: UV in 1/16 texel.
    let fixed = |c: f32| (c * 16.0) as i64 as u32;
    w[word::U] = fixed(u0);
    w[word::U + 1] = fixed(u1);
    w[word::V] = fixed(v0);
    w[word::V + 1] = fixed(v1);
    w[word::Z] = fb.z;
    w[word::RGBA] = rgba(fb.rgba);
    w[word::FOG] = fb.f as u32;
    Some(true)
}
