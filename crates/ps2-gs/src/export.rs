//! What the GS draws, handed on for a GPU to draw the same (the GPU GS,
//! `docs/research/gpu-gs.md`).
//!
//! The GS hands its drawer thread batches of primitives and the pixels of
//! transfers, in order. A [`Tap`] set with [`crate::Gs::set_tap`] lives on
//! that thread and is handed the same, in the same order: each batch as
//! primitives encoded in [`PRIM_WORDS`] words (the [`word`] and [`flag`]
//! constants), each transfer's pixels as writes to words of local memory,
//! and the CLUT loads before them ([`Clut`]); with the CPU no longer
//! drawing, it is also asked for the display's picture and for local
//! memory. A batch's primitives may be drawn at
//! once, each pixel's in order: no primitive in it reads what another
//! writes elsewhere. A primitive drawn alone that textures from blocks it
//! writes reads its texture as local memory was before it
//! ([`flag::SNAPSHOT`]).
//!
//! Points, lines, antialiased edges and sprites with STQ textures are
//! walked on the CPU, as the software GS walks them (in `f32`), and handed
//! on as fragments for the GPU to shade.
//!
//! Values are as the software GS computes them before its per-pixel work:
//! positions less XYOFFSET in 1/16 pixel, a triangle's vertices in
//! clockwise order with its edges' top-left ownership, texture
//! coordinates in the shared integer arithmetic (`fixed`).

/// Words a primitive takes.
pub const PRIM_WORDS: usize = 72;

/// Where each field of an encoded primitive lies (word indices).
pub mod word {
    /// PRIM's kind (0 point .. 7) in bits 0..3, [`super::flag`]s above.
    pub const KIND: usize = 0;
    /// The pixels it may touch, inclusive: `x0 | x1 << 16`.
    pub const BBOX_X: usize = 1;
    /// `y0 | y1 << 16`.
    pub const BBOX_Y: usize = 2;
    /// The frame's format (PSM code).
    pub const FRAME_PSM: usize = 3;
    /// FBMSK.
    pub const FBMSK: usize = 4;
    /// The depth buffer's format (PSM code), when the depth test is on.
    pub const ZBUF_PSM: usize = 5;
    /// TEST's low 32 bits.
    pub const TEST: usize = 6;
    /// ALPHA: A, B, C, D in bits 0..8, FIX in 8..16.
    pub const ALPHA: usize = 7;
    /// TEXA: TA0 in bits 0..8, AEM in bit 8, TA1 in 16..24.
    pub const TEXA: usize = 8;
    /// FOGCOL.
    pub const FOGCOL: usize = 9;
    /// The texture's format (PSM code), when it has one.
    pub const TEX_PSM: usize = 10;
    /// TFX in bits 0..2, CPSM in 4..8, CSA in 8..13.
    pub const TEX_FUNC: usize = 11;
    /// CLAMP's modes: U in bits 0..2, V in 2..4.
    pub const WRAP: usize = 12;
    /// U's MIN and MAX (`min | max << 16`).
    pub const WRAP_U: usize = 13;
    /// V's MIN and MAX.
    pub const WRAP_V: usize = 14;
    /// The texture's width and height in texels (`w | h << 16`).
    pub const TEX_SIZE: usize = 15;
    /// The CLUT it reads: a slot of [`super::CLUT_SLOTS`] (its version
    /// modulo their number, see [`super::Clut`]).
    pub const CLUT: usize = 16;
    /// SCANMSK.
    pub const SCANMSK: usize = 17;
    /// For STQ triangles, the shifts that make U and V (see `fixed`).
    pub const SHIFT_U: usize = 18;
    /// See [`SHIFT_U`].
    pub const SHIFT_V: usize = 19;
    /// The frame's layout ([`crate::Layout::gpu`], seven words).
    pub const FRAME_LAYOUT: usize = 20;
    /// The depth buffer's layout.
    pub const DEPTH_LAYOUT: usize = 27;
    /// The texture's layout.
    pub const TEX_LAYOUT: usize = 34;
    /// Positions, x then y for each vertex (`i32`, 1/16 pixel). A sprite's
    /// are its corners, left-top then right-bottom.
    pub const POS: usize = 41;
    /// Depth at each vertex (a sprite's in the first).
    pub const Z: usize = 47;
    /// Colour at each vertex, RGBA bytes.
    pub const RGBA: usize = 50;
    /// Fog at each vertex.
    pub const FOG: usize = 53;
    /// U at each vertex in 1/16 texel (UV), or S scaled (STQ). A sprite's
    /// are at its left and right corners.
    pub const U: usize = 56;
    /// V likewise, or T scaled; a sprite's at its top and bottom corners.
    pub const V: usize = 59;
    /// Q scaled (STQ).
    pub const Q: usize = 62;
    /// The flat colour (the last vertex's, as given).
    pub const FLAT_RGBA: usize = 65;
    /// Its fragments: the first, in [`super::Batch::frags`] (in
    /// fragments), and how many.
    pub const FRAGS: usize = 66;
    /// See [`FRAGS`].
    pub const FRAG_COUNT: usize = 67;
    /// The scissor, inclusive: `x0 | x1 << 16`.
    pub const SCISSOR_X: usize = 68;
    /// `y0 | y1 << 16`.
    pub const SCISSOR_Y: usize = 69;
}

/// Words a fragment takes in [`Batch::frags`]: `x | y << 16`, depth,
/// colour (RGBA bytes), texture coordinates U and V in 1/16 texel (`i32`),
/// fog, and coverage (0 to 0xFFFF on an antialiased edge, else
/// `u32::MAX`), then a word unused.
pub const FRAG_WORDS: usize = 8;

/// Flags in [`word::KIND`].
pub mod flag {
    /// TME.
    pub const TME: u32 = 1 << 8;
    /// FGE.
    pub const FGE: u32 = 1 << 9;
    /// ABE.
    pub const ABE: u32 = 1 << 10;
    /// FST.
    pub const FST: u32 = 1 << 11;
    /// IIP.
    pub const IIP: u32 = 1 << 12;
    /// ZBUF's ZMSK.
    pub const ZMSK: u32 = 1 << 13;
    /// FBA.
    pub const FBA: u32 = 1 << 14;
    /// COLCLAMP.
    pub const COLCLAMP: u32 = 1 << 15;
    /// PABE.
    pub const PABE: u32 = 1 << 16;
    /// Bilinear filtering.
    pub const LINEAR: u32 = 1 << 17;
    /// TEX0's TCC.
    pub const TCC: u32 = 1 << 18;
    /// The depth test is on (ZTE): the depth buffer is in use.
    pub const ZBUF: u32 = 1 << 19;
    /// The texture has a format (and a layout).
    pub const TEX: u32 = 1 << 20;
    /// The texture is read as local memory was before the primitive.
    pub const SNAPSHOT: u32 = 1 << 21;
    /// A triangle's edges own the samples on them (top-left rule), one bit
    /// per edge: opposite vertex 0, 1, 2.
    pub const OWNED: u32 = 1 << 22;
    /// A triangle's three depths are the same.
    pub const FLAT_Z: u32 = 1 << 25;
    /// A triangle's texture coordinates are STQ (else UV).
    pub const STQ: u32 = 1 << 26;
    /// Antialiasing (AA1): coverage alpha, blending.
    pub const AA1: u32 = 1 << 27;
    /// Drawn from its fragments alone, walked on the CPU (points, lines,
    /// sprites with STQ textures); a triangle's fragments, if any, are its
    /// antialiased edges, drawn after its inside.
    pub const LISTED: u32 = 1 << 28;
}

/// Primitives that may be drawn at once, each pixel's in order.
#[derive(Default, Debug)]
pub struct Batch {
    /// The primitives, [`PRIM_WORDS`] words each.
    pub prims: Vec<u32>,
    /// Fragments walked on the CPU, [`FRAG_WORDS`] words each, each
    /// primitive's in order.
    pub frags: Vec<u32>,
}

/// A write to local memory: the word, the bits written, their new value.
pub type Write = [u32; 3];

/// CLUTs the GPU keeps, each 256 words (halfwords `i` and `256 + i` of the
/// CLUT buffer as word `i`'s low and high halves). The CLUT has a version,
/// counted up by every load; version `v` lives in slot `v % CLUT_SLOTS`.
/// The GS draws a batch before more loads than that could overwrite the
/// CLUTs its primitives read.
pub const CLUT_SLOTS: u32 = 1024;

/// The CLUT changing, in order with the rest.
#[derive(Clone, Debug)]
pub enum Clut {
    /// The whole CLUT buffer, as version `version` (when the tap is set).
    Set {
        /// Its version.
        version: u32,
        /// The 512 halfwords.
        halves: Box<[u16; 512]>,
    },
    /// A load from local memory making version `version` from the one
    /// before: version, entries (16 or 256), CSA, flags (bit 0: 32-bit
    /// entries; bit 1: CSM2), COU and COV for CSM2, two words unused, the
    /// layout read ([`crate::Layout::gpu`]: CT32, CT16 or CT16S at CBP,
    /// TEXCLUT's width for CSM2 else 1), a word unused. Entry `j` is read
    /// at (COU·16 + j, COV) for CSM2; for CSM1 with 256 entries at (k % 16,
    /// k / 16) with `k` = `j` with bits 3 and 4 swapped; with 16 at (j % 8,
    /// j / 8). It lands at CLUT halfword `(CSA·16 + j) & 0x1FF`, or for
    /// 32-bit entries its halves at `(CSA·16 + j) & 0xFF` and 256 more.
    Load([u32; 16]),
}

/// What is handed the GS's work for a GPU, on the GS's drawer thread.
pub trait Tap: Send {
    /// Local memory as it is now: when the tap is set, and after a change
    /// the GS makes otherwise (a local-to-local copy).
    fn memory(&mut self, words: &[u32]);
    /// A transfer's pixels, as writes (each word at most once).
    fn writes(&mut self, writes: &[Write]);
    /// CLUT changes, in order.
    fn cluts(&mut self, cluts: &[Clut]);
    /// A batch of primitives.
    fn batch(&mut self, batch: &Batch);
    /// The display's picture ([`crate::display::gpu_display`] gives its
    /// size and parameters).
    fn display(&mut self, regs: &crate::Privileged) -> crate::Frame;
    /// Local memory as it is after everything handed over.
    fn read(&mut self) -> Vec<u32>;
}
