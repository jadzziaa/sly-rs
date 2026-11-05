//! A widescreen game's 2D, drawn for 4:3, narrowed back to 4:3.
//!
//! With a game's camera widened for 16:9 (`sly_common::hooks`), its 3D
//! fills the wider picture, but what it draws flat (captions, portraits,
//! counters) is still laid out for 4:3 and would be stretched. Narrowing
//! happens to the GIF packets before the GS takes them, so the CPU and GPU
//! renderers draw the same thing. In a displayed frame, X moves 3/4 of the
//! way to the frame's centre for:
//!
//! - each GIF tag drawn without a depth test (ZTE off, or ZTST ALWAYS)
//!   whose vertices do not reach both edges of the frame;
//! - everything drawn in a sub-view, a scissor narrower than the frame
//!   (Sly 1's portraits are 3D models drawn in one), and the scissor
//!   itself.
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

/// The display's frames (FBP, in FRAME's units), as V-blanks report them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Narrow {
    displayed: Vec<u32>,
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

/// Where a vertex's X is: a PACKED quadword's low bits, or a REGLIST half.
#[derive(Clone, Copy)]
struct Vertex {
    qword: usize,
    shift: u32,
    /// Its context's XYOFFSET X and frame width, in 1/16 pixels.
    offset: i64,
    width: i64,
    /// Drawn in a sub-view of a displayed frame.
    sub: bool,
}

impl Vertex {
    fn new(qword: usize, shift: u32, state: &State, narrow: &Narrow) -> Self {
        let context = state.context();
        let width = frame_width(state.frame[context]);
        Self {
            qword,
            shift,
            offset: i64::from(bits(state.xyoffset[context], 0, 16)),
            width: width * 16,
            sub: narrow.shown(state.frame[context]) && sub_view(state.scissor[context], width),
        }
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

/// X in a frame `width` pixels wide (in any unit), 3/4 of the way to its
/// centre.
const fn towards_centre(x: i64, width: i64) -> i64 {
    let centre = width / 2;
    centre + ((x - centre) * 3).div_euclid(4)
}

/// A SCISSOR value with its X range narrowed.
fn narrow_scissor(scissor: u64, width: i64) -> u64 {
    let x = |at| {
        let moved = towards_centre(i64::from(bits(scissor, at, 11)), width);
        u64::try_from(moved.clamp(0, 0x7FF)).unwrap_or(0) << at
    };
    (scissor & !(0x7FF | 0x7FF << 16)) | x(0) | x(16)
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
    /// Narrows `data`'s flat tags (see the module), if widescreen is on.
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
                            vertices.push(Vertex::new(at, 0, &state, narrow));
                        } else if let Some(context) = scissor_context(address) {
                            // A sub-view's scissor narrows with what it holds.
                            let frame = state.frame[context];
                            let width = frame_width(frame);
                            let mut value = value;
                            if narrow.shown(frame) && sub_view(value, width) {
                                value = narrow_scissor(value, width);
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
                            vertices.push(Vertex::new(qword, shift, &state, narrow));
                        } else if address < 0xE {
                            state.write(address, (q >> shift) as u64);
                        }
                    }
                    at += (nloop * nreg).div_ceil(2);
                }
                _ => at += nloop,
            }
            if vertices.iter().any(|v| v.sub) {
                narrow_tag(data, &vertices, false);
            } else if flat && !vertices.is_empty() {
                narrow_tag(data, &vertices, true);
            }
        }
    }
}

/// Moves a tag's vertices 3/4 of the way to the frame's centre, unless
/// `spanning` ones are to stay and they reach both of its edges.
fn narrow_tag(data: &mut [u128], vertices: &[Vertex], spanning: bool) {
    // Each vertex's X in its frame, in 1/16 pixels.
    let xs: Vec<i64> = vertices
        .iter()
        .map(|v| i64::from(((data[v.qword] >> v.shift) & 0xFFFF) as u16) - v.offset)
        .collect();
    let edge = 8 * 16;
    let left = xs.iter().any(|&x| x <= edge);
    let right = xs.iter().zip(vertices).any(|(&x, v)| x >= v.width - edge);
    if spanning && left && right {
        return;
    }
    for (v, x) in vertices.iter().zip(xs) {
        let moved = towards_centre(x, v.width) + v.offset;
        let raw = u128::from(u16::try_from(moved.clamp(0, 0xFFFF)).unwrap_or(0));
        let mask = 0xFFFF_u128 << v.shift;
        data[v.qword] = (data[v.qword] & !mask) | (raw << v.shift);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PACKED tag of `n` XYZ2 vertices (X in pixels, offset 0).
    fn sprites(xs: &[u32]) -> Vec<u128> {
        let n = xs.len() as u128;
        let tag = n | 1 << 15 | 1 << 46 | 6 << 47 | 1 << 60 | 5 << 64;
        let mut data = vec![tag];
        data.extend(xs.iter().map(|&x| u128::from(x * 16)));
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
    fn flat_tags_narrow_towards_the_centre_and_full_width_ones_stay() {
        let gs = gs();
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
        assert_eq!(data[1] as u64, 0x50 | 0xC8 << 16 | 0x9D << 32 | 0xD5 << 48);
        assert_eq!((data[3], data[4]), (80 * 16 + 12, 200 * 16));
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
