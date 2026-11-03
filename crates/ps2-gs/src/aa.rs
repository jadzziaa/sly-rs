//! Antialiasing (`PRIM.AA1`) as PCSX2's software renderer does it
//! (`GSRasterizer::DrawEdgeTriangle` and `DrawEdgeLine`, GPL-3.0-or-later;
//! see NOTICE.md): after a triangle's inside, one pixel along each edge per
//! step of its longer axis, with the share of the pixel the triangle
//! covers; a line as two pixels per step whose shares sum to the whole.
//!
//! The walks are PCSX2's, in its types (`f32` positions and decision
//! values, `f64` where its arithmetic promotes), so the same pixels come
//! out with the same coverage. Each pixel carries where along its edge it
//! lies, for the caller to interpolate the edge's attributes.

/// An edge pixel: where it is, its coverage (0 to 0xFFFF), and the edge it
/// was found on, from vertex `from` to `to` at `t` along the way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EdgePixel {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) coverage: u32,
    pub(crate) from: usize,
    pub(crate) to: usize,
    pub(crate) t: f32,
}

/// Vertex orders that sort three vertices by y, by which of y0 > y1,
/// y0 > y2 and y1 > y2 hold (PCSX2's `s_ysort`).
const Y_SORT: [[usize; 3]; 8] = [
    [0, 1, 2],
    [1, 0, 2],
    [0, 0, 0],
    [1, 2, 0],
    [0, 2, 1],
    [0, 0, 0],
    [2, 0, 1],
    [2, 1, 0],
];

/// An edge function: `x * a + y * b + c`, in wrapping 32-bit arithmetic as
/// PCSX2's vectors compute it.
#[derive(Clone, Copy)]
struct EdgeFn {
    a: i32,
    b: i32,
    c: i32,
}

impl EdgeFn {
    fn at(self, x: i32, y: i32) -> i32 {
        self.a
            .wrapping_mul(x)
            .wrapping_add(self.b.wrapping_mul(y))
            .wrapping_add(self.c)
    }
}

/// The edge pixels of the triangle at `pos` (1/16 pixel), inside
/// `scissor` (x0, x1, y0, y1, inclusive).
#[allow(clippy::float_cmp, reason = "PCSX2 compares positions exactly")]
pub(crate) fn triangle(pos: [(i32, i32); 3], scissor: [i32; 4], emit: &mut impl FnMut(EdgePixel)) {
    let p = pos.map(|(x, y)| (x as f32 / 16.0, y as f32 / 16.0));
    let m1 = usize::from(p[0].1 > p[1].1)
        | usize::from(p[0].1 > p[2].1) << 1
        | usize::from(p[1].1 > p[2].1) << 2;
    let i = Y_SORT[m1];
    let (v0, v1, v2) = (p[i[0]], p[i[1]], p[i[2]]);
    if v0.1 == v1.1 && v1.1 == v2.1 {
        return;
    }
    let sub = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0, a.1 - b.1);
    let (dv0, dv1, dv2) = (sub(v1, v0), sub(v2, v0), sub(v2, v1));
    let clockwise = dv0.1 * dv1.0 - dv0.0 * dv1.1 < 0.0;
    let tl0 = v0.1 == v1.1 || !clockwise;
    let tl1 = clockwise;
    let tl2 = v1.1 != v2.1 && !clockwise;
    let xy = [i[0], i[1], i[2]].map(|k| pos[k]);
    let function = |a: (i32, i32), b: (i32, i32), tl: bool| {
        let (dx, dy) = (b.0.wrapping_sub(a.0), b.1.wrapping_sub(a.1));
        let f = EdgeFn {
            a: dy.wrapping_mul(16),
            b: dx.wrapping_neg().wrapping_mul(16),
            c: b.0.wrapping_mul(a.1).wrapping_sub(a.0.wrapping_mul(b.1)),
        };
        let f = if clockwise {
            EdgeFn {
                a: f.a.wrapping_neg(),
                b: f.b.wrapping_neg(),
                c: f.c.wrapping_neg(),
            }
        } else {
            f
        };
        EdgeFn {
            c: f.c.wrapping_add(i32::from(tl)),
            ..f
        }
    };
    let f0 = function(xy[0], xy[1], tl0);
    let f1 = function(xy[2], xy[0], tl1);
    let f2 = function(xy[1], xy[2], tl2);
    let edges = [
        (v0, v1, dv0, f1, f2, tl0, i[0], i[1]),
        (v0, v2, dv1, f2, f0, tl1, i[0], i[2]),
        (v1, v2, dv2, f0, f1, tl2, i[1], i[2]),
    ];
    for (a, b, dv, e1, e2, tl, from, to) in edges {
        edge(a, b, dv, [e1, e2], tl, scissor, &mut |x, y, coverage, t| {
            emit(EdgePixel {
                x,
                y,
                coverage,
                from,
                to,
                t,
            });
        });
    }
}

/// One edge of a triangle, from `v0` to `v1` (`dv` apart), inside the
/// other two edges' functions `efun`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::similar_names,
    clippy::float_cmp,
    clippy::too_many_lines,
    reason = "PCSX2's walk, step by step, in its types and names"
)]
fn edge(
    v0: (f32, f32),
    v1: (f32, f32),
    dv: (f32, f32),
    efun: [EdgeFn; 2],
    tl: bool,
    scissor: [i32; 4],
    emit: &mut impl FnMut(i32, i32, u32, f32),
) {
    let (delta_x, delta_y) = dv;
    if delta_x == 0.0 && delta_y == 0.0 {
        return;
    }
    let step_x = delta_x.abs() >= delta_y.abs();
    let (pos_x, pos_y) = (delta_x >= 0.0, delta_y >= 0.0);
    // Outside the triangle towards the top or left.
    let side = tl ^ (step_x && delta_y != 0.0 && pos_x == pos_y);
    let (dxi, dyi) = (if pos_x { 1 } else { -1 }, if pos_y { 1 } else { -1 });
    let ((x0, y0), (x1, y1)) = (v0, v1);
    let rx0 = if pos_x {
        (x0 - 1.0).ceil()
    } else {
        (x0 + 1.0).floor()
    };
    let ry0 = if pos_y {
        (y0 - 1.0).ceil()
    } else {
        (y0 + 1.0).floor()
    };
    let rx1 = if pos_x {
        (x1 + 1.0).floor()
    } else {
        (x1 - 1.0).ceil()
    };
    let ry1 = if pos_y {
        (y1 + 1.0).floor()
    } else {
        (y1 - 1.0).ceil()
    };
    let (rxi0, ryi0, rxi1, ryi1) = (rx0 as i32, ry0 as i32, rx1 as i32, ry1 as i32);
    let [sx0, sx1, sy0, sy1] = scissor;
    let bxi0 = (x0.min(x1).floor() as i32).max(sx0);
    let byi0 = ((y0.min(y1) - 1.0).ceil() as i32).max(sy0);
    let bxi1 = (x0.max(x1).ceil() as i32).min(sx1);
    let byi1 = ((y0.max(y1) + 1.0).floor() as i32).min(sy1);
    let drive = if step_x { delta_x } else { delta_y }.abs();
    let pos_d = if step_x { pos_y } else { pos_x };
    let scale_d = (512.0 * drive) as i32;
    let scale_df = scale_d as f32;
    let d_d = (512.0 * if step_x { delta_y } else { delta_x }) as i32;
    let mut d = (scale_d as f32 * if step_x { y0 - ry0 } else { x0 - rx0 }) as i32;
    let (mut xi, mut yi) = (rxi0, ryi0);
    let [f1, f2] = efun;
    let (mut e1, mut e2) = (f1.at(xi, yi), f2.at(xi, yi));
    let along = |f: EdgeFn| if step_x { f.b } else { f.a };
    let step_dependent =
        |sign: i32, d: &mut i32, xi: &mut i32, yi: &mut i32, e1: &mut i32, e2: &mut i32| {
            *d -= scale_d * sign;
            if step_x {
                *yi += sign;
            } else {
                *xi += sign;
            }
            *e1 = e1.wrapping_add(along(f1).wrapping_mul(sign));
            *e2 = e2.wrapping_add(along(f2).wrapping_mul(sign));
        };
    let prestep = if step_x {
        dxi as f32 * (rx0 - x0)
    } else {
        dyi as f32 * (ry0 - y0)
    };
    d += (d_d as f32 * prestep) as i32;
    while d >= scale_d / 2 {
        step_dependent(1, &mut d, &mut xi, &mut yi, &mut e1, &mut e2);
    }
    while d < -scale_d / 2 {
        step_dependent(-1, &mut d, &mut xi, &mut yi, &mut e1, &mut e2);
    }
    let mut steps = 0.0f32;
    // A guard: an edge spans the 2048-pixel space at most.
    for _ in 0..4096 {
        let df = d as f32 / scale_df;
        let (coverage, offset) = if df > 0.0 {
            if side {
                ((65535.0 * (1.0 - f64::from(df))) as i32, 0)
            } else {
                ((65535.0 * df) as i32, 1)
            }
        } else if df < 0.0 {
            if side {
                ((65535.0 * -df) as i32, -1)
            } else {
                ((65535.0 * (1.0 + f64::from(df))) as i32, 0)
            }
        } else if tl {
            (0, if side { -1 } else { 1 })
        } else {
            (0xFFFF, 0)
        };
        let (xi2, yi2) = if step_x {
            (xi, yi + offset)
        } else {
            (xi + offset, yi)
        };
        let e12 = e1.wrapping_add(along(f1).wrapping_mul(offset));
        let e22 = e2.wrapping_add(along(f2).wrapping_mul(offset));
        if e12 > 0 && e22 > 0 && (bxi0..=bxi1).contains(&xi2) && (byi0..=byi1).contains(&yi2) {
            emit(
                xi2,
                yi2,
                coverage.clamp(0, 0xFFFF) as u32,
                (prestep + steps) / drive,
            );
        }
        if if step_x { xi == rxi1 } else { yi == ryi1 } {
            break;
        }
        steps += 1.0;
        d += d_d;
        if step_x {
            xi += dxi;
            e1 = e1.wrapping_add(dxi.wrapping_mul(f1.a));
            e2 = e2.wrapping_add(dxi.wrapping_mul(f2.a));
        } else {
            yi += dyi;
            e1 = e1.wrapping_add(dyi.wrapping_mul(f1.b));
            e2 = e2.wrapping_add(dyi.wrapping_mul(f2.b));
        }
        if pos_d {
            if d >= scale_d / 2 {
                step_dependent(1, &mut d, &mut xi, &mut yi, &mut e1, &mut e2);
            }
        } else if d < -scale_d / 2 {
            step_dependent(-1, &mut d, &mut xi, &mut yi, &mut e1, &mut e2);
        }
    }
}

/// The pixels of an antialiased line from `a` to `b` (1/16 pixel), inside
/// `scissor` (inclusive): two a step, sharing the coverage.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::similar_names,
    clippy::float_cmp,
    clippy::too_many_lines,
    reason = "PCSX2's walk, step by step, in its types and names"
)]
pub(crate) fn line(
    a: (i32, i32),
    b: (i32, i32),
    scissor: [i32; 4],
    emit: &mut impl FnMut(EdgePixel),
) {
    let (x0, y0) = (a.0 as f32 / 16.0, a.1 as f32 / 16.0);
    let (x1, y1) = (b.0 as f32 / 16.0, b.1 as f32 / 16.0);
    let (delta_x, delta_y) = (x1 - x0, y1 - y0);
    let step_x = delta_x.abs() >= delta_y.abs();
    let (pos_x, pos_y) = (delta_x >= 0.0, delta_y >= 0.0);
    let (dxi, dyi) = (if pos_x { 1 } else { -1 }, if pos_y { 1 } else { -1 });
    let (mut rx0, mut ry0) = ((x0 + 0.5).floor(), (y0 + 0.5).floor());
    let (mut rx1, mut ry1) = ((x1 + 0.5).floor(), (y1 + 0.5).floor());
    // The diamond exit rule for the first and last pixels.
    let exits = |dx: f32, dy: f32| {
        let dist = dx.abs() + dy.abs();
        if dist < 0.5 {
            return false;
        }
        if step_x {
            let good = if pos_x { dx > 0.0 } else { dx < 0.0 };
            good && (dist > 0.5 || dy >= 0.0)
        } else {
            let good = if pos_y { dy > 0.0 } else { dy < 0.0 };
            good && (dist > 0.5 || dx >= 0.0)
        }
    };
    if exits(x0 - rx0, y0 - ry0) {
        if step_x {
            rx0 += dxi as f32;
        } else {
            ry0 += dyi as f32;
        }
    }
    if !exits(x1 - rx1, y1 - ry1) {
        if step_x {
            rx1 -= dxi as f32;
        } else {
            ry1 -= dyi as f32;
        }
    }
    if (if step_x {
        dxi as f32 * (rx1 - rx0)
    } else {
        dyi as f32 * (ry1 - ry0)
    }) < 0.0
    {
        return;
    }
    let (rxi1, ryi1) = (rx1 as i32, ry1 as i32);
    let drive = if step_x { delta_x } else { delta_y }.abs();
    let pos_d = if step_x { pos_y } else { pos_x };
    let scale_d = (512.0 * drive) as i32;
    let scale_df = scale_d as f32;
    let d_d = (512.0 * if step_x { delta_y } else { delta_x }) as i32;
    let mut d = (scale_d as f32 * if step_x { y0 - ry0 } else { x0 - rx0 }) as i32;
    let (mut xi, mut yi) = (rx0 as i32, ry0 as i32);
    let step_dependent = |sign: i32, d: &mut i32, xi: &mut i32, yi: &mut i32| {
        *d -= scale_d * sign;
        if step_x {
            *yi += sign;
        } else {
            *xi += sign;
        }
    };
    let prestep = if step_x {
        dxi as f32 * (rx0 - x0)
    } else {
        dyi as f32 * (ry0 - y0)
    };
    d += (d_d as f32 * prestep) as i32;
    while d >= scale_d / 2 {
        step_dependent(1, &mut d, &mut xi, &mut yi);
    }
    while d < -scale_d / 2 {
        step_dependent(-1, &mut d, &mut xi, &mut yi);
    }
    let [sx0, sx1, sy0, sy1] = scissor;
    let mut steps = 0.0f32;
    for _ in 0..4096 {
        let t = (prestep + steps) / drive;
        let coverage = (65535.0 * (d as f32 / scale_df).abs()) as i32;
        let covi = coverage.clamp(0, 0xFFFF) as u32;
        let offset = if d >= 0 { 1 } else { -1 };
        let neighbour = if step_x {
            (xi, yi + offset)
        } else {
            (xi + offset, yi)
        };
        for ((x, y), coverage) in [((xi, yi), 0xFFFF - covi), (neighbour, covi)] {
            if (sx0..=sx1).contains(&x) && (sy0..=sy1).contains(&y) {
                emit(EdgePixel {
                    x,
                    y,
                    coverage,
                    from: 0,
                    to: 1,
                    t,
                });
            }
        }
        if if step_x { xi == rxi1 } else { yi == ryi1 } {
            break;
        }
        steps += 1.0;
        d += d_d;
        if step_x {
            xi += dxi;
        } else {
            yi += dyi;
        }
        if pos_d {
            if d >= scale_d / 2 {
                step_dependent(1, &mut d, &mut xi, &mut yi);
            }
        } else if d < -scale_d / 2 {
            step_dependent(-1, &mut d, &mut xi, &mut yi);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_shares_each_step_between_two_pixels() {
        let mut pixels = Vec::new();
        // From (10, 10.25) to (20, 13.25): x-major.
        line((160, 164), (320, 212), [0, 639, 0, 447], &mut |p| {
            pixels.push(p);
        });
        assert_ne!(pixels.len(), 0);
        for pair in pixels.chunks(2) {
            assert_eq!(pair[0].coverage + pair[1].coverage, 0xFFFF);
            assert_eq!(pair[0].x, pair[1].x);
            assert_eq!((pair[0].y - pair[1].y).abs(), 1);
        }
    }

    #[test]
    fn triangle_edges_lie_around_the_triangle() {
        let mut pixels = Vec::new();
        let pos = [(16 * 10, 16 * 10), (16 * 30, 16 * 12), (16 * 14, 16 * 28)];
        triangle(pos, [0, 639, 0, 447], &mut |p| {
            pixels.push(p);
        });
        assert!(pixels.len() > 30);
        for p in &pixels {
            assert!((9..=31).contains(&p.x) && (9..=29).contains(&p.y), "{p:?}");
            assert!(
                (0.0..=1.0 + 1.0 / 16.0).contains(&p.t) || p.t > -0.1,
                "{p:?}"
            );
        }
    }
}
