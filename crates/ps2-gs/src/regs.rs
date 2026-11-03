//! GS register addresses and their fields.

/// General-purpose register addresses (what A+D writes name).
pub mod addr {
    #![allow(missing_docs, reason = "the registers' own names")]
    pub const PRIM: u8 = 0x00;
    pub const RGBAQ: u8 = 0x01;
    pub const ST: u8 = 0x02;
    pub const UV: u8 = 0x03;
    pub const XYZF2: u8 = 0x04;
    pub const XYZ2: u8 = 0x05;
    pub const TEX0_1: u8 = 0x06;
    pub const TEX0_2: u8 = 0x07;
    pub const CLAMP_1: u8 = 0x08;
    pub const CLAMP_2: u8 = 0x09;
    pub const FOG: u8 = 0x0A;
    pub const XYZF3: u8 = 0x0C;
    pub const XYZ3: u8 = 0x0D;
    pub const TEX1_1: u8 = 0x14;
    pub const TEX1_2: u8 = 0x15;
    pub const TEX2_1: u8 = 0x16;
    pub const TEX2_2: u8 = 0x17;
    pub const XYOFFSET_1: u8 = 0x18;
    pub const XYOFFSET_2: u8 = 0x19;
    pub const PRMODECONT: u8 = 0x1A;
    pub const PRMODE: u8 = 0x1B;
    pub const TEXCLUT: u8 = 0x1C;
    pub const SCANMSK: u8 = 0x22;
    pub const MIPTBP1_1: u8 = 0x34;
    pub const MIPTBP1_2: u8 = 0x35;
    pub const MIPTBP2_1: u8 = 0x36;
    pub const MIPTBP2_2: u8 = 0x37;
    pub const TEXA: u8 = 0x3B;
    pub const FOGCOL: u8 = 0x3D;
    pub const TEXFLUSH: u8 = 0x3F;
    pub const SCISSOR_1: u8 = 0x40;
    pub const SCISSOR_2: u8 = 0x41;
    pub const ALPHA_1: u8 = 0x42;
    pub const ALPHA_2: u8 = 0x43;
    pub const DIMX: u8 = 0x44;
    pub const DTHE: u8 = 0x45;
    pub const COLCLAMP: u8 = 0x46;
    pub const TEST_1: u8 = 0x47;
    pub const TEST_2: u8 = 0x48;
    pub const PABE: u8 = 0x49;
    pub const FBA_1: u8 = 0x4A;
    pub const FBA_2: u8 = 0x4B;
    pub const FRAME_1: u8 = 0x4C;
    pub const FRAME_2: u8 = 0x4D;
    pub const ZBUF_1: u8 = 0x4E;
    pub const ZBUF_2: u8 = 0x4F;
    pub const BITBLTBUF: u8 = 0x50;
    pub const TRXPOS: u8 = 0x51;
    pub const TRXREG: u8 = 0x52;
    pub const TRXDIR: u8 = 0x53;
    pub const HWREG: u8 = 0x54;
    pub const SIGNAL: u8 = 0x60;
    pub const FINISH: u8 = 0x61;
    pub const LABEL: u8 = 0x62;
}

/// `n` bits of `v` from bit `lo`.
#[must_use]
pub const fn bits(v: u64, lo: u32, n: u32) -> u32 {
    ((v >> lo) & ((1 << n) - 1)) as u32
}

/// The registers of one drawing context.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// TEX0.
    pub tex0: u64,
    /// TEX1.
    pub tex1: u64,
    /// CLAMP.
    pub clamp: u64,
    /// XYOFFSET.
    pub xyoffset: u64,
    /// MIPTBP1.
    pub miptbp1: u64,
    /// MIPTBP2.
    pub miptbp2: u64,
    /// SCISSOR.
    pub scissor: u64,
    /// ALPHA.
    pub alpha: u64,
    /// TEST.
    pub test: u64,
    /// FBA.
    pub fba: u64,
    /// FRAME.
    pub frame: u64,
    /// ZBUF.
    pub zbuf: u64,
}

/// TEX0's fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tex0 {
    /// Texture base, in blocks.
    pub tbp: u32,
    /// Texture width, in 64-pixel units.
    pub tbw: u32,
    /// Texture format (raw).
    pub psm: u32,
    /// Width and height as powers of two.
    pub tw: u32,
    /// Height.
    pub th: u32,
    /// Texture colour component: use the texture's alpha.
    pub tcc: bool,
    /// Texture function.
    pub tfx: u32,
    /// CLUT base, in blocks.
    pub cbp: u32,
    /// CLUT format (raw).
    pub cpsm: u32,
    /// CLUT storage mode (1 = CSM2).
    pub csm: bool,
    /// CLUT entry offset, in 16-entry units.
    pub csa: u32,
    /// CLUT load control.
    pub cld: u32,
}

impl Tex0 {
    /// Decodes TEX0.
    #[must_use]
    pub const fn new(v: u64) -> Self {
        Self {
            tbp: bits(v, 0, 14),
            tbw: bits(v, 14, 6),
            psm: bits(v, 20, 6),
            tw: bits(v, 26, 4),
            th: bits(v, 30, 4),
            tcc: bits(v, 34, 1) != 0,
            tfx: bits(v, 35, 2),
            cbp: bits(v, 37, 14),
            cpsm: bits(v, 51, 4),
            csm: bits(v, 55, 1) != 0,
            csa: bits(v, 56, 5),
            cld: bits(v, 61, 3),
        }
    }
}

/// The bits of TEX0 that TEX2 sets: PSM and the CLUT fields.
pub const TEX2_MASK: u64 = (0x3F << 20) | !((1 << 37) - 1);
