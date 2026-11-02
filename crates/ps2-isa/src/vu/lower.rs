//! Lower instructions: integer, load/store, branch, divide, random and EFU
//! operations (bits 31..0 of a pair).

use core::fmt;

use super::upper::write_dest;
use crate::regs::VU_COMPONENTS;

macro_rules! lower_ops {
    ($( $variant:ident = $mnemonic:literal, $format:ident; )*) => {
        /// A lower-pipeline operation. [`LowerOp::Invalid`] for unknown
        /// opcodes; [`LowerOp::Loi`] when the pair's I flag makes the lower
        /// word a float immediate.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(u8)]
        pub enum LowerOp {
            /// Not an instruction.
            Invalid,
            $(
                #[doc = concat!("`", $mnemonic, "`")]
                $variant,
            )*
        }

        impl LowerOp {
            /// Every operation except [`LowerOp::Invalid`].
            pub const ALL: &'static [LowerOp] = &[$(LowerOp::$variant,)*];

            /// The assembler mnemonic.
            #[must_use]
            pub const fn mnemonic(self) -> &'static str {
                match self {
                    LowerOp::Invalid => "(invalid)",
                    $(LowerOp::$variant => $mnemonic,)*
                }
            }

            pub(crate) const fn format(self) -> Format {
                match self {
                    LowerOp::Invalid => Format::None,
                    $(LowerOp::$variant => Format::$format,)*
                }
            }
        }
    };
}

lower_ops! {
    Loi = "loi", Imm32;
    // Type 2: opcode in bits 31..25.
    Lq = "lq", LqFtIs; Sq = "sq", SqFsIt; Ilw = "ilw", IlwItIs; Isw = "isw", IlwItIs;
    Iaddiu = "iaddiu", ItIsImm15; Isubiu = "isubiu", ItIsImm15;
    Fceq = "fceq", Vi1Imm24; Fcset = "fcset", Imm24; Fcand = "fcand", Vi1Imm24; Fcor = "fcor", Vi1Imm24;
    Fseq = "fseq", ItImm12; Fsset = "fsset", Imm12; Fsand = "fsand", ItImm12; Fsor = "fsor", ItImm12;
    Fmeq = "fmeq", ItIs; Fmand = "fmand", ItIs; Fmor = "fmor", ItIs; Fcget = "fcget", It;
    B = "b", Branch; Bal = "bal", ItBranch; Jr = "jr", Is; Jalr = "jalr", ItIs;
    Ibeq = "ibeq", ItIsBranch; Ibne = "ibne", ItIsBranch;
    Ibltz = "ibltz", IsBranch; Ibgtz = "ibgtz", IsBranch; Iblez = "iblez", IsBranch; Ibgez = "ibgez", IsBranch;
    // Type 1: bit 31 set, function in bits 5..0.
    Iadd = "iadd", IdIsIt; Isub = "isub", IdIsIt; Iaddi = "iaddi", ItIsImm5; Iand = "iand", IdIsIt; Ior = "ior", IdIsIt;
    // Type 3: the special table, indexed by bits 10..6 and 1..0.
    Move = "move", FtFs; Mr32 = "mr32", FtFs;
    Lqi = "lqi", Lqi; Sqi = "sqi", Sqi; Lqd = "lqd", Lqd; Sqd = "sqd", Sqd;
    Div = "div", Div; Sqrt = "sqrt", Sqrt; Rsqrt = "rsqrt", Div; Waitq = "waitq", None;
    Mtir = "mtir", Mtir; Mfir = "mfir", Mfir; Ilwr = "ilwr", Ilwr; Iswr = "iswr", Ilwr;
    Rnext = "rnext", FtR; Rget = "rget", FtR; Rinit = "rinit", RFsf; Rxor = "rxor", RFsf;
    Mfp = "mfp", FtP; Xtop = "xtop", It; Xitop = "xitop", It; Xgkick = "xgkick", Is;
    Esadd = "esadd", PFs; Ersadd = "ersadd", PFs; Eleng = "eleng", PFs; Erleng = "erleng", PFs;
    Eatanxy = "eatanxy", PFs; Eatanxz = "eatanxz", PFs; Esum = "esum", PFs;
    Esqrt = "esqrt", PFsf; Ersqrt = "ersqrt", PFsf; Ercpr = "ercpr", PFsf; Waitp = "waitp", None;
    Esin = "esin", PFsf; Eatan = "eatan", PFsf; Eexp = "eexp", PFsf;
}

/// Operand formats of lower instructions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    None,
    Imm32,
    LqFtIs,
    SqFsIt,
    IlwItIs,
    ItIsImm15,
    Vi1Imm24,
    Imm24,
    ItImm12,
    Imm12,
    ItIs,
    It,
    Is,
    Branch,
    ItBranch,
    ItIsBranch,
    IsBranch,
    IdIsIt,
    ItIsImm5,
    FtFs,
    Lqi,
    Sqi,
    Lqd,
    Sqd,
    Div,
    Sqrt,
    Mtir,
    Mfir,
    Ilwr,
    FtR,
    RFsf,
    FtP,
    PFs,
    PFsf,
}

const DEST: u32 = 0x01E0_0000;
const FTF: u32 = 0x0180_0000;
const FSF: u32 = 0x0060_0000;
const FT: u32 = 0x001F_0000;
const FS: u32 = 0x0000_F800;
const FD: u32 = 0x0000_07C0;
const IMM11: u32 = 0x0000_07FF;

impl Format {
    /// Bits carrying operands.
    const fn operand_mask(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Imm32 => 0xFFFF_FFFF,
            // iaddiu/isubiu spread their 15-bit immediate over dest and imm11.
            Self::LqFtIs | Self::SqFsIt | Self::IlwItIs | Self::ItIsImm15 => DEST | FT | FS | IMM11,
            Self::Vi1Imm24 | Self::Imm24 => 0x00FF_FFFF,
            Self::ItImm12 => 0x0020_0000 | FT | IMM11,
            Self::Imm12 => 0x0020_0000 | IMM11,
            Self::ItIs => FT | FS,
            Self::It => FT,
            Self::Is => FS,
            Self::Branch => IMM11,
            Self::ItBranch => FT | IMM11,
            Self::ItIsBranch => FT | FS | IMM11,
            Self::IsBranch => FS | IMM11,
            Self::IdIsIt | Self::ItIsImm5 => FT | FS | FD,
            Self::FtFs
            | Self::Lqi
            | Self::Sqi
            | Self::Lqd
            | Self::Sqd
            | Self::Mfir
            | Self::Ilwr => DEST | FT | FS,
            Self::Div => FTF | FSF | FT | FS,
            Self::Sqrt => FTF | FT,
            Self::Mtir => FSF | FT | FS,
            Self::FtR | Self::FtP => DEST | FT,
            Self::RFsf | Self::PFsf => FSF | FS,
            // esadd, eleng, esum and the like encode the components they read
            // (xyz, xyzw, xy, xz) in the dest field.
            Self::PFs => DEST | FS,
        }
    }

    const fn has_dest(self) -> bool {
        matches!(
            self,
            Self::LqFtIs
                | Self::SqFsIt
                | Self::IlwItIs
                | Self::FtFs
                | Self::Lqi
                | Self::Sqi
                | Self::Lqd
                | Self::Sqd
                | Self::Mfir
                | Self::Ilwr
                | Self::FtR
                | Self::FtP
        )
    }
}

/// Decodes the operation of a lower word (without the I flag).
pub(crate) const fn decode(w: u32) -> LowerOp {
    use LowerOp as L;
    match w >> 25 {
        0x00 => L::Lq,
        0x01 => L::Sq,
        0x04 => L::Ilw,
        0x05 => L::Isw,
        0x08 => L::Iaddiu,
        0x09 => L::Isubiu,
        0x10 => L::Fceq,
        0x11 => L::Fcset,
        0x12 => L::Fcand,
        0x13 => L::Fcor,
        0x14 => L::Fseq,
        0x15 => L::Fsset,
        0x16 => L::Fsand,
        0x17 => L::Fsor,
        0x18 => L::Fmeq,
        0x1A => L::Fmand,
        0x1B => L::Fmor,
        0x1C => L::Fcget,
        0x20 => L::B,
        0x21 => L::Bal,
        0x24 => L::Jr,
        0x25 => L::Jalr,
        0x28 => L::Ibeq,
        0x29 => L::Ibne,
        0x2C => L::Ibltz,
        0x2D => L::Ibgtz,
        0x2E => L::Iblez,
        0x2F => L::Ibgez,
        0x40 => match w & 0x3F {
            0x30 => L::Iadd,
            0x31 => L::Isub,
            0x32 => L::Iaddi,
            0x34 => L::Iand,
            0x35 => L::Ior,
            0x3C..=0x3F => special(w),
            _ => L::Invalid,
        },
        _ => L::Invalid,
    }
}

const fn special(w: u32) -> LowerOp {
    use LowerOp as L;
    match (((w >> 6) & 0x1F) << 2) | (w & 3) {
        0x30 => L::Move,
        0x31 => L::Mr32,
        0x34 => L::Lqi,
        0x35 => L::Sqi,
        0x36 => L::Lqd,
        0x37 => L::Sqd,
        0x38 => L::Div,
        0x39 => L::Sqrt,
        0x3A => L::Rsqrt,
        0x3B => L::Waitq,
        0x3C => L::Mtir,
        0x3D => L::Mfir,
        0x3E => L::Ilwr,
        0x3F => L::Iswr,
        0x40 => L::Rnext,
        0x41 => L::Rget,
        0x42 => L::Rinit,
        0x43 => L::Rxor,
        0x64 => L::Mfp,
        0x68 => L::Xtop,
        0x69 => L::Xitop,
        0x6C => L::Xgkick,
        0x70 => L::Esadd,
        0x71 => L::Ersadd,
        0x72 => L::Eleng,
        0x73 => L::Erleng,
        0x74 => L::Eatanxy,
        0x75 => L::Eatanxz,
        0x76 => L::Esum,
        0x78 => L::Esqrt,
        0x79 => L::Ersqrt,
        0x7A => L::Ercpr,
        0x7B => L::Waitp,
        0x7C => L::Esin,
        0x7D => L::Eatan,
        0x7E => L::Eexp,
        _ => L::Invalid,
    }
}

/// Bits a valid lower word may set: its opcode bits and operands.
pub(crate) const fn allowed_mask(op: LowerOp, w: u32) -> u32 {
    let opcode = if w >> 31 == 0 {
        0xFE00_0000
    } else if w & 0x3C == 0x3C {
        0xFE00_07FF
    } else {
        0xFE00_003F
    };
    opcode | op.format().operand_mask()
}

/// The signed 11-bit immediate (bits 10..0).
pub(crate) const fn imm11(w: u32) -> i32 {
    ((w << 21) as i32) >> 21
}

/// Writes a lower instruction. `pc` (the pair's byte address in VU micro
/// memory) resolves branch targets.
pub(crate) fn write(
    f: &mut fmt::Formatter<'_>,
    op: LowerOp,
    w: u32,
    pc: Option<u32>,
) -> fmt::Result {
    let format = op.format();
    // `move.dest $vf0, $vf0` is the conventional lower no-op.
    if op == LowerOp::Move && (w >> 11).trailing_zeros() >= 10 {
        return f.write_str("nop");
    }
    f.write_str(op.mnemonic())?;
    if format.has_dest() {
        f.write_str(".")?;
        write_dest(f, w)?;
    }
    let (ft, fs, fd) = ((w >> 16) & 0x1F, (w >> 11) & 0x1F, (w >> 6) & 0x1F);
    let fsf = VU_COMPONENTS[((w >> 21) & 3) as usize];
    let ftf = VU_COMPONENTS[((w >> 23) & 3) as usize];
    let target = |f: &mut fmt::Formatter<'_>| match pc {
        Some(pc) => write!(
            f,
            "0x{:04X}",
            pc.wrapping_add(8).wrapping_add((imm11(w) * 8) as u32)
        ),
        None => write!(f, ". + 8 + {}", imm11(w) * 8),
    };
    match format {
        Format::None => Ok(()),
        Format::Imm32 => write!(f, " 0x{w:08X}"),
        Format::LqFtIs => write!(f, " $vf{ft}, {}($vi{fs})", imm11(w)),
        Format::SqFsIt => write!(f, " $vf{fs}, {}($vi{ft})", imm11(w)),
        Format::IlwItIs => write!(f, " $vi{ft}, {}($vi{fs})", imm11(w)),
        Format::ItIsImm15 => write!(
            f,
            " $vi{ft}, $vi{fs}, {}",
            ((w >> 10) & 0x7800) | (w & 0x7FF)
        ),
        Format::Vi1Imm24 => write!(f, " $vi1, 0x{:06X}", w & 0x00FF_FFFF),
        Format::Imm24 => write!(f, " 0x{:06X}", w & 0x00FF_FFFF),
        Format::ItImm12 => write!(f, " $vi{ft}, 0x{:03X}", ((w >> 10) & 0x800) | (w & 0x7FF)),
        Format::Imm12 => write!(f, " 0x{:03X}", ((w >> 10) & 0x800) | (w & 0x7FF)),
        Format::ItIs => write!(f, " $vi{ft}, $vi{fs}"),
        Format::It => write!(f, " $vi{ft}"),
        Format::Is => write!(f, " $vi{fs}"),
        Format::Branch => {
            f.write_str(" ")?;
            target(f)
        }
        Format::ItBranch => {
            write!(f, " $vi{ft}, ")?;
            target(f)
        }
        Format::ItIsBranch => {
            write!(f, " $vi{ft}, $vi{fs}, ")?;
            target(f)
        }
        Format::IsBranch => {
            write!(f, " $vi{fs}, ")?;
            target(f)
        }
        Format::IdIsIt => write!(f, " $vi{fd}, $vi{fs}, $vi{ft}"),
        Format::ItIsImm5 => write!(f, " $vi{ft}, $vi{fs}, {}", (((fd << 3) as u8 as i8) >> 3)),
        Format::FtFs => write!(f, " $vf{ft}, $vf{fs}"),
        Format::Lqi => write!(f, " $vf{ft}, ($vi{fs}++)"),
        Format::Sqi => write!(f, " $vf{fs}, ($vi{ft}++)"),
        Format::Lqd => write!(f, " $vf{ft}, (--$vi{fs})"),
        Format::Sqd => write!(f, " $vf{fs}, (--$vi{ft})"),
        Format::Div => write!(f, " Q, $vf{fs}{fsf}, $vf{ft}{ftf}"),
        Format::Sqrt => write!(f, " Q, $vf{ft}{ftf}"),
        Format::Mtir => write!(f, " $vi{ft}, $vf{fs}{fsf}"),
        Format::Mfir => write!(f, " $vf{ft}, $vi{fs}"),
        Format::Ilwr => write!(f, " $vi{ft}, ($vi{fs})"),
        Format::FtR => write!(f, " $vf{ft}, R"),
        Format::RFsf => write!(f, " R, $vf{fs}{fsf}"),
        Format::FtP => write!(f, " $vf{ft}, P"),
        Format::PFs => write!(f, " P, $vf{fs}"),
        Format::PFsf => write!(f, " P, $vf{fs}{fsf}"),
    }
}
