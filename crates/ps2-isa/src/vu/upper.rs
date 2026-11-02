//! Upper instructions: the FMAC pipeline (bits 63..32 of a pair).

use core::fmt::{self, Write};

use crate::regs::VU_COMPONENTS;

macro_rules! upper_ops {
    ($( $variant:ident = $mnemonic:literal, $format:ident; )*) => {
        /// An upper-pipeline operation. [`UpperOp::Invalid`] for unknown
        /// opcodes.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(u8)]
        pub enum UpperOp {
            /// Not an instruction.
            Invalid,
            $(
                #[doc = concat!("`", $mnemonic, "`")]
                $variant,
            )*
        }

        impl UpperOp {
            /// Every operation except [`UpperOp::Invalid`].
            pub const ALL: &'static [UpperOp] = &[$(UpperOp::$variant,)*];

            /// The assembler mnemonic.
            #[must_use]
            pub const fn mnemonic(self) -> &'static str {
                match self {
                    UpperOp::Invalid => "(invalid)",
                    $(UpperOp::$variant => $mnemonic,)*
                }
            }

            pub(crate) const fn format(self) -> Format {
                match self {
                    UpperOp::Invalid => Format::None,
                    $(UpperOp::$variant => Format::$format,)*
                }
            }
        }
    };
}

upper_ops! {
    Addx = "addx", FdFsFtBc; Addy = "addy", FdFsFtBc; Addz = "addz", FdFsFtBc; Addw = "addw", FdFsFtBc;
    Subx = "subx", FdFsFtBc; Suby = "suby", FdFsFtBc; Subz = "subz", FdFsFtBc; Subw = "subw", FdFsFtBc;
    Maddx = "maddx", FdFsFtBc; Maddy = "maddy", FdFsFtBc; Maddz = "maddz", FdFsFtBc; Maddw = "maddw", FdFsFtBc;
    Msubx = "msubx", FdFsFtBc; Msuby = "msuby", FdFsFtBc; Msubz = "msubz", FdFsFtBc; Msubw = "msubw", FdFsFtBc;
    Maxx = "maxx", FdFsFtBc; Maxy = "maxy", FdFsFtBc; Maxz = "maxz", FdFsFtBc; Maxw = "maxw", FdFsFtBc;
    Minix = "minix", FdFsFtBc; Miniy = "miniy", FdFsFtBc; Miniz = "miniz", FdFsFtBc; Miniw = "miniw", FdFsFtBc;
    Mulx = "mulx", FdFsFtBc; Muly = "muly", FdFsFtBc; Mulz = "mulz", FdFsFtBc; Mulw = "mulw", FdFsFtBc;
    Mulq = "mulq", FdFsQ; Maxi = "maxi", FdFsI; Muli = "muli", FdFsI; Minii = "minii", FdFsI;
    Addq = "addq", FdFsQ; Maddq = "maddq", FdFsQ; Addi = "addi", FdFsI; Maddi = "maddi", FdFsI;
    Subq = "subq", FdFsQ; Msubq = "msubq", FdFsQ; Subi = "subi", FdFsI; Msubi = "msubi", FdFsI;
    Add = "add", FdFsFt; Madd = "madd", FdFsFt; Mul = "mul", FdFsFt; Max = "max", FdFsFt;
    Sub = "sub", FdFsFt; Msub = "msub", FdFsFt; Opmsub = "opmsub", FdFsFt; Mini = "mini", FdFsFt;
    Addax = "addax", AccFsFtBc; Adday = "adday", AccFsFtBc; Addaz = "addaz", AccFsFtBc; Addaw = "addaw", AccFsFtBc;
    Subax = "subax", AccFsFtBc; Subay = "subay", AccFsFtBc; Subaz = "subaz", AccFsFtBc; Subaw = "subaw", AccFsFtBc;
    Maddax = "maddax", AccFsFtBc; Madday = "madday", AccFsFtBc; Maddaz = "maddaz", AccFsFtBc; Maddaw = "maddaw", AccFsFtBc;
    Msubax = "msubax", AccFsFtBc; Msubay = "msubay", AccFsFtBc; Msubaz = "msubaz", AccFsFtBc; Msubaw = "msubaw", AccFsFtBc;
    Itof0 = "itof0", FtFs; Itof4 = "itof4", FtFs; Itof12 = "itof12", FtFs; Itof15 = "itof15", FtFs;
    Ftoi0 = "ftoi0", FtFs; Ftoi4 = "ftoi4", FtFs; Ftoi12 = "ftoi12", FtFs; Ftoi15 = "ftoi15", FtFs;
    Mulax = "mulax", AccFsFtBc; Mulay = "mulay", AccFsFtBc; Mulaz = "mulaz", AccFsFtBc; Mulaw = "mulaw", AccFsFtBc;
    Mulaq = "mulaq", AccFsQ; Abs = "abs", FtFs; Mulai = "mulai", AccFsI; Clipw = "clipw", Clip;
    Addaq = "addaq", AccFsQ; Maddaq = "maddaq", AccFsQ; Addai = "addai", AccFsI; Maddai = "maddai", AccFsI;
    Subaq = "subaq", AccFsQ; Msubaq = "msubaq", AccFsQ; Subai = "subai", AccFsI; Msubai = "msubai", AccFsI;
    Adda = "adda", AccFsFt; Madda = "madda", AccFsFt; Mula = "mula", AccFsFt;
    Suba = "suba", AccFsFt; Msuba = "msuba", AccFsFt; Opmula = "opmula", AccFsFt; Nop = "nop", None;
}

/// Operand formats of upper instructions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    None,
    FdFsFt,
    FdFsFtBc,
    FdFsQ,
    FdFsI,
    AccFsFt,
    AccFsFtBc,
    AccFsQ,
    AccFsI,
    FtFs,
    Clip,
}

const DEST: u32 = 0x01E0_0000;
const FT: u32 = 0x001F_0000;
const FS: u32 = 0x0000_F800;
const FD: u32 = 0x0000_07C0;
/// The I, E, M, D and T flag bits.
pub(crate) const FLAGS: u32 = 0xF800_0000;

impl Format {
    const fn operand_mask(self) -> u32 {
        match self {
            Self::None => 0,
            Self::FdFsFt | Self::FdFsFtBc => DEST | FT | FS | FD,
            Self::FdFsQ | Self::FdFsI => DEST | FS | FD,
            Self::AccFsFt | Self::AccFsFtBc | Self::FtFs | Self::Clip => DEST | FT | FS,
            Self::AccFsQ | Self::AccFsI => DEST | FS,
        }
    }
}

/// Decodes the operation of an upper word.
pub(crate) const fn decode(w: u32) -> UpperOp {
    use UpperOp as U;
    const BC: [[UpperOp; 4]; 7] = [
        [U::Addx, U::Addy, U::Addz, U::Addw],
        [U::Subx, U::Suby, U::Subz, U::Subw],
        [U::Maddx, U::Maddy, U::Maddz, U::Maddw],
        [U::Msubx, U::Msuby, U::Msubz, U::Msubw],
        [U::Maxx, U::Maxy, U::Maxz, U::Maxw],
        [U::Minix, U::Miniy, U::Miniz, U::Miniw],
        [U::Mulx, U::Muly, U::Mulz, U::Mulw],
    ];
    const ACC_BC: [[UpperOp; 4]; 5] = [
        [U::Addax, U::Adday, U::Addaz, U::Addaw],
        [U::Subax, U::Subay, U::Subaz, U::Subaw],
        [U::Maddax, U::Madday, U::Maddaz, U::Maddaw],
        [U::Msubax, U::Msubay, U::Msubaz, U::Msubaw],
        [U::Mulax, U::Mulay, U::Mulaz, U::Mulaw],
    ];
    let funct = w & 0x3F;
    match funct {
        0x00..=0x1B => BC[(funct >> 2) as usize][(funct & 3) as usize],
        0x1C => U::Mulq,
        0x1D => U::Maxi,
        0x1E => U::Muli,
        0x1F => U::Minii,
        0x20 => U::Addq,
        0x21 => U::Maddq,
        0x22 => U::Addi,
        0x23 => U::Maddi,
        0x24 => U::Subq,
        0x25 => U::Msubq,
        0x26 => U::Subi,
        0x27 => U::Msubi,
        0x28 => U::Add,
        0x29 => U::Madd,
        0x2A => U::Mul,
        0x2B => U::Max,
        0x2C => U::Sub,
        0x2D => U::Msub,
        0x2E => U::Opmsub,
        0x2F => U::Mini,
        0x3C..=0x3F => {
            let index = (((w >> 6) & 0x1F) << 2) | (w & 3);
            match index {
                0x00..=0x0F => ACC_BC[(index >> 2) as usize][(index & 3) as usize],
                0x10 => U::Itof0,
                0x11 => U::Itof4,
                0x12 => U::Itof12,
                0x13 => U::Itof15,
                0x14 => U::Ftoi0,
                0x15 => U::Ftoi4,
                0x16 => U::Ftoi12,
                0x17 => U::Ftoi15,
                0x18..=0x1B => ACC_BC[4][(index & 3) as usize],
                0x1C => U::Mulaq,
                0x1D => U::Abs,
                0x1E => U::Mulai,
                0x1F => U::Clipw,
                0x20 => U::Addaq,
                0x21 => U::Maddaq,
                0x22 => U::Addai,
                0x23 => U::Maddai,
                0x24 => U::Subaq,
                0x25 => U::Msubaq,
                0x26 => U::Subai,
                0x27 => U::Msubai,
                0x28 => U::Adda,
                0x29 => U::Madda,
                0x2A => U::Mula,
                0x2C => U::Suba,
                0x2D => U::Msuba,
                0x2E => U::Opmula,
                0x2F => U::Nop,
                _ => U::Invalid,
            }
        }
        _ => U::Invalid,
    }
}

/// Bits a valid upper word may set besides its operands: the flags and the
/// opcode (bits 5..0, plus 10..6 for the special table).
pub(crate) const fn fixed_mask(op: UpperOp, w: u32) -> u32 {
    let opcode = if w & 0x3C == 0x3C { 0x7FF } else { 0x3F };
    FLAGS | opcode | op.format().operand_mask()
}

/// Writes an upper instruction (without flags).
pub(crate) fn write(f: &mut fmt::Formatter<'_>, op: UpperOp, w: u32) -> fmt::Result {
    let format = op.format();
    f.write_str(op.mnemonic())?;
    if format != Format::None {
        f.write_char('.')?;
        write_dest(f, w)?;
    }
    let (ft, fs, fd) = ((w >> 16) & 0x1F, (w >> 11) & 0x1F, (w >> 6) & 0x1F);
    let bc = VU_COMPONENTS[(w & 3) as usize];
    match format {
        Format::None => Ok(()),
        Format::FdFsFt => write!(f, " $vf{fd}, $vf{fs}, $vf{ft}"),
        Format::FdFsFtBc => write!(f, " $vf{fd}, $vf{fs}, $vf{ft}{bc}"),
        Format::FdFsQ => write!(f, " $vf{fd}, $vf{fs}, Q"),
        Format::FdFsI => write!(f, " $vf{fd}, $vf{fs}, I"),
        Format::AccFsFt => write!(f, " ACC, $vf{fs}, $vf{ft}"),
        Format::AccFsFtBc => write!(f, " ACC, $vf{fs}, $vf{ft}{bc}"),
        Format::AccFsQ => write!(f, " ACC, $vf{fs}, Q"),
        Format::AccFsI => write!(f, " ACC, $vf{fs}, I"),
        Format::FtFs => write!(f, " $vf{ft}, $vf{fs}"),
        Format::Clip => write!(f, " $vf{fs}, $vf{ft}w"),
    }
}

/// Writes the `xyzw` components of the `dest` field (bits 24..21).
pub(crate) fn write_dest(f: &mut fmt::Formatter<'_>, w: u32) -> fmt::Result {
    for (bit, name) in (21..25).rev().zip(VU_COMPONENTS) {
        if w & (1 << bit) != 0 {
            f.write_char(name)?;
        }
    }
    Ok(())
}
