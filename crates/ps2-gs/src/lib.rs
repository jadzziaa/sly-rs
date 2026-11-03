//! The Graphics Synthesizer (plan §5, phase 6), modelled in software: its
//! local memory in every pixel format, the GIF packets that set its
//! registers and upload images, drawing, and the display circuits' read-out
//! of a field as RGB.
//!
//! The GS draws each primitive as its last vertex arrives, so state is
//! always current: a CLUT loads when TEX0 is written, a transfer's pixels
//! land as its data comes.

// GS registers pack fields of every width into 64-bit words; the casts take
// those fields apart and are the hardware's behaviour.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    reason = "register fields"
)]

mod batch;
pub mod display;
mod draw;
pub mod dump;
pub mod gif;
pub mod mem;
pub mod regs;
mod tables;
pub mod thread;
mod transfer;

pub use display::{Frame, Privileged};
pub use mem::{Memory, Psm};

use regs::{Context, Tex0, addr, bits};

/// A vertex as the GS queues it: position in 12.4 window coordinates
/// (before XYOFFSET), depth, fog and the attribute registers when it came.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Vertex {
    x: u32,
    y: u32,
    z: u32,
    f: u8,
    rgba: [u8; 4],
    q: f32,
    s: f32,
    t: f32,
    u: u32,
    v: u32,
}

/// Events GIF packets asked for (writes to SIGNAL, FINISH and LABEL),
/// collected for the EE side to raise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Events {
    /// SIGNAL was written: its value.
    pub signal: Option<u64>,
    /// FINISH was written.
    pub finish: bool,
    /// LABEL was written: its value.
    pub label: Option<u64>,
}

/// The GS.
pub struct Gs {
    /// Local memory.
    pub mem: Memory,
    /// The two drawing contexts.
    pub ctx: [Context; 2],
    prim: u64,
    prmode: u64,
    prmodecont: u64,
    rgbaq: u64,
    st: u64,
    uv: u64,
    fog: u64,
    texclut: u64,
    scanmsk: u64,
    texa: u64,
    fogcol: u64,
    dimx: u64,
    dthe: u64,
    colclamp: u64,
    pabe: u64,
    bitbltbuf: u64,
    trxpos: u64,
    trxreg: u64,
    /// The vertex queue and how many it holds.
    queue: [Vertex; 3],
    queued: usize,
    /// The CLUT buffer: 512 halfwords; a 32-bit entry keeps its low half at
    /// `i` and its high half at `256 + i`.
    clut: [u16; 512],
    /// A copy of it the primitives drawn since it last changed share.
    clut_shared: Option<std::sync::Arc<[u16; 512]>>,
    /// CBP0 and CBP1, for CLD 4 and 5.
    cbp: [u32; 2],
    /// The host-to-local transfer under way.
    transfer: Option<transfer::Transfer>,
    /// Local-to-host data waiting to be read.
    readback: std::collections::VecDeque<u128>,
    /// GIF tag state, per path.
    paths: [gif::Path; 4],
    /// Events since they were last taken.
    events: Events,
    /// The PCSX2 dump being written, if any.
    dump: Option<dump::Dump>,
    /// Primitives drawn (for reports).
    pub drawn: u64,
    /// Host time spent drawing (for reports).
    pub draw_time: std::time::Duration,
    /// Primitives waiting to be drawn, when drawing on several threads.
    batch: batch::Batch,
}

impl Default for Gs {
    fn default() -> Self {
        Self {
            mem: Memory::default(),
            ctx: [Context::default(); 2],
            prim: 0,
            prmode: 0,
            prmodecont: 1,
            rgbaq: 0,
            st: 0,
            uv: 0,
            fog: 0,
            texclut: 0,
            scanmsk: 0,
            texa: 0,
            fogcol: 0,
            dimx: 0,
            dthe: 0,
            colclamp: 1,
            pabe: 0,
            bitbltbuf: 0,
            trxpos: 0,
            trxreg: 0,
            queue: [Vertex::default(); 3],
            queued: 0,
            clut: [0; 512],
            clut_shared: None,
            cbp: [0; 2],
            transfer: None,
            readback: std::collections::VecDeque::new(),
            paths: Default::default(),
            events: Events::default(),
            dump: None,
            drawn: 0,
            draw_time: std::time::Duration::ZERO,
            batch: batch::Batch::default(),
        }
    }
}

impl Gs {
    /// Writes general-purpose register `address` (as A+D does).
    pub fn write(&mut self, address: u8, value: u64) {
        match address {
            addr::PRIM => {
                self.prim = value & 0x7FF;
                self.queued = 0;
            }
            addr::RGBAQ => self.rgbaq = value,
            addr::ST => self.st = value,
            addr::UV => self.uv = value,
            addr::XYZF2 | addr::XYZF3 => self.vertex(
                bits(value, 0, 16),
                bits(value, 16, 16),
                bits(value, 32, 24),
                Some(bits(value, 56, 8) as u8),
                address == addr::XYZF2,
            ),
            addr::XYZ2 | addr::XYZ3 => self.vertex(
                bits(value, 0, 16),
                bits(value, 16, 16),
                (value >> 32) as u32,
                None,
                address == addr::XYZ2,
            ),
            addr::TEX0_1 | addr::TEX0_2 => {
                let i = usize::from(address - addr::TEX0_1);
                self.ctx[i].tex0 = value;
                self.load_clut(Tex0::new(value));
            }
            addr::TEX2_1 | addr::TEX2_2 => {
                let i = usize::from(address - addr::TEX2_1);
                let tex0 = (self.ctx[i].tex0 & !regs::TEX2_MASK) | (value & regs::TEX2_MASK);
                self.ctx[i].tex0 = tex0;
                self.load_clut(Tex0::new(tex0));
            }
            addr::CLAMP_1 | addr::CLAMP_2 => {
                self.ctx[usize::from(address - addr::CLAMP_1)].clamp = value;
            }
            addr::FOG => self.fog = value,
            addr::TEX1_1 | addr::TEX1_2 => {
                self.ctx[usize::from(address - addr::TEX1_1)].tex1 = value;
            }
            addr::XYOFFSET_1 | addr::XYOFFSET_2 => {
                self.ctx[usize::from(address - addr::XYOFFSET_1)].xyoffset = value;
            }
            addr::PRMODECONT => self.prmodecont = value,
            addr::PRMODE => self.prmode = value,
            addr::TEXCLUT => self.texclut = value,
            addr::SCANMSK => self.scanmsk = value,
            addr::MIPTBP1_1 | addr::MIPTBP1_2 => {
                self.ctx[usize::from(address - addr::MIPTBP1_1)].miptbp1 = value;
            }
            addr::MIPTBP2_1 | addr::MIPTBP2_2 => {
                self.ctx[usize::from(address - addr::MIPTBP2_1)].miptbp2 = value;
            }
            addr::TEXA => self.texa = value,
            addr::FOGCOL => self.fogcol = value,
            addr::SCISSOR_1 | addr::SCISSOR_2 => {
                self.ctx[usize::from(address - addr::SCISSOR_1)].scissor = value;
            }
            addr::ALPHA_1 | addr::ALPHA_2 => {
                self.ctx[usize::from(address - addr::ALPHA_1)].alpha = value;
            }
            addr::DIMX => self.dimx = value,
            addr::DTHE => self.dthe = value,
            addr::COLCLAMP => self.colclamp = value,
            addr::TEST_1 | addr::TEST_2 => {
                self.ctx[usize::from(address - addr::TEST_1)].test = value;
            }
            addr::PABE => self.pabe = value,
            addr::FBA_1 | addr::FBA_2 => self.ctx[usize::from(address - addr::FBA_1)].fba = value,
            addr::FRAME_1 | addr::FRAME_2 => {
                self.ctx[usize::from(address - addr::FRAME_1)].frame = value;
            }
            addr::ZBUF_1 | addr::ZBUF_2 => {
                self.ctx[usize::from(address - addr::ZBUF_1)].zbuf = value;
            }
            addr::BITBLTBUF => self.bitbltbuf = value,
            addr::TRXPOS => self.trxpos = value,
            addr::TRXREG => self.trxreg = value,
            addr::TRXDIR => self.start_transfer(bits(value, 0, 2)),
            addr::HWREG => self.image(&value.to_le_bytes()),
            addr::SIGNAL => self.events.signal = Some(value),
            addr::FINISH => self.events.finish = true,
            addr::LABEL => self.events.label = Some(value),
            // TEXFLUSH; nothing else is a register.
            _ => {}
        }
    }

    /// Takes the events packets have asked for since the last call.
    pub fn take_events(&mut self) -> Events {
        std::mem::take(&mut self.events)
    }

    /// Queues a vertex, drawing when it completes a primitive and `kick`
    /// asks for it.
    fn vertex(&mut self, x: u32, y: u32, z: u32, f: Option<u8>, kick: bool) {
        let q = f32::from_bits((self.rgbaq >> 32) as u32);
        let v = Vertex {
            x,
            y,
            z,
            f: f.unwrap_or(bits(self.fog, 56, 8) as u8),
            rgba: [
                bits(self.rgbaq, 0, 8) as u8,
                bits(self.rgbaq, 8, 8) as u8,
                bits(self.rgbaq, 16, 8) as u8,
                bits(self.rgbaq, 24, 8) as u8,
            ],
            q,
            s: f32::from_bits(self.st as u32),
            t: f32::from_bits((self.st >> 32) as u32),
            u: bits(self.uv, 0, 14),
            v: bits(self.uv, 16, 14),
        };
        let kind = self.prim & 7;
        let needed = match kind {
            0 => 1,
            1 | 2 | 6 => 2,
            3..=5 => 3,
            _ => return,
        };
        self.queue[self.queued] = v;
        self.queued += 1;
        if self.queued < needed {
            return;
        }
        if kick {
            let vertices = self.queue;
            self.draw(&vertices[..needed]);
        }
        self.queued = match kind {
            2 => {
                self.queue[0] = self.queue[1];
                1
            }
            4 => {
                self.queue[0] = self.queue[1];
                self.queue[1] = self.queue[2];
                2
            }
            5 => {
                self.queue[1] = self.queue[2];
                2
            }
            _ => 0,
        };
    }

    /// Loads the CLUT as TEX0's CLD asks.
    fn load_clut(&mut self, t: Tex0) {
        let load = match t.cld {
            1 => true,
            2 => {
                self.cbp[0] = t.cbp;
                true
            }
            3 => {
                self.cbp[1] = t.cbp;
                true
            }
            4 | 5 => {
                let i = (t.cld - 4) as usize;
                let changed = self.cbp[i] != t.cbp;
                self.cbp[i] = t.cbp;
                changed
            }
            _ => false,
        };
        let Some(psm) = Psm::from_bits(u64::from(t.psm)) else {
            return;
        };
        let entries = match psm {
            Psm::T8 | Psm::T8h => 256,
            Psm::T4 | Psm::T4hl | Psm::T4hh => 16,
            _ => return,
        };
        if !load {
            return;
        }
        // Primitives waiting to be drawn may write what the CLUT is read
        // from.
        self.before_clut_load(t);
        self.clut_shared = None;
        let cpsm = match t.cpsm {
            0 | 1 => Psm::Ct32,
            0xA => Psm::Ct16s,
            _ => Psm::Ct16,
        };
        for i in 0..entries {
            let (x, y, bw) = if t.csm {
                // CSM2: a line of 16-bit entries at TEXCLUT's offset.
                let (cbw, cou, cov) = (
                    bits(self.texclut, 0, 6),
                    bits(self.texclut, 6, 6),
                    bits(self.texclut, 12, 10),
                );
                (cou * 16 + i, cov, cbw)
            } else if entries == 256 {
                // CSM1, 8-bit: 16 x 16 with entries 8-15 and 16-23 of each
                // 32 swapped.
                let j = (i & 0xE7) | ((i & 8) << 1) | ((i & 0x10) >> 1);
                (j % 16, j / 16, 1)
            } else {
                (i % 8, i / 8, 1)
            };
            if cpsm == Psm::Ct32 && !t.csm {
                let c = self.mem.read(Psm::Ct32, t.cbp, bw, x, y);
                let at = ((t.csa * 16 + i) & 0xFF) as usize;
                self.clut[at] = c as u16;
                self.clut[256 + at] = (c >> 16) as u16;
            } else {
                let c = self
                    .mem
                    .read(if t.csm { Psm::Ct16 } else { cpsm }, t.cbp, bw, x, y);
                self.clut[((t.csa * 16 + i) & 0x1FF) as usize] = c as u16;
            }
        }
    }
}

/// A CLUT's colour for `index` as RGBA8888 (16-bit entries take alpha
/// from TEXA).
pub(crate) fn clut_color(clut: &[u16; 512], texa: u64, t: &Tex0, index: u32) -> u32 {
    if t.cpsm < 2 {
        let at = ((t.csa * 16 + index) & 0xFF) as usize;
        u32::from(clut[at]) | (u32::from(clut[256 + at]) << 16)
    } else {
        expand16(
            texa,
            u32::from(clut[((t.csa * 16 + index) & 0x1FF) as usize]),
        )
    }
}

/// A 16-bit colour as RGBA8888, alpha from TEXA.
pub(crate) fn expand16(texa: u64, c: u32) -> u32 {
    let rgb = ((c & 0x1F) << 3) | (((c >> 5) & 0x1F) << 11) | (((c >> 10) & 0x1F) << 19);
    #[allow(
        clippy::verbose_bit_mask,
        reason = "the 15 colour bits, as the manual puts it"
    )]
    let a = if c & 0x8000 != 0 {
        bits(texa, 32, 8)
    } else if bits(texa, 15, 1) != 0 && c & 0x7FFF == 0 {
        0
    } else {
        bits(texa, 0, 8)
    };
    rgb | (a << 24)
}

/// A 24-bit colour as RGBA8888, alpha from TEXA.
pub(crate) fn expand24(texa: u64, c: u32) -> u32 {
    let c = c & 0x00FF_FFFF;
    let a = if bits(texa, 15, 1) != 0 && c == 0 {
        0
    } else {
        bits(texa, 0, 8)
    };
    c | (a << 24)
}
