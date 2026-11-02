//! Operand formats: which bits of the word carry operands, and how they are
//! written. The syntax follows rabbitizer's (lowercase mnemonics, `$`
//! registers with o32 names, upper-case hexadecimal immediates, decimal
//! shift amounts), so the two can be compared text for text.

use core::fmt::{self, Write};

use super::Insn;
use crate::regs::{GPR_NAMES, VU_COMPONENTS};

/// How an operation's operands are encoded and written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(missing_docs, reason = "each variant names its operands in order")]
pub enum Format {
    None,
    Jump,
    Branch,
    RsBranch,
    RsRtBranch,
    RtRsImm,
    RtRsUimm,
    RtUimm,
    RtMem,
    OpMem,
    FtMem,
    VfMem,
    Rd,
    Rs,
    RdRs,
    RdRt,
    RsRt,
    RdRsRt,
    RdRtRs,
    RdRtSa,
    MulRd,
    Jalr,
    Code20,
    Code20Split,
    RsRtCode,
    RsImm,
    RtCop0,
    RtFs,
    RtCop1Ctl,
    FdFsFt,
    FdFs,
    FdFt,
    FsFt,
    RtVf,
    RtVi,
    VFdFsFt,
    VFdFsFtBc,
    VFdFsQ,
    VFdFsI,
    VAccFsFt,
    VAccFsFtBc,
    VAccFsQ,
    VAccFsI,
    VFtFs,
    VClip,
    VDiv,
    VSqrt,
    VLqi,
    VSqi,
    VLqd,
    VSqd,
    VMtir,
    VMfir,
    VIlwr,
    VFtR,
    VRFs,
    ViIdIsIt,
    ViItIsImm5,
    Imm15,
    ViS,
}

const RS: u32 = 0x03E0_0000;
const RT: u32 = 0x001F_0000;
const RD: u32 = 0x0000_F800;
const SA: u32 = 0x0000_07C0;
const IMM: u32 = 0x0000_FFFF;
const TARGET: u32 = 0x03FF_FFFF;
const CODE20: u32 = 0x03FF_FFC0;
const CODE10: u32 = 0x0000_FFC0;
const DEST: u32 = 0x01E0_0000;
const FSF: u32 = 0x0060_0000;
const FTF: u32 = 0x0180_0000;
const IMM15: u32 = 0x001F_FFC0;

impl Format {
    /// The bits that carry operands.
    #[must_use]
    pub const fn operand_mask(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Jump => TARGET,
            Self::Branch => IMM,
            Self::RsBranch | Self::RsImm => RS | IMM,
            Self::RtUimm => RT | IMM,
            Self::RsRtBranch
            | Self::RtRsImm
            | Self::RtRsUimm
            | Self::RtMem
            | Self::OpMem
            | Self::FtMem
            | Self::VfMem => RS | RT | IMM,
            Self::Rd | Self::ViS => RD,
            Self::Rs => RS,
            Self::RdRs | Self::Jalr => RD | RS,
            Self::RdRt | Self::RtCop0 | Self::RtFs | Self::RtCop1Ctl | Self::RtVf | Self::RtVi => {
                RD | RT
            }
            Self::RsRt => RS | RT,
            Self::RdRsRt | Self::RdRtRs | Self::MulRd => RD | RS | RT,
            Self::RdRtSa | Self::FdFsFt | Self::ViIdIsIt | Self::ViItIsImm5 => RD | RT | SA,
            Self::Code20 | Self::Code20Split => CODE20,
            Self::RsRtCode => RS | RT | CODE10,
            Self::FdFs => SA | RD,
            Self::FdFt => SA | RT,
            Self::FsFt | Self::VIlwr => RD | RT,
            Self::VFdFsFt | Self::VFdFsFtBc => DEST | RT | RD | SA,
            Self::VFdFsQ | Self::VFdFsI => DEST | RD | SA,
            Self::VAccFsFt
            | Self::VAccFsFtBc
            | Self::VFtFs
            | Self::VClip
            | Self::VLqi
            | Self::VSqi
            | Self::VLqd
            | Self::VSqd
            | Self::VMfir => DEST | RT | RD,
            Self::VAccFsQ | Self::VAccFsI => DEST | RD,
            Self::VDiv => FTF | FSF | RT | RD,
            Self::VSqrt => FTF | RT,
            Self::VMtir => FSF | RT | RD,
            Self::VFtR => DEST | RT,
            Self::VRFs => FSF | RD,
            Self::Imm15 => IMM15,
        }
    }

    /// Whether the mnemonic takes a `.xyzw` destination suffix.
    const fn has_dest(self) -> bool {
        matches!(
            self,
            Self::VFdFsFt
                | Self::VFdFsFtBc
                | Self::VFdFsQ
                | Self::VFdFsI
                | Self::VAccFsFt
                | Self::VAccFsFtBc
                | Self::VAccFsQ
                | Self::VAccFsI
                | Self::VFtFs
                | Self::VClip
                | Self::VLqi
                | Self::VSqi
                | Self::VLqd
                | Self::VSqd
                | Self::VMfir
                | Self::VFtR
        )
    }
}

/// An instruction written with branch and jump targets resolved against its
/// address. Made by [`Insn::display`].
#[derive(Clone, Copy, Debug)]
pub struct Display {
    pub(crate) insn: Insn,
    pub(crate) pc: Option<u32>,
}

impl fmt::Display for Display {
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per operand format reads best as a table"
    )]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let insn = self.insn;
        let op = insn.op();
        if !insn.is_valid() {
            return write!(f, ".word 0x{:08X}", insn.word());
        }
        // jalr writes rd only when it is not $ra, multu only when it is not
        // $zero: settle which operands are written before writing them.
        let format = match op.format() {
            Format::Jalr if insn.rd() == 31 => Format::Rs,
            Format::MulRd if insn.rd() == 0 => Format::RsRt,
            Format::Jalr => Format::RdRs,
            Format::MulRd => Format::RdRsRt,
            other => other,
        };
        f.write_str(op.mnemonic())?;
        if format.has_dest() {
            f.write_char('.')?;
            dest(f, insn.dest())?;
        }
        let gpr = |r: u8| GPR_NAMES[r as usize];
        let (rs, rt, rd) = (gpr(insn.rs()), gpr(insn.rt()), gpr(insn.rd()));
        let (vs, vt, vd) = (insn.rd(), insn.rt(), insn.sa());
        match format {
            Format::None => Ok(()),
            Format::Jump => {
                let target = insn.jump_target(self.pc.unwrap_or(0));
                write!(f, " 0x{target:08X}")
            }
            Format::Branch => {
                f.write_char(' ')?;
                self.branch_target(f)
            }
            Format::RsBranch => {
                write!(f, " {rs}, ")?;
                self.branch_target(f)
            }
            Format::RsRtBranch => {
                write!(f, " {rs}, {rt}, ")?;
                self.branch_target(f)
            }
            Format::RtRsImm => write!(f, " {rt}, {rs}, {}", Hex(insn.simm().into())),
            Format::RtRsUimm => write!(f, " {rt}, {rs}, {}", Hex(insn.imm().into())),
            Format::RtUimm => write!(f, " {rt}, {}", Hex(insn.imm().into())),
            Format::RtMem => write!(f, " {rt}, {}({rs})", Hex(insn.simm().into())),
            Format::OpMem => write!(
                f,
                " {}, {}({rs})",
                Hex(insn.rt().into()),
                Hex(insn.simm().into())
            ),
            Format::FtMem => write!(f, " $f{}, {}({rs})", insn.rt(), Hex(insn.simm().into())),
            Format::VfMem => write!(f, " $vf{}, {}({rs})", insn.rt(), Hex(insn.simm().into())),
            Format::Rd => write!(f, " {rd}"),
            Format::Rs => write!(f, " {rs}"),
            Format::RsRt => write!(f, " {rs}, {rt}"),
            Format::RdRs | Format::Jalr => write!(f, " {rd}, {rs}"),
            Format::RdRt => write!(f, " {rd}, {rt}"),
            Format::RdRsRt | Format::MulRd => write!(f, " {rd}, {rs}, {rt}"),
            Format::RdRtRs => write!(f, " {rd}, {rt}, {rs}"),
            Format::RdRtSa => write!(f, " {rd}, {rt}, {}", insn.sa()),
            Format::Code20 => write!(f, " {}", insn.code20()),
            Format::Code20Split => {
                let code = insn.code20();
                write!(f, " {}, {}", code >> 10, code & 0x3FF)
            }
            Format::RsRtCode => match (insn.word() >> 6) & 0x3FF {
                0 => write!(f, " {rs}, {rt}"),
                code => write!(f, " {rs}, {rt}, {code}"),
            },
            Format::RsImm => write!(f, " {rs}, {}", Hex(insn.simm().into())),
            Format::RtCop0 | Format::RtCop1Ctl => write!(f, " {rt}, ${}", insn.rd()),
            Format::RtFs => write!(f, " {rt}, $f{}", insn.rd()),
            Format::FdFsFt => write!(f, " $f{}, $f{}, $f{}", insn.sa(), insn.rd(), insn.rt()),
            Format::FdFs => write!(f, " $f{}, $f{}", insn.sa(), insn.rd()),
            Format::FdFt => write!(f, " $f{}, $f{}", insn.sa(), insn.rt()),
            Format::FsFt => write!(f, " $f{}, $f{}", insn.rd(), insn.rt()),
            Format::RtVf => write!(f, " {rt}, $vf{}", insn.rd()),
            Format::RtVi => write!(f, " {rt}, $vi{}", insn.rd()),
            Format::VFdFsFt => write!(f, " $vf{vd}, $vf{vs}, $vf{vt}"),
            Format::VFdFsFtBc => write!(f, " $vf{vd}, $vf{vs}, $vf{vt}{}", insn.bc()),
            Format::VFdFsQ => write!(f, " $vf{vd}, $vf{vs}, Q"),
            Format::VFdFsI => write!(f, " $vf{vd}, $vf{vs}, I"),
            Format::VAccFsFt => write!(f, " ACC, $vf{vs}, $vf{vt}"),
            Format::VAccFsFtBc => write!(f, " ACC, $vf{vs}, $vf{vt}{}", insn.bc()),
            Format::VAccFsQ => write!(f, " ACC, $vf{vs}, Q"),
            Format::VAccFsI => write!(f, " ACC, $vf{vs}, I"),
            Format::VFtFs => write!(f, " $vf{vt}, $vf{vs}"),
            Format::VClip => write!(f, " $vf{vs}, $vf{vt}w"),
            Format::VDiv => write!(f, " Q, $vf{vs}{}, $vf{vt}{}", insn.fsf(), insn.ftf()),
            Format::VSqrt => write!(f, " Q, $vf{vt}{}", insn.ftf()),
            Format::VLqi => write!(f, " $vf{vt}, ($vi{vs}++)"),
            Format::VSqi => write!(f, " $vf{vs}, ($vi{vt}++)"),
            Format::VLqd => write!(f, " $vf{vt}, (--$vi{vs})"),
            Format::VSqd => write!(f, " $vf{vs}, (--$vi{vt})"),
            Format::VMtir => write!(f, " $vi{vt}, $vf{vs}{}", insn.fsf()),
            Format::VMfir => write!(f, " $vf{vt}, $vi{vs}"),
            Format::VIlwr => write!(f, " $vi{vt}, ($vi{vs})"),
            Format::VFtR => write!(f, " $vf{vt}, R"),
            Format::VRFs => write!(f, " R, $vf{vs}{}", insn.fsf()),
            Format::ViIdIsIt => write!(f, " $vi{vd}, $vi{vs}, $vi{vt}"),
            Format::ViItIsImm5 => write!(f, " $vi{vt}, $vi{vs}, {}", Hex(insn.imm5().into())),
            Format::Imm15 => write!(f, " {}", Hex((insn.imm15() << 3).into())),
            Format::ViS => write!(f, " $vi{vs}"),
        }
    }
}

impl Display {
    fn branch_target(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.pc {
            Some(pc) => write!(f, "0x{:08X}", self.insn.branch_target(pc)),
            None => write!(f, ". + 4 + {}", Hex((i64::from(self.insn.simm())) << 2)),
        }
    }
}

/// Writes the `xyzw` components selected by a 4-bit `dest` field (x is bit 3).
fn dest(f: &mut fmt::Formatter<'_>, dest: u8) -> fmt::Result {
    for (bit, name) in (0..4).rev().zip(VU_COMPONENTS) {
        if dest & (1 << bit) != 0 {
            f.write_char(name)?;
        }
    }
    Ok(())
}

/// A signed value as `0x1F` or `-0x1F`.
struct Hex(i64);

impl fmt::Display for Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 < 0 {
            write!(f, "-0x{:X}", self.0.unsigned_abs())
        } else {
            write!(f, "0x{:X}", self.0)
        }
    }
}
