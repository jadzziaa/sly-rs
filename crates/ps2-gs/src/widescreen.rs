//! A widescreen game's 2D, drawn for 4:3, narrowed back to 4:3.
//!
//! With a game's camera widened for 16:9 (`sly_common::hooks`), its 3D
//! fills the wider picture, but what it draws flat (captions, portraits,
//! counters) is still laid out for 4:3 and would be stretched. Narrowing
//! happens to the GIF packets before the GS takes them, so the CPU and GPU
//! renderers draw the same thing. In a displayed frame, these narrow to
//! 3/4 of their width:
//!
//! - each GIF tag drawn without a depth test (ZTE off, or ZTST ALWAYS)
//!   whose vertices do not reach both edges of the frame;
//! - everything drawn in a sub-view, a scissor narrower than the frame
//!   (Sly 1's portraits are 3D models drawn in one), and the scissor
//!   itself.
//!
//! A sub-view in the left or right third of the frame keeps its distance
//! from that edge, as it sits in 4:3 (the portraits at the screen's
//! edges); anything else narrows towards the frame's centre. A flat tag
//! centred in an edge sub-view's box goes with it (the portraits' static
//! and outlines, drawn without their scissor).
//!
//! Depth-tested 3D in the whole frame, and flat things that span it (screen
//! passes, fades, the binocucom's outline), stay as they are; so does
//! anything drawn to a frame the display does not show (textures drawn
//! into, such as shadows).
//!
//! A tag is judged as a whole, since its vertices may share primitives (a
//! strip); a tag carried over from an earlier transfer is left alone.

use crate::Gs;
use crate::regs::{addr, bits};

/// The display's frames (FBP, in FRAME's units), as V-blanks report them,
/// and the edge sub-views seen.
#[derive(Clone, Debug, Default)]
pub(crate) struct Narrow {
    displayed: Vec<u32>,
    boxes: Vec<SubView>,
}

/// Which point of the frame something keeps its distance from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Anchor {
    Left,
    Centre,
    Right,
}

/// An edge sub-view's box, in pixels as the game set it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SubView {
    x: [i64; 2],
    y: [i64; 2],
    anchor: Anchor,
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
            sub: (narrow.shown(state.frame[context]) && sub_view(scissor, width)).then(|| {
                let [x0, x1, _, _] = scissor_box(scissor);
                anchor(x0, x1, width)
            }),
        }
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

/// Where a box from `x0` to `x1` in a frame `width` wide (pixels) keeps its
/// place: at the edge whose third it is in, else at the centre.
const fn anchor(x0: i64, x1: i64, width: i64) -> Anchor {
    let twice_centre = x0 + x1;
    if twice_centre * 3 < width * 2 {
        Anchor::Left
    } else if twice_centre * 3 > width * 4 {
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

    /// Notes an edge sub-view's box (the last eight kept).
    fn note(&mut self, scissor: u64, width: i64) {
        let [x0, x1, y0, y1] = scissor_box(scissor);
        let anchor = anchor(x0, x1, width);
        let sub = SubView {
            x: [x0, x1],
            y: [y0, y1],
            anchor,
        };
        if anchor != Anchor::Centre && !self.boxes.contains(&sub) {
            if self.boxes.len() == 8 {
                self.boxes.remove(0);
            }
            self.boxes.push(sub);
        }
    }

    /// The anchor of the edge sub-view whose box holds the centre of a flat
    /// tag's vertices, if one does (outlines drawn around a box reach
    /// beyond it).
    fn anchor_of(&self, places: &[[i64; 2]]) -> Option<Anchor> {
        let centre = |k: usize| {
            let (lo, hi) = places.iter().fold((i64::MAX, i64::MIN), |(lo, hi), p| {
                (lo.min(p[k]), hi.max(p[k]))
            });
            i64::midpoint(lo, hi)
        };
        let (x, y) = (centre(0), centre(1));
        self.boxes.iter().rev().find_map(|b| {
            ((b.x[0] * 16..=b.x[1] * 16 + 16).contains(&x)
                && (b.y[0] * 16..=b.y[1] * 16 + 16).contains(&y))
            .then_some(b.anchor)
        })
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
    pub(crate) fn narrow(&mut self, path: usize, data: &mut [u128]) {
        let Some(mut narrow) = self.narrow.take() else {
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
            let mut flat = true;
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
                            vertices.push(Vertex::new(at, (0, y), &state, &narrow));
                        } else if let Some(context) = scissor_context(address) {
                            // A sub-view's scissor narrows with what it holds.
                            let frame = state.frame[context];
                            let width = frame_width(frame);
                            let mut value = value;
                            if narrow.shown(frame) && sub_view(value, width) {
                                narrow.note(value, width);
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
                            vertices.push(Vertex::new(qword, (shift, shift + 16), &state, &narrow));
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
                let places: Vec<[i64; 2]> = vertices.iter().map(|v| v.place(data)).collect();
                let spans = places.iter().any(|p| p[0] <= EDGE)
                    && places
                        .iter()
                        .zip(&vertices)
                        .any(|(p, v)| p[0] >= v.width - EDGE);
                if !spans {
                    let anchor = narrow.anchor_of(&places).unwrap_or(Anchor::Centre);
                    narrow_tag(data, &vertices, anchor);
                }
            }
        }
        self.narrow = Some(narrow);
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
        let n = xs.len() as u128;
        let tag = n | 1 << 15 | 1 << 46 | 6 << 47 | 1 << 60 | 5 << 64;
        let mut data = vec![tag];
        data.extend(
            xs.iter()
                .map(|&x| u128::from(x * 16) | u128::from(y * 16) << 32),
        );
        data
    }

    fn gs() -> Gs {
        let mut gs = Gs {
            narrow: Some(Narrow {
                displayed: vec![0],
                ..Narrow::default()
            }),
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
    fn flat_tags_narrow_towards_the_centre_and_full_width_ones_stay() {
        let mut gs = gs();
        let mut data = sprites(&[0, 160]);
        gs.narrow(1, &mut data);
        // 320 + (0 - 320) * 3/4 = 80; 320 + (160 - 320) * 3/4 = 200.
        assert_eq!((data[1], data[2]), (80 * 16, 200 * 16));
        let mut data = sprites(&[0, 640]);
        gs.narrow(1, &mut data);
        assert_eq!((data[1], data[2]), (0, 640 * 16));
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
        // Flat things inside its box go with it.
        let mut data = sprites_at(&[10, 150], 180);
        gs.ctx[0].test = 0;
        gs.ctx[0].scissor = 0x27F << 16 | 0xDF << 48;
        gs.narrow(1, &mut data);
        let line = u128::from(180_u32 * 16) << 32;
        assert_eq!((data[1], data[2]), (0x78 | line, 0x708 | line));
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
