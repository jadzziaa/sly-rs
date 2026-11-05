//! A widescreen game's 2D, drawn for 4:3, narrowed back to 4:3.
//!
//! With a game's camera widened for 16:9 (`sly_common::hooks`), its 3D
//! fills the wider picture, but what it draws flat (captions, portraits,
//! counters) is still laid out for 4:3 and would be stretched. Narrowing
//! happens to the GIF packets before the GS takes them, so the CPU and GPU
//! renderers draw the same thing. In a displayed frame, these narrow to
//! 3/4 of their width:
//!
//! - each GIF tag sent by path 2 or 3 (VIF1's DIRECT, the GIF's DMA)
//!   drawn without a depth test (ZTE off, or ZTST ALWAYS) whose vertices
//!   do not reach both edges of the frame (VU1's flat output is projected
//!   3D, such as light cones, which the wider camera already widens);
//! - everything drawn in a sub-view, a scissor narrower than the frame
//!   (Sly 1's portraits are 3D models drawn in one), and the scissor
//!   itself.
//!
//! What lies wholly in the frame's left or right third keeps its distance
//! from that edge, as it sits in 4:3 (the portraits, captions); anything
//! else narrows towards the frame's centre (the text box, the credits).
//!
//! Depth-tested 3D in the whole frame, and flat things that span it (screen
//! passes, fades, the binocucom's outline), stay as they are, except for
//! vertices around a narrowed tag's box (a fade's hole cut to a logo),
//! which move with it; so does anything drawn to a frame the display does
//! not show (textures drawn into, such as shadows).
//!
//! A tag is judged as a whole, since its vertices may share primitives (a
//! strip); a tag carried over from an earlier transfer is left alone.

use crate::Gs;
use crate::regs::{addr, bits};

/// The display's frames (FBP, in FRAME's units), as V-blanks report them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Narrow {
    displayed: Vec<u32>,
}

/// Which point of the frame something keeps its distance from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Anchor {
    Left,
    Centre,
    Right,
}

/// The registers a tag's vertices are judged and moved by, followed through
/// a transfer.
#[derive(Clone, Copy)]
struct State {
    prim: u64,
    prmode: u64,
    prmodecont: u64,
    test: [u64; 2],
    frame: [u64; 2],
    xyoffset: [u64; 2],
    scissor: [u64; 2],
}

impl State {
    /// The context vertices go to now.
    const fn context(&self) -> usize {
        let source = if self.prmodecont & 1 != 0 {
            self.prim
        } else {
            self.prmode
        };
        ((source >> 9) & 1) as usize
    }

    fn write(&mut self, address: u8, value: u64) {
        match address {
            addr::PRIM => self.prim = value & 0x7FF,
            addr::PRMODE => self.prmode = value,
            addr::PRMODECONT => self.prmodecont = value,
            addr::TEST_1 | addr::TEST_2 => self.test[usize::from(address - addr::TEST_1)] = value,
            addr::FRAME_1 | addr::FRAME_2 => {
                self.frame[usize::from(address - addr::FRAME_1)] = value;
            }
            addr::SCISSOR_1 | addr::SCISSOR_2 => {
                self.scissor[usize::from(address - addr::SCISSOR_1)] = value;
            }
            addr::XYOFFSET_1 | addr::XYOFFSET_2 => {
                self.xyoffset[usize::from(address - addr::XYOFFSET_1)] = value;
            }
            _ => {}
        }
    }
}

/// Where a vertex's X is (a PACKED quadword's low bits, or a 64-bit
/// register value's), and its Y.
#[derive(Clone, Copy)]
struct Vertex {
    qword: usize,
    shift: u32,
    y_shift: u32,
    /// Its context's XYOFFSET and frame width, in 1/16 pixels.
    offset: [i64; 2],
    width: i64,
    /// Its scissor's top and bottom (exclusive), in 1/16 pixels.
    rows: [i64; 2],
    /// Drawn in a sub-view of a displayed frame: its anchor.
    sub: Option<Anchor>,
}

impl Vertex {
    /// A vertex at `qword`, X at `shift`, Y at `y_shift`.
    fn new(qword: usize, (shift, y_shift): (u32, u32), state: &State, narrow: &Narrow) -> Self {
        let context = state.context();
        let width = frame_width(state.frame[context]);
        let scissor = state.scissor[context];
        let xyoffset = state.xyoffset[context];
        Self {
            qword,
            shift,
            y_shift,
            offset: [bits(xyoffset, 0, 16), bits(xyoffset, 32, 16)].map(i64::from),
            width: width * 16,
            rows: {
                let [_, _, y0, y1] = scissor_box(scissor);
                [y0 * 16, (y1 + 1) * 16]
            },
            sub: (narrow.shown(state.frame[context]) && sub_view(scissor, width)).then(|| {
                let [x0, x1, _, _] = scissor_box(scissor);
                anchor(x0, x1, width)
            }),
        }
    }

    /// Whether `at` (its place) is a corner of the frame, as its scissor
    /// bounds it: exactly on its left or right edge, and within a pixel of
    /// its top or bottom (Sly 1 draws every other field half a line up).
    fn at_corner(&self, [x, y]: [i64; 2]) -> bool {
        let near = |a: i64, b: i64| (a - b).abs() <= 16;
        (x == 0 || x == self.width) && (near(y, self.rows[0]) || near(y, self.rows[1]))
    }

    /// Its X and Y in its frame, in 1/16 pixels.
    fn place(&self, data: &[u128]) -> [i64; 2] {
        let at = |shift: u32| i64::from(((data[self.qword] >> shift) & 0xFFFF) as u16);
        [
            at(self.shift) - self.offset[0],
            at(self.y_shift) - self.offset[1],
        ]
    }
}

/// A SCISSOR's X and Y bounds, in pixels.
fn scissor_box(scissor: u64) -> [i64; 4] {
    [0, 16, 32, 48].map(|at| i64::from(bits(scissor, at, 11)))
}

/// Where something from `x0` to `x1` in a frame `width` wide (any unit)
/// keeps its place: at the edge whose third it lies wholly in, else at the
/// centre.
const fn anchor(x0: i64, x1: i64, width: i64) -> Anchor {
    if x1 * 3 <= width {
        Anchor::Left
    } else if x0 * 3 >= width * 2 {
        Anchor::Right
    } else {
        Anchor::Centre
    }
}

/// A frame's width in pixels.
fn frame_width(frame: u64) -> i64 {
    i64::from(bits(frame, 16, 6)) * 64
}

/// Whether a scissor leaves out either edge of a frame `width` pixels wide.
fn sub_view(scissor: u64, width: i64) -> bool {
    let (x0, x1) = (
        i64::from(bits(scissor, 0, 11)),
        i64::from(bits(scissor, 16, 11)),
    );
    x0 > 8 || x1 < width - 1 - 8
}

/// X in a frame `width` wide (in any unit), its distance from `anchor`
/// narrowed to 3/4.
const fn narrowed(x: i64, width: i64, anchor: Anchor) -> i64 {
    match anchor {
        Anchor::Left => three_quarters(x),
        Anchor::Centre => width / 2 + three_quarters(x - width / 2),
        Anchor::Right => width - three_quarters(width - x),
    }
}

/// A distance at 3/4, rounded to the nearest.
const fn three_quarters(d: i64) -> i64 {
    (d * 3 + 2).div_euclid(4)
}

/// A SCISSOR value with its X range narrowed (its right bound inclusive).
fn narrow_scissor(scissor: u64, width: i64, anchor: Anchor) -> u64 {
    let [x0, x1, _, _] = scissor_box(scissor);
    let field = |x: i64, at: u32| u64::try_from(x.clamp(0, 0x7FF)).unwrap_or(0) << at;
    (scissor & !(0x7FF | 0x7FF << 16))
        | field(narrowed(x0, width, anchor), 0)
        | field(narrowed(x1 + 1, width, anchor) - 1, 16)
}

const fn is_vertex(address: u8) -> bool {
    matches!(address, addr::XYZF2 | addr::XYZ2 | addr::XYZF3 | addr::XYZ3)
}

impl Narrow {
    /// Notes the frames the display shows (DISPFB1 and 2, as PMODE enables
    /// them); the last four kept.
    pub(crate) fn displayed(&mut self, regs: &crate::Privileged) {
        for (k, dispfb) in regs.dispfb.iter().enumerate() {
            if regs.pmode & (1 << k) == 0 {
                continue;
            }
            let fbp = bits(*dispfb, 0, 9);
            if !self.displayed.contains(&fbp) {
                if self.displayed.len() == 4 {
                    self.displayed.remove(0);
                }
                self.displayed.push(fbp);
            }
        }
    }

    /// Whether `frame` is one the display shows.
    fn shown(&self, frame: u64) -> bool {
        self.displayed.contains(&bits(frame, 0, 9))
    }

    /// Whether a vertex sent now belongs to flat 2D in a shown frame.
    fn flat(&self, state: &State) -> bool {
        let context = state.context();
        let test = state.test[context];
        let no_depth = bits(test, 16, 1) == 0 || bits(test, 17, 2) == 1;
        no_depth && self.shown(state.frame[context])
    }
}

/// The context a SCISSOR register belongs to.
fn scissor_context(address: u8) -> Option<usize> {
    match address {
        addr::SCISSOR_1 => Some(0),
        addr::SCISSOR_2 => Some(1),
        _ => None,
    }
}

impl Gs {
    /// Narrows `data`'s flat tags and sub-views (see the module), if
    /// widescreen is on.
    pub(crate) fn narrow(&self, path: usize, data: &mut [u128]) {
        let Some(narrow) = &self.narrow else {
            return;
        };
        let mut state = State {
            prim: self.prim,
            prmode: self.prmode,
            prmodecont: self.prmodecont,
            test: [self.ctx[0].test, self.ctx[1].test],
            frame: [self.ctx[0].frame, self.ctx[1].frame],
            xyoffset: [self.ctx[0].xyoffset, self.ctx[1].xyoffset],
            scissor: [self.ctx[0].scissor, self.ctx[1].scissor],
        };
        let mut at = self.paths[path].remaining();
        let mut vertices = Vec::new();
        // The transfer's flat tags, judged together once it is walked, and
        // those that span the frame.
        let mut pieces: Vec<Piece> = Vec::new();
        let mut fills: Vec<Piece> = Vec::new();
        while at < data.len() {
            let tag = data[at];
            at += 1;
            let lo = tag as u64;
            let nloop = bits(lo, 0, 15) as usize;
            let flg = bits(lo, 58, 2);
            let nreg = match bits(lo, 60, 4) {
                0 => 16,
                n => n as usize,
            };
            let desc = |k: usize| ((tag >> (64 + 4 * (k % nreg))) & 0xF) as u8;
            vertices.clear();
            let mut flat = path != 1;
            match flg {
                0 => {
                    if bits(lo, 46, 1) != 0 {
                        state.prim = u64::from(bits(lo, 47, 11));
                    }
                    for k in 0..nloop * nreg {
                        let Some(&q) = data.get(at) else { break };
                        let (address, value) = match desc(k) {
                            0xE => (((q >> 64) & 0xFF) as u8, q as u64),
                            0xF => (0xFF, 0),
                            d => (d, q as u64),
                        };
                        if is_vertex(address) {
                            flat &= narrow.flat(&state);
                            // XYZ2 and XYZF2 in their PACKED forms keep Y
                            // in the second word; as register values, in
                            // the first.
                            let y = if matches!(desc(k), 4 | 5) { 32 } else { 16 };
                            vertices.push(Vertex::new(at, (0, y), &state, narrow));
                        } else if let Some(context) = scissor_context(address) {
                            // A sub-view's scissor narrows with what it holds.
                            let frame = state.frame[context];
                            let width = frame_width(frame);
                            let mut value = value;
                            if narrow.shown(frame) && sub_view(value, width) {
                                let [x0, x1, _, _] = scissor_box(value);
                                value = narrow_scissor(value, width, anchor(x0, x1, width));
                                data[at] = (data[at] >> 64 << 64) | u128::from(value);
                            }
                            state.write(address, value);
                        } else {
                            state.write(address, value);
                        }
                        at += 1;
                    }
                }
                1 => {
                    for k in 0..nloop * nreg {
                        let qword = at + k / 2;
                        let Some(&q) = data.get(qword) else { break };
                        let shift = 64 * (k as u32 % 2);
                        let address = desc(k);
                        if is_vertex(address) {
                            flat &= narrow.flat(&state);
                            vertices.push(Vertex::new(qword, (shift, shift + 16), &state, narrow));
                        } else if address < 0xE {
                            state.write(address, (q >> shift) as u64);
                        }
                    }
                    at += (nloop * nreg).div_ceil(2);
                }
                _ => at += nloop,
            }
            if let Some(anchor) = vertices.iter().find_map(|v| v.sub) {
                narrow_tag(data, &vertices, anchor);
            } else if flat && !vertices.is_empty() {
                let piece = Piece::new(data, &vertices);
                let spans = piece.bounds[0] <= EDGE && piece.bounds[1] >= vertices[0].width - EDGE;
                if spans {
                    fills.push(piece);
                } else {
                    pieces.push(piece);
                }
            }
        }
        narrow_framed(data, &mut fills);
        let moved = narrow_pieces(data, &mut pieces);
        follow_pieces(data, &fills, &moved);
    }
}

/// A flat tag: its vertices and their bounds (X, then Y; 1/16 pixels).
struct Piece {
    vertices: Vec<Vertex>,
    bounds: [i64; 4],
    /// The piece whose group it joined (itself at first).
    group: usize,
}

impl Piece {
    fn new(data: &[u128], vertices: &[Vertex]) -> Self {
        let mut bounds = [i64::MAX, i64::MIN, i64::MAX, i64::MIN];
        for v in vertices {
            let [x, y] = v.place(data);
            bounds = [
                bounds[0].min(x),
                bounds[1].max(x),
                bounds[2].min(y),
                bounds[3].max(y),
            ];
        }
        Self {
            vertices: vertices.to_vec(),
            bounds,
            group: 0,
        }
    }

    /// Whether two pieces belong to one block: they overlap across, and
    /// are at most a line apart down (the lines of a caption).
    fn near(&self, other: &Self) -> bool {
        let line = 16 * 16;
        let [a, b] = [self.bounds, other.bounds];
        a[0] <= b[1] && b[0] <= a[1] && a[2] <= b[3] + line && b[2] <= a[3] + line
    }
}

/// Narrows a transfer's flat pieces by blocks: pieces near each other
/// (directly or through others) share the anchor of their joint bounds, so
/// that a caption's lines stay together. Returns each piece's bounds as
/// they were, and the anchor it moved by.
fn narrow_pieces(data: &mut [u128], pieces: &mut [Piece]) -> Vec<([i64; 4], Anchor)> {
    fn root(pieces: &[Piece], mut k: usize) -> usize {
        while pieces[k].group != k {
            k = pieces[k].group;
        }
        k
    }
    for (k, piece) in pieces.iter_mut().enumerate() {
        piece.group = k;
    }
    for k in 0..pieces.len() {
        for j in 0..k {
            if pieces[k].near(&pieces[j]) {
                let (rk, rj) = (root(pieces, k), root(pieces, j));
                pieces[rk].group = rj;
            }
        }
    }
    let mut moved = Vec::new();
    for k in 0..pieces.len() {
        let group = root(pieces, k);
        let (x0, x1) = pieces
            .iter()
            .enumerate()
            .filter(|&(j, _)| root(pieces, j) == group)
            .fold((i64::MAX, i64::MIN), |(lo, hi), (_, p)| {
                (lo.min(p.bounds[0]), hi.max(p.bounds[1]))
            });
        let width = pieces[k].vertices[0].width;
        let anchor = anchor(x0, x1, width);
        narrow_tag(data, &pieces[k].vertices, anchor);
        moved.push((pieces[k].bounds, anchor));
    }
    moved
}

/// Narrows what a fade frames while it is larger than the frame. Sly 1's
/// fades close on (and open from) its logo through a fill over the whole
/// frame, its corners on the frame's, with a hole cut to the logo's box;
/// grown past the frame's edges, the logo's tags span it too. So that the
/// logo keeps its 4:3 shape at every size, when a tag spanning the frame
/// fills such a hole across, it and the others lying within the hole narrow
/// towards the centre, and the hole with them; they and the fill are taken
/// out of `fills`. Fills proper (vertices exactly on both edges) never count
/// as framed, and the binocucom's tick band, as wide but in no hole, stays.
fn narrow_framed(data: &mut [u128], fills: &mut Vec<Piece>) {
    let near = |a: i64, b: i64| (a - b).abs() <= 16;
    let places: Vec<Vec<[i64; 2]>> = fills
        .iter()
        .map(|f| f.vertices.iter().map(|v| v.place(data)).collect())
        .collect();
    let proper: Vec<bool> = fills
        .iter()
        .zip(&places)
        .map(|(f, at)| {
            let width = f.vertices[0].width;
            at.iter().any(|&[x, _]| x == 0) && at.iter().any(|&[x, _]| x == width)
        })
        .collect();
    let mut done = vec![false; fills.len()];
    for k in 0..fills.len() {
        let (fill, at) = (&fills[k], &places[k]);
        let corner: Vec<bool> = fill
            .vertices
            .iter()
            .zip(at)
            .map(|(v, &p)| v.at_corner(p))
            .collect();
        if !corner.contains(&true) || !corner.contains(&false) {
            continue;
        }
        let hole = at.iter().zip(&corner).filter(|&(_, &c)| !c).fold(
            [i64::MAX, i64::MIN, i64::MAX, i64::MIN],
            |b, (&[x, y], _)| [b[0].min(x), b[1].max(x), b[2].min(y), b[3].max(y)],
        );
        // A tiled fill's inner vertices are no hole.
        if near(hole[0], 0) && near(hole[1], fill.vertices[0].width) {
            continue;
        }
        let framed: Vec<usize> = (0..fills.len())
            .filter(|&j| {
                let b = fills[j].bounds;
                j != k
                    && !done[j]
                    && !proper[j]
                    && b[0] >= hole[0] - 16
                    && b[1] <= hole[1] + 16
                    && b[2] >= hole[2] - 16
                    && b[3] <= hole[3] + 16
            })
            .collect();
        let across = framed
            .iter()
            .any(|&j| near(fills[j].bounds[0], hole[0]) && near(fills[j].bounds[1], hole[1]));
        if !across {
            continue;
        }
        for &j in &framed {
            narrow_tag(data, &fills[j].vertices, Anchor::Centre);
            done[j] = true;
        }
        let edge: Vec<Vertex> = fill
            .vertices
            .iter()
            .zip(&corner)
            .filter(|&(_, &c)| !c)
            .map(|(v, _)| *v)
            .collect();
        narrow_tag(data, &edge, Anchor::Centre);
        done[k] = true;
    }
    let mut k = 0;
    fills.retain(|_| {
        k += 1;
        !done[k - 1]
    });
}

/// Moves the vertices of flat tags that span the frame which lie on a
/// narrowed piece's sides with that piece, when the fill has vertices on
/// both its left and right sides: a fill drawn around a piece, its hole cut
/// to the piece's box (Sly 1's fade to its logo), keeps closing on it.
/// Vertices elsewhere stay, so the fill still spans the frame; a fill that
/// only meets a piece on one side (the binocucom's, at the portraits' frames)
/// stays as it is.
fn follow_pieces(data: &mut [u128], fills: &[Piece], pieces: &[([i64; 4], Anchor)]) {
    let near = |a: i64, b: i64| (a - b).abs() <= 16;
    for fill in fills {
        let width = fill.vertices[0].width;
        // Vertices at the frame's edges stay there.
        let inside = |x: i64| x > EDGE && x < width - EDGE;
        let at: Vec<_> = fill.vertices.iter().map(|v| v.place(data)).collect();
        let mut moved = vec![false; at.len()];
        for &(b, anchor) in pieces {
            let on = |side: i64| -> Vec<bool> {
                at.iter()
                    .map(|&[x, y]| inside(x) && near(x, side) && y >= b[2] - 16 && y <= b[3] + 16)
                    .collect()
            };
            let (left, right) = (on(b[0]), on(b[1]));
            if !left.contains(&true) || !right.contains(&true) {
                continue;
            }
            for (k, v) in fill.vertices.iter().enumerate() {
                if (left[k] || right[k]) && !moved[k] {
                    narrow_tag(data, std::slice::from_ref(v), anchor);
                    moved[k] = true;
                }
            }
        }
    }
}

/// How near an edge (1/16 pixels) a flat tag must reach to span the frame.
const EDGE: i64 = 8 * 16;

/// Narrows a tag's vertices' distance from `anchor` to 3/4.
fn narrow_tag(data: &mut [u128], vertices: &[Vertex], anchor: Anchor) {
    for v in vertices {
        let [x, _] = v.place(data);
        let moved = narrowed(x, v.width, anchor) + v.offset[0];
        let raw = u128::from(u16::try_from(moved.clamp(0, 0xFFFF)).unwrap_or(0));
        let mask = 0xFFFF_u128 << v.shift;
        data[v.qword] = (data[v.qword] & !mask) | (raw << v.shift);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PACKED tag of XYZ2 vertices at `xs` on line 0 (pixels, offset 0).
    fn sprites(xs: &[u32]) -> Vec<u128> {
        sprites_at(xs, 0)
    }

    /// The same on line `y`.
    fn sprites_at(xs: &[u32], y: u32) -> Vec<u128> {
        let points: Vec<_> = xs.iter().map(|&x| (x, y)).collect();
        sprites_through(&points)
    }

    /// A PACKED tag of XYZ2 vertices at `points` (pixels, offset 0).
    fn sprites_through(points: &[(u32, u32)]) -> Vec<u128> {
        let n = points.len() as u128;
        let tag = n | 1 << 15 | 1 << 46 | 6 << 47 | 1 << 60 | 5 << 64;
        let mut data = vec![tag];
        data.extend(
            points
                .iter()
                .map(|&(x, y)| u128::from(x * 16) | u128::from(y * 16) << 32),
        );
        data
    }

    fn gs() -> Gs {
        let mut gs = Gs {
            narrow: Some(Narrow { displayed: vec![0] }),
            prmodecont: 1,
            ..Gs::default()
        };
        // A 640-wide frame at FBP 0; no depth test.
        gs.ctx[0].frame = 10 << 16;
        gs.ctx[0].test = 0;
        // The scissor is the whole frame.
        gs.ctx[0].scissor = 0x27F << 16 | 0xDF << 48;
        gs
    }

    #[test]
    fn flat_tags_narrow_towards_their_anchor_and_full_width_ones_stay() {
        let gs = gs();
        // Across the middle: 320 + (200 - 320) * 3/4 = 230, and 410.
        let mut data = sprites(&[200, 440]);
        gs.narrow(3, &mut data);
        assert_eq!((data[1], data[2]), (230 * 16, 410 * 16));
        // In the left third: from the left edge.
        let mut data = sprites(&[0, 160]);
        gs.narrow(3, &mut data);
        assert_eq!((data[1], data[2]), (0, 120 * 16));
        // Across the frame: as it is.
        let mut data = sprites(&[0, 640]);
        gs.narrow(3, &mut data);
        assert_eq!((data[1], data[2]), (0, 640 * 16));
        // From VU1: projected 3D, as it is.
        let mut data = sprites(&[200, 440]);
        gs.narrow(1, &mut data);
        assert_eq!((data[1], data[2]), (200 * 16, 440 * 16));
    }

    #[test]
    fn sub_views_narrow_with_their_scissor_even_when_depth_tested() {
        let mut gs = gs();
        gs.ctx[0].test = 1 << 16 | 2 << 17;
        // A+D SCISSOR_1 = x 1..160, then two vertices.
        let tag = 1 | 1 << 15 | 1 << 60 | 0xE << 64;
        let scissor = 0x1 | 0xA0 << 16 | 0x9D << 32 | 0xD5 << 48;
        let mut data = vec![tag, u128::from(addr::SCISSOR_1) << 64 | scissor];
        data.extend(sprites(&[1, 160]));
        gs.narrow(1, &mut data);
        // In the left third: it keeps its distance from the left edge.
        assert_eq!(data[1] as u64, 0x1 | 0x78 << 16 | 0x9D << 32 | 0xD5 << 48);
        assert_eq!((data[3], data[4]), (12, 120 * 16));
        // Flat things in the same third go the same way.
        let mut data = sprites_at(&[10, 150], 180);
        gs.ctx[0].test = 0;
        gs.ctx[0].scissor = 0x27F << 16 | 0xDF << 48;
        gs.narrow(3, &mut data);
        let line = u128::from(180_u32 * 16) << 32;
        assert_eq!((data[1], data[2]), (0x78 | line, 0x708 | line));
    }

    #[test]
    fn a_captions_lines_keep_one_anchor() {
        let gs = gs();
        // A long first line across the middle, a short second one under it
        // in the left third: both narrow towards the centre.
        let mut data = sprites_at(&[47, 480], 100);
        data.extend(sprites_at(&[62, 162], 115));
        gs.narrow(3, &mut data);
        let x = |q: u128| i64::from((q & 0xFFFF) as u16);
        let centre = |px: i64| narrowed(px * 16, 640 * 16, Anchor::Centre);
        assert_eq!((x(data[4]), x(data[5])), (centre(62), centre(162)));
        // Alone, the second line keeps to the left edge.
        let mut data = sprites_at(&[62, 162], 115);
        gs.narrow(3, &mut data);
        assert_eq!(x(data[1]), narrowed(62 * 16, 640 * 16, Anchor::Left));
    }

    #[test]
    fn a_fill_around_a_block_closes_on_it() {
        let gs = gs();
        // A frame around a hole at x 280-360, y 90-130 (Sly 1's fade to its
        // logo), then the logo in the hole.
        let mut data = sprites_through(&[
            (0, 0),
            (280, 90),
            (640, 0),
            (360, 90),
            (640, 224),
            (360, 130),
            (0, 224),
            (280, 130),
        ]);
        data.extend(sprites_through(&[(280, 90), (360, 130)]));
        gs.narrow(3, &mut data);
        let x = |q: u128| (q & 0xFFFF) as u32 / 16;
        // The logo narrows to 290-350, and the hole with it.
        assert_eq!((x(data[10]), x(data[11])), (290, 350));
        let xs: Vec<_> = data[1..9].iter().map(|&q| x(q)).collect();
        assert_eq!(xs, [0, 290, 640, 350, 640, 350, 0, 290]);
        // A fill that meets a piece on one side only (the binocucom's at a
        // portrait's frame) stays.
        let mut data = sprites_through(&[(0, 0), (640, 0), (0, 160), (160, 160)]);
        data.extend(sprites_through(&[(0, 160), (160, 220)]));
        gs.narrow(3, &mut data);
        assert_eq!((x(data[3]), x(data[4])), (0, 160));
        assert_eq!(x(data[7]), 120);
    }

    #[test]
    fn a_logo_larger_than_the_frame_keeps_narrowing_in_its_fade() {
        let mut gs = gs();
        // An XYOFFSET of 1000 pixels, so that vertices can lie left of the
        // frame.
        gs.ctx[0].xyoffset = 1000 * 16;
        let tag = |points: &[(i32, i32)]| {
            let shifted: Vec<_> = points
                .iter()
                .map(|&(x, y)| (u32::try_from(x + 1000).unwrap(), u32::try_from(y).unwrap()))
                .collect();
            sprites_through(&shifted)
        };
        // The fade's fill, its corners on the frame's and its hole at x -20
        // to 660 (the logo's box, wider than the frame); the logo across the
        // hole and a smaller part of it, both spanning the frame; and a wide
        // row of sprites in no hole (the binocucom's tick band).
        let mut data = tag(&[
            (0, 0),
            (-20, 10),
            (640, 0),
            (660, 10),
            (640, 224),
            (660, 200),
            (0, 224),
            (-20, 200),
        ]);
        data.extend(tag(&[(-20, 10), (660, 200)]));
        data.extend(tag(&[(-4, 50), (644, 150)]));
        data.extend(tag(&[(-200, 5), (840, 20)]));
        gs.narrow(3, &mut data);
        let x = |q: u128| i32::from((q & 0xFFFF) as u16) / 16 - 1000;
        let centre =
            |px: i64| i32::try_from(narrowed(px * 16, 640 * 16, Anchor::Centre) / 16).unwrap();
        let fill: Vec<_> = data[1..9].iter().map(|&q| x(q)).collect();
        let (l, r) = (centre(-20), centre(660));
        assert_eq!(fill, [0, l, 640, r, 640, r, 0, l]);
        assert_eq!((x(data[10]), x(data[11])), (l, r));
        assert_eq!((x(data[13]), x(data[14])), (centre(-4), centre(644)));
        // The tick band stays.
        assert_eq!((x(data[16]), x(data[17])), (-200, 840));
    }

    #[test]
    fn sub_views_keep_the_edge_of_their_third() {
        assert_eq!(anchor(1, 160, 640), Anchor::Left);
        assert_eq!(anchor(180, 459, 640), Anchor::Centre);
        assert_eq!(anchor(479, 638, 640), Anchor::Right);
        let scissor = 0x1DF | 0x27E << 16;
        let [x0, x1, _, _] = scissor_box(narrow_scissor(scissor, 640, Anchor::Right));
        assert_eq!((x0, x1), (519, 638));
    }

    #[test]
    fn depth_tested_tags_and_hidden_frames_stay() {
        let mut tested = gs();
        // ZTE on, ZTST GEQUAL.
        tested.ctx[0].test = 1 << 16 | 2 << 17;
        let mut data = sprites(&[0, 160]);
        tested.narrow(1, &mut data);
        assert_eq!((data[1], data[2]), (0, 160 * 16));
        let mut gs = gs();
        gs.ctx[0].frame = 10 << 16 | 0x46;
        let mut data = sprites(&[0, 160]);
        gs.narrow(1, &mut data);
        assert_eq!((data[1], data[2]), (0, 160 * 16));
    }
}
