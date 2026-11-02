//! The R5900 operations: one table giving each its mnemonic, operand format,
//! decode class and control flow.
//!
//! Mnemonics follow rabbitizer's names for the R5900 (the reference this
//! crate is tested against), with one exception: `sqrt.s` (rabbitizer: `c1`).

use super::format::Format;

/// How an operation is identified: which bits of the word are fixed by the
/// opcode. Every bit that is neither fixed nor an operand must be zero in a
/// valid instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// The whole word is fixed (`nop`).
    Word,
    /// Primary opcode, bits 31..26.
    Primary,
    /// `SPECIAL` function, bits 31..26 and 5..0.
    Special,
    /// `SPECIAL` function plus the `sa` field (`sync`, `sync.p`).
    SpecialSa,
    /// `REGIMM`, bits 31..26 and 20..16.
    Regimm,
    /// Coprocessor and `rs` field, bits 31..21 (moves).
    CopRs,
    /// Coprocessor move with an interlock bit, bits 31..21 and 0.
    CopRsI,
    /// Coprocessor branch, bits 31..16.
    CopBranch,
    /// Coprocessor operation, bits 31..21 and 5..0 (COP0 `CO`, COP1 formats).
    CopFunct,
    /// MMI function, bits 31..26 and 5..0.
    Mmi,
    /// MMI function and sub-function, bits 31..26 and 10..0.
    MmiSub,
    /// COP2 macro operation, bits 31..25 and 5..0.
    Vu,
    /// COP2 macro special operation, bits 31..25 and 10..0.
    VuSpecial,
    /// COP2 macro special operation that also fixes its `dest` field
    /// (`vilwr.x` and the like), bits 31..21 and 10..0.
    VuSpecialDest,
}

impl Class {
    /// The bits of the word this class fixes.
    pub(crate) const fn fixed_mask(self) -> u32 {
        match self {
            Self::Word => 0xFFFF_FFFF,
            Self::Primary => 0xFC00_0000,
            Self::Special | Self::Mmi => 0xFC00_003F,
            Self::SpecialSa | Self::MmiSub => 0xFC00_07FF,
            Self::Regimm => 0xFC1F_0000,
            Self::CopRs => 0xFFE0_0000,
            Self::CopRsI => 0xFFE0_0001,
            Self::CopBranch => 0xFFFF_0000,
            Self::CopFunct => 0xFFE0_003F,
            Self::Vu => 0xFE00_003F,
            Self::VuSpecial => 0xFE00_07FF,
            Self::VuSpecialDest => 0xFFE0_07FF,
        }
    }
}

/// What an instruction does to control flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Flow {
    /// Falls through to the next instruction.
    Next,
    /// Conditional branch with a delay slot.
    Branch,
    /// Branch-likely: the delay slot runs only if the branch is taken.
    BranchLikely,
    /// Conditional branch that links (`bltzal`, `bgezal`).
    BranchLink,
    /// Branch-likely that links (`bltzall`, `bgezall`).
    BranchLinkLikely,
    /// Jump within the 256 MiB region (`j`).
    Jump,
    /// Call within the 256 MiB region (`jal`).
    JumpLink,
    /// Jump to a register (`jr`).
    JumpReg,
    /// Call through a register (`jalr`).
    JumpRegLink,
    /// System call exception.
    Syscall,
    /// Breakpoint exception.
    Break,
    /// Conditional trap.
    Trap,
    /// Return from exception (no delay slot).
    Eret,
}

impl Flow {
    /// Whether the next instruction is a delay slot.
    #[must_use]
    pub const fn has_delay_slot(self) -> bool {
        matches!(
            self,
            Self::Branch
                | Self::BranchLikely
                | Self::BranchLink
                | Self::BranchLinkLikely
                | Self::Jump
                | Self::JumpLink
                | Self::JumpReg
                | Self::JumpRegLink
        )
    }

    /// Whether the delay slot is skipped when the branch is not taken.
    #[must_use]
    pub const fn is_likely(self) -> bool {
        matches!(self, Self::BranchLikely | Self::BranchLinkLikely)
    }

    /// Whether the instruction writes a return address.
    #[must_use]
    pub const fn links(self) -> bool {
        matches!(
            self,
            Self::BranchLink | Self::BranchLinkLikely | Self::JumpLink | Self::JumpRegLink
        )
    }
}

macro_rules! ops {
    ($( $variant:ident = $mnemonic:literal, $format:ident, $class:ident, $flow:ident; )*) => {
        /// An R5900 operation. [`Op::Invalid`] stands for every word that is
        /// not an instruction.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(u16)]
        pub enum Op {
            /// Not an instruction.
            Invalid,
            $(
                #[doc = concat!("`", $mnemonic, "`")]
                $variant,
            )*
        }

        impl Op {
            /// Every operation except [`Op::Invalid`].
            pub const ALL: &'static [Op] = &[$(Op::$variant,)*];

            /// The assembler mnemonic.
            #[must_use]
            pub const fn mnemonic(self) -> &'static str {
                match self {
                    Op::Invalid => "(invalid)",
                    $(Op::$variant => $mnemonic,)*
                }
            }

            /// The operand format.
            #[must_use]
            pub const fn format(self) -> Format {
                match self {
                    Op::Invalid => Format::None,
                    $(Op::$variant => Format::$format,)*
                }
            }

            /// What the instruction does to control flow.
            #[must_use]
            pub const fn flow(self) -> Flow {
                match self {
                    Op::Invalid => Flow::Next,
                    $(Op::$variant => Flow::$flow,)*
                }
            }

            pub(crate) const fn class(self) -> Class {
                match self {
                    Op::Invalid => Class::Word,
                    $(Op::$variant => Class::$class,)*
                }
            }
        }
    };
}

ops! {
    // Primary opcodes.
    J = "j", Jump, Primary, Jump;
    Jal = "jal", Jump, Primary, JumpLink;
    Beq = "beq", RsRtBranch, Primary, Branch;
    Bne = "bne", RsRtBranch, Primary, Branch;
    Blez = "blez", RsBranch, Primary, Branch;
    Bgtz = "bgtz", RsBranch, Primary, Branch;
    Addi = "addi", RtRsImm, Primary, Next;
    Addiu = "addiu", RtRsImm, Primary, Next;
    Slti = "slti", RtRsImm, Primary, Next;
    Sltiu = "sltiu", RtRsImm, Primary, Next;
    Andi = "andi", RtRsUimm, Primary, Next;
    Ori = "ori", RtRsUimm, Primary, Next;
    Xori = "xori", RtRsUimm, Primary, Next;
    Lui = "lui", RtUimm, Primary, Next;
    Beql = "beql", RsRtBranch, Primary, BranchLikely;
    Bnel = "bnel", RsRtBranch, Primary, BranchLikely;
    Blezl = "blezl", RsBranch, Primary, BranchLikely;
    Bgtzl = "bgtzl", RsBranch, Primary, BranchLikely;
    Daddi = "daddi", RtRsImm, Primary, Next;
    Daddiu = "daddiu", RtRsImm, Primary, Next;
    Ldl = "ldl", RtMem, Primary, Next;
    Ldr = "ldr", RtMem, Primary, Next;
    Lq = "lq", RtMem, Primary, Next;
    Sq = "sq", RtMem, Primary, Next;
    Lb = "lb", RtMem, Primary, Next;
    Lh = "lh", RtMem, Primary, Next;
    Lwl = "lwl", RtMem, Primary, Next;
    Lw = "lw", RtMem, Primary, Next;
    Lbu = "lbu", RtMem, Primary, Next;
    Lhu = "lhu", RtMem, Primary, Next;
    Lwr = "lwr", RtMem, Primary, Next;
    Lwu = "lwu", RtMem, Primary, Next;
    Sb = "sb", RtMem, Primary, Next;
    Sh = "sh", RtMem, Primary, Next;
    Swl = "swl", RtMem, Primary, Next;
    Sw = "sw", RtMem, Primary, Next;
    Sdl = "sdl", RtMem, Primary, Next;
    Sdr = "sdr", RtMem, Primary, Next;
    Swr = "swr", RtMem, Primary, Next;
    Cache = "cache", OpMem, Primary, Next;
    Lwc1 = "lwc1", FtMem, Primary, Next;
    Pref = "pref", OpMem, Primary, Next;
    Lqc2 = "lqc2", VfMem, Primary, Next;
    Ld = "ld", RtMem, Primary, Next;
    Swc1 = "swc1", FtMem, Primary, Next;
    Sqc2 = "sqc2", VfMem, Primary, Next;
    Sd = "sd", RtMem, Primary, Next;

    // SPECIAL.
    Nop = "nop", None, Word, Next;
    Sll = "sll", RdRtSa, Special, Next;
    Srl = "srl", RdRtSa, Special, Next;
    Sra = "sra", RdRtSa, Special, Next;
    Sllv = "sllv", RdRtRs, Special, Next;
    Srlv = "srlv", RdRtRs, Special, Next;
    Srav = "srav", RdRtRs, Special, Next;
    Jr = "jr", Rs, Special, JumpReg;
    Jalr = "jalr", Jalr, Special, JumpRegLink;
    Movz = "movz", RdRsRt, Special, Next;
    Movn = "movn", RdRsRt, Special, Next;
    Syscall = "syscall", Code20, Special, Syscall;
    Break = "break", Code20Split, Special, Break;
    Sync = "sync", None, SpecialSa, Next;
    SyncP = "sync.p", None, SpecialSa, Next;
    Mfhi = "mfhi", Rd, Special, Next;
    Mthi = "mthi", Rs, Special, Next;
    Mflo = "mflo", Rd, Special, Next;
    Mtlo = "mtlo", Rs, Special, Next;
    Dsllv = "dsllv", RdRtRs, Special, Next;
    Dsrlv = "dsrlv", RdRtRs, Special, Next;
    Dsrav = "dsrav", RdRtRs, Special, Next;
    Mult = "mult", RdRsRt, Special, Next;
    Multu = "multu", MulRd, Special, Next;
    Div = "div", RsRt, Special, Next;
    Divu = "divu", RsRt, Special, Next;
    Add = "add", RdRsRt, Special, Next;
    Addu = "addu", RdRsRt, Special, Next;
    Sub = "sub", RdRsRt, Special, Next;
    Subu = "subu", RdRsRt, Special, Next;
    And = "and", RdRsRt, Special, Next;
    Or = "or", RdRsRt, Special, Next;
    Xor = "xor", RdRsRt, Special, Next;
    Nor = "nor", RdRsRt, Special, Next;
    Mfsa = "mfsa", Rd, Special, Next;
    Mtsa = "mtsa", Rs, Special, Next;
    Slt = "slt", RdRsRt, Special, Next;
    Sltu = "sltu", RdRsRt, Special, Next;
    Dadd = "dadd", RdRsRt, Special, Next;
    Daddu = "daddu", RdRsRt, Special, Next;
    Dsub = "dsub", RdRsRt, Special, Next;
    Dsubu = "dsubu", RdRsRt, Special, Next;
    Tge = "tge", RsRtCode, Special, Trap;
    Tgeu = "tgeu", RsRtCode, Special, Trap;
    Tlt = "tlt", RsRtCode, Special, Trap;
    Tltu = "tltu", RsRtCode, Special, Trap;
    Teq = "teq", RsRtCode, Special, Trap;
    Tne = "tne", RsRtCode, Special, Trap;
    Dsll = "dsll", RdRtSa, Special, Next;
    Dsrl = "dsrl", RdRtSa, Special, Next;
    Dsra = "dsra", RdRtSa, Special, Next;
    Dsll32 = "dsll32", RdRtSa, Special, Next;
    Dsrl32 = "dsrl32", RdRtSa, Special, Next;
    Dsra32 = "dsra32", RdRtSa, Special, Next;

    // REGIMM.
    Bltz = "bltz", RsBranch, Regimm, Branch;
    Bgez = "bgez", RsBranch, Regimm, Branch;
    Bltzl = "bltzl", RsBranch, Regimm, BranchLikely;
    Bgezl = "bgezl", RsBranch, Regimm, BranchLikely;
    Tgei = "tgei", RsImm, Regimm, Trap;
    Tgeiu = "tgeiu", RsImm, Regimm, Trap;
    Tlti = "tlti", RsImm, Regimm, Trap;
    Tltiu = "tltiu", RsImm, Regimm, Trap;
    Teqi = "teqi", RsImm, Regimm, Trap;
    Tnei = "tnei", RsImm, Regimm, Trap;
    Bltzal = "bltzal", RsBranch, Regimm, BranchLink;
    Bgezal = "bgezal", RsBranch, Regimm, BranchLink;
    Bltzall = "bltzall", RsBranch, Regimm, BranchLinkLikely;
    Bgezall = "bgezall", RsBranch, Regimm, BranchLinkLikely;
    Mtsab = "mtsab", RsImm, Regimm, Next;
    Mtsah = "mtsah", RsImm, Regimm, Next;

    // COP0.
    Mfc0 = "mfc0", RtCop0, CopRs, Next;
    Mtc0 = "mtc0", RtCop0, CopRs, Next;
    Bc0f = "bc0f", Branch, CopBranch, Branch;
    Bc0t = "bc0t", Branch, CopBranch, Branch;
    Bc0fl = "bc0fl", Branch, CopBranch, BranchLikely;
    Bc0tl = "bc0tl", Branch, CopBranch, BranchLikely;
    Tlbr = "tlbr", None, CopFunct, Next;
    Tlbwi = "tlbwi", None, CopFunct, Next;
    Tlbwr = "tlbwr", None, CopFunct, Next;
    Tlbp = "tlbp", None, CopFunct, Next;
    Eret = "eret", None, CopFunct, Eret;
    Ei = "ei", None, CopFunct, Next;
    Di = "di", None, CopFunct, Next;

    // COP1 (single precision only).
    Mfc1 = "mfc1", RtFs, CopRs, Next;
    Cfc1 = "cfc1", RtCop1Ctl, CopRs, Next;
    Mtc1 = "mtc1", RtFs, CopRs, Next;
    Ctc1 = "ctc1", RtCop1Ctl, CopRs, Next;
    Bc1f = "bc1f", Branch, CopBranch, Branch;
    Bc1t = "bc1t", Branch, CopBranch, Branch;
    Bc1fl = "bc1fl", Branch, CopBranch, BranchLikely;
    Bc1tl = "bc1tl", Branch, CopBranch, BranchLikely;
    AddS = "add.s", FdFsFt, CopFunct, Next;
    SubS = "sub.s", FdFsFt, CopFunct, Next;
    MulS = "mul.s", FdFsFt, CopFunct, Next;
    DivS = "div.s", FdFsFt, CopFunct, Next;
    SqrtS = "sqrt.s", FdFt, CopFunct, Next;
    AbsS = "abs.s", FdFs, CopFunct, Next;
    MovS = "mov.s", FdFs, CopFunct, Next;
    NegS = "neg.s", FdFs, CopFunct, Next;
    RsqrtS = "rsqrt.s", FdFsFt, CopFunct, Next;
    AddaS = "adda.s", FsFt, CopFunct, Next;
    SubaS = "suba.s", FsFt, CopFunct, Next;
    MulaS = "mula.s", FsFt, CopFunct, Next;
    MaddS = "madd.s", FdFsFt, CopFunct, Next;
    MsubS = "msub.s", FdFsFt, CopFunct, Next;
    MaddaS = "madda.s", FsFt, CopFunct, Next;
    MsubaS = "msuba.s", FsFt, CopFunct, Next;
    CvtWS = "cvt.w.s", FdFs, CopFunct, Next;
    MaxS = "max.s", FdFsFt, CopFunct, Next;
    MinS = "min.s", FdFsFt, CopFunct, Next;
    CFS = "c.f.s", FsFt, CopFunct, Next;
    CEqS = "c.eq.s", FsFt, CopFunct, Next;
    CLtS = "c.lt.s", FsFt, CopFunct, Next;
    CLeS = "c.le.s", FsFt, CopFunct, Next;
    CvtSW = "cvt.s.w", FdFs, CopFunct, Next;

    // MMI.
    Madd = "madd", RdRsRt, Mmi, Next;
    Maddu = "maddu", RdRsRt, Mmi, Next;
    Plzcw = "plzcw", RdRs, Mmi, Next;
    Mfhi1 = "mfhi1", Rd, Mmi, Next;
    Mthi1 = "mthi1", Rs, Mmi, Next;
    Mflo1 = "mflo1", Rd, Mmi, Next;
    Mtlo1 = "mtlo1", Rs, Mmi, Next;
    Mult1 = "mult1", RdRsRt, Mmi, Next;
    Multu1 = "multu1", RdRsRt, Mmi, Next;
    Div1 = "div1", RsRt, Mmi, Next;
    Divu1 = "divu1", RsRt, Mmi, Next;
    Madd1 = "madd1", RdRsRt, Mmi, Next;
    Maddu1 = "maddu1", RdRsRt, Mmi, Next;
    PmfhlLw = "pmfhl.lw", Rd, MmiSub, Next;
    PmfhlUw = "pmfhl.uw", Rd, MmiSub, Next;
    PmfhlSlw = "pmfhl.slw", Rd, MmiSub, Next;
    PmfhlLh = "pmfhl.lh", Rd, MmiSub, Next;
    PmfhlSh = "pmfhl.sh", Rd, MmiSub, Next;
    PmthlLw = "pmthl.lw", Rs, MmiSub, Next;
    Psllh = "psllh", RdRtSa, Mmi, Next;
    Psrlh = "psrlh", RdRtSa, Mmi, Next;
    Psrah = "psrah", RdRtSa, Mmi, Next;
    Psllw = "psllw", RdRtSa, Mmi, Next;
    Psrlw = "psrlw", RdRtSa, Mmi, Next;
    Psraw = "psraw", RdRtSa, Mmi, Next;

    // MMI0.
    Paddw = "paddw", RdRsRt, MmiSub, Next;
    Psubw = "psubw", RdRsRt, MmiSub, Next;
    Pcgtw = "pcgtw", RdRsRt, MmiSub, Next;
    Pmaxw = "pmaxw", RdRsRt, MmiSub, Next;
    Paddh = "paddh", RdRsRt, MmiSub, Next;
    Psubh = "psubh", RdRsRt, MmiSub, Next;
    Pcgth = "pcgth", RdRsRt, MmiSub, Next;
    Pmaxh = "pmaxh", RdRsRt, MmiSub, Next;
    Paddb = "paddb", RdRsRt, MmiSub, Next;
    Psubb = "psubb", RdRsRt, MmiSub, Next;
    Pcgtb = "pcgtb", RdRsRt, MmiSub, Next;
    Paddsw = "paddsw", RdRsRt, MmiSub, Next;
    Psubsw = "psubsw", RdRsRt, MmiSub, Next;
    Pextlw = "pextlw", RdRsRt, MmiSub, Next;
    Ppacw = "ppacw", RdRsRt, MmiSub, Next;
    Paddsh = "paddsh", RdRsRt, MmiSub, Next;
    Psubsh = "psubsh", RdRsRt, MmiSub, Next;
    Pextlh = "pextlh", RdRsRt, MmiSub, Next;
    Ppach = "ppach", RdRsRt, MmiSub, Next;
    Paddsb = "paddsb", RdRsRt, MmiSub, Next;
    Psubsb = "psubsb", RdRsRt, MmiSub, Next;
    Pextlb = "pextlb", RdRsRt, MmiSub, Next;
    Ppacb = "ppacb", RdRsRt, MmiSub, Next;
    Pext5 = "pext5", RdRt, MmiSub, Next;
    Ppac5 = "ppac5", RdRt, MmiSub, Next;

    // MMI1.
    Pabsw = "pabsw", RdRt, MmiSub, Next;
    Pceqw = "pceqw", RdRsRt, MmiSub, Next;
    Pminw = "pminw", RdRsRt, MmiSub, Next;
    Padsbh = "padsbh", RdRsRt, MmiSub, Next;
    Pabsh = "pabsh", RdRt, MmiSub, Next;
    Pceqh = "pceqh", RdRsRt, MmiSub, Next;
    Pminh = "pminh", RdRsRt, MmiSub, Next;
    Pceqb = "pceqb", RdRsRt, MmiSub, Next;
    Padduw = "padduw", RdRsRt, MmiSub, Next;
    Psubuw = "psubuw", RdRsRt, MmiSub, Next;
    Pextuw = "pextuw", RdRsRt, MmiSub, Next;
    Padduh = "padduh", RdRsRt, MmiSub, Next;
    Psubuh = "psubuh", RdRsRt, MmiSub, Next;
    Pextuh = "pextuh", RdRsRt, MmiSub, Next;
    Paddub = "paddub", RdRsRt, MmiSub, Next;
    Psubub = "psubub", RdRsRt, MmiSub, Next;
    Pextub = "pextub", RdRsRt, MmiSub, Next;
    Qfsrv = "qfsrv", RdRsRt, MmiSub, Next;

    // MMI2.
    Pmaddw = "pmaddw", RdRsRt, MmiSub, Next;
    Psllvw = "psllvw", RdRtRs, MmiSub, Next;
    Psrlvw = "psrlvw", RdRtRs, MmiSub, Next;
    Pmsubw = "pmsubw", RdRsRt, MmiSub, Next;
    Pmfhi = "pmfhi", Rd, MmiSub, Next;
    Pmflo = "pmflo", Rd, MmiSub, Next;
    Pinth = "pinth", RdRsRt, MmiSub, Next;
    Pmultw = "pmultw", RdRsRt, MmiSub, Next;
    Pdivw = "pdivw", RsRt, MmiSub, Next;
    Pcpyld = "pcpyld", RdRsRt, MmiSub, Next;
    Pmaddh = "pmaddh", RdRsRt, MmiSub, Next;
    Phmadh = "phmadh", RdRsRt, MmiSub, Next;
    Pand = "pand", RdRsRt, MmiSub, Next;
    Pxor = "pxor", RdRsRt, MmiSub, Next;
    Pmsubh = "pmsubh", RdRsRt, MmiSub, Next;
    Phmsbh = "phmsbh", RdRsRt, MmiSub, Next;
    Pexeh = "pexeh", RdRt, MmiSub, Next;
    Prevh = "prevh", RdRt, MmiSub, Next;
    Pmulth = "pmulth", RdRsRt, MmiSub, Next;
    Pdivbw = "pdivbw", RsRt, MmiSub, Next;
    Pexew = "pexew", RdRt, MmiSub, Next;
    Prot3w = "prot3w", RdRt, MmiSub, Next;

    // MMI3.
    Pmadduw = "pmadduw", RdRsRt, MmiSub, Next;
    Psravw = "psravw", RdRtRs, MmiSub, Next;
    Pmthi = "pmthi", Rs, MmiSub, Next;
    Pmtlo = "pmtlo", Rs, MmiSub, Next;
    Pinteh = "pinteh", RdRsRt, MmiSub, Next;
    Pmultuw = "pmultuw", RdRsRt, MmiSub, Next;
    Pdivuw = "pdivuw", RsRt, MmiSub, Next;
    Pcpyud = "pcpyud", RdRsRt, MmiSub, Next;
    Por = "por", RdRsRt, MmiSub, Next;
    Pnor = "pnor", RdRsRt, MmiSub, Next;
    Pexch = "pexch", RdRt, MmiSub, Next;
    Pcpyh = "pcpyh", RdRt, MmiSub, Next;
    Pexcw = "pexcw", RdRt, MmiSub, Next;

    // COP2 moves and branches.
    Qmfc2Ni = "qmfc2.ni", RtVf, CopRsI, Next;
    Qmfc2I = "qmfc2.i", RtVf, CopRsI, Next;
    Cfc2Ni = "cfc2.ni", RtVi, CopRsI, Next;
    Cfc2I = "cfc2.i", RtVi, CopRsI, Next;
    Qmtc2Ni = "qmtc2.ni", RtVf, CopRsI, Next;
    Qmtc2I = "qmtc2.i", RtVf, CopRsI, Next;
    Ctc2Ni = "ctc2.ni", RtVi, CopRsI, Next;
    Ctc2I = "ctc2.i", RtVi, CopRsI, Next;
    Bc2f = "bc2f", Branch, CopBranch, Branch;
    Bc2t = "bc2t", Branch, CopBranch, Branch;
    Bc2fl = "bc2fl", Branch, CopBranch, BranchLikely;
    Bc2tl = "bc2tl", Branch, CopBranch, BranchLikely;

    // COP2 macro mode (VU0), upper-pipeline operations.
    Vaddx = "vaddx", VFdFsFtBc, Vu, Next;
    Vaddy = "vaddy", VFdFsFtBc, Vu, Next;
    Vaddz = "vaddz", VFdFsFtBc, Vu, Next;
    Vaddw = "vaddw", VFdFsFtBc, Vu, Next;
    Vsubx = "vsubx", VFdFsFtBc, Vu, Next;
    Vsuby = "vsuby", VFdFsFtBc, Vu, Next;
    Vsubz = "vsubz", VFdFsFtBc, Vu, Next;
    Vsubw = "vsubw", VFdFsFtBc, Vu, Next;
    Vmaddx = "vmaddx", VFdFsFtBc, Vu, Next;
    Vmaddy = "vmaddy", VFdFsFtBc, Vu, Next;
    Vmaddz = "vmaddz", VFdFsFtBc, Vu, Next;
    Vmaddw = "vmaddw", VFdFsFtBc, Vu, Next;
    Vmsubx = "vmsubx", VFdFsFtBc, Vu, Next;
    Vmsuby = "vmsuby", VFdFsFtBc, Vu, Next;
    Vmsubz = "vmsubz", VFdFsFtBc, Vu, Next;
    Vmsubw = "vmsubw", VFdFsFtBc, Vu, Next;
    Vmaxx = "vmaxx", VFdFsFtBc, Vu, Next;
    Vmaxy = "vmaxy", VFdFsFtBc, Vu, Next;
    Vmaxz = "vmaxz", VFdFsFtBc, Vu, Next;
    Vmaxw = "vmaxw", VFdFsFtBc, Vu, Next;
    Vminix = "vminix", VFdFsFtBc, Vu, Next;
    Vminiy = "vminiy", VFdFsFtBc, Vu, Next;
    Vminiz = "vminiz", VFdFsFtBc, Vu, Next;
    Vminiw = "vminiw", VFdFsFtBc, Vu, Next;
    Vmulx = "vmulx", VFdFsFtBc, Vu, Next;
    Vmuly = "vmuly", VFdFsFtBc, Vu, Next;
    Vmulz = "vmulz", VFdFsFtBc, Vu, Next;
    Vmulw = "vmulw", VFdFsFtBc, Vu, Next;
    Vmulq = "vmulq", VFdFsQ, Vu, Next;
    Vmaxi = "vmaxi", VFdFsI, Vu, Next;
    Vmuli = "vmuli", VFdFsI, Vu, Next;
    Vminii = "vminii", VFdFsI, Vu, Next;
    Vaddq = "vaddq", VFdFsQ, Vu, Next;
    Vmaddq = "vmaddq", VFdFsQ, Vu, Next;
    Vaddi = "vaddi", VFdFsI, Vu, Next;
    Vmaddi = "vmaddi", VFdFsI, Vu, Next;
    Vsubq = "vsubq", VFdFsQ, Vu, Next;
    Vmsubq = "vmsubq", VFdFsQ, Vu, Next;
    Vsubi = "vsubi", VFdFsI, Vu, Next;
    Vmsubi = "vmsubi", VFdFsI, Vu, Next;
    Vadd = "vadd", VFdFsFt, Vu, Next;
    Vmadd = "vmadd", VFdFsFt, Vu, Next;
    Vmul = "vmul", VFdFsFt, Vu, Next;
    Vmax = "vmax", VFdFsFt, Vu, Next;
    Vsub = "vsub", VFdFsFt, Vu, Next;
    Vmsub = "vmsub", VFdFsFt, Vu, Next;
    Vopmsub = "vopmsub", VFdFsFt, Vu, Next;
    Vmini = "vmini", VFdFsFt, Vu, Next;
    Viadd = "viadd", ViIdIsIt, Vu, Next;
    Visub = "visub", ViIdIsIt, Vu, Next;
    Viaddi = "viaddi", ViItIsImm5, Vu, Next;
    Viand = "viand", ViIdIsIt, Vu, Next;
    Vior = "vior", ViIdIsIt, Vu, Next;
    Vcallms = "vcallms", Imm15, Vu, Next;
    Vcallmsr = "vcallmsr", ViS, Vu, Next;

    // COP2 macro mode, special table.
    Vaddax = "vaddax", VAccFsFtBc, VuSpecial, Next;
    Vadday = "vadday", VAccFsFtBc, VuSpecial, Next;
    Vaddaz = "vaddaz", VAccFsFtBc, VuSpecial, Next;
    Vaddaw = "vaddaw", VAccFsFtBc, VuSpecial, Next;
    Vsubax = "vsubax", VAccFsFtBc, VuSpecial, Next;
    Vsubay = "vsubay", VAccFsFtBc, VuSpecial, Next;
    Vsubaz = "vsubaz", VAccFsFtBc, VuSpecial, Next;
    Vsubaw = "vsubaw", VAccFsFtBc, VuSpecial, Next;
    Vmaddax = "vmaddax", VAccFsFtBc, VuSpecial, Next;
    Vmadday = "vmadday", VAccFsFtBc, VuSpecial, Next;
    Vmaddaz = "vmaddaz", VAccFsFtBc, VuSpecial, Next;
    Vmaddaw = "vmaddaw", VAccFsFtBc, VuSpecial, Next;
    Vmsubax = "vmsubax", VAccFsFtBc, VuSpecial, Next;
    Vmsubay = "vmsubay", VAccFsFtBc, VuSpecial, Next;
    Vmsubaz = "vmsubaz", VAccFsFtBc, VuSpecial, Next;
    Vmsubaw = "vmsubaw", VAccFsFtBc, VuSpecial, Next;
    Vitof0 = "vitof0", VFtFs, VuSpecial, Next;
    Vitof4 = "vitof4", VFtFs, VuSpecial, Next;
    Vitof12 = "vitof12", VFtFs, VuSpecial, Next;
    Vitof15 = "vitof15", VFtFs, VuSpecial, Next;
    Vftoi0 = "vftoi0", VFtFs, VuSpecial, Next;
    Vftoi4 = "vftoi4", VFtFs, VuSpecial, Next;
    Vftoi12 = "vftoi12", VFtFs, VuSpecial, Next;
    Vftoi15 = "vftoi15", VFtFs, VuSpecial, Next;
    Vmulax = "vmulax", VAccFsFtBc, VuSpecial, Next;
    Vmulay = "vmulay", VAccFsFtBc, VuSpecial, Next;
    Vmulaz = "vmulaz", VAccFsFtBc, VuSpecial, Next;
    Vmulaw = "vmulaw", VAccFsFtBc, VuSpecial, Next;
    Vmulaq = "vmulaq", VAccFsQ, VuSpecial, Next;
    Vabs = "vabs", VFtFs, VuSpecial, Next;
    Vmulai = "vmulai", VAccFsI, VuSpecial, Next;
    Vclipw = "vclipw", VClip, VuSpecial, Next;
    Vaddaq = "vaddaq", VAccFsQ, VuSpecial, Next;
    Vmaddaq = "vmaddaq", VAccFsQ, VuSpecial, Next;
    Vaddai = "vaddai", VAccFsI, VuSpecial, Next;
    Vmaddai = "vmaddai", VAccFsI, VuSpecial, Next;
    Vsubaq = "vsubaq", VAccFsQ, VuSpecial, Next;
    Vmsubaq = "vmsubaq", VAccFsQ, VuSpecial, Next;
    Vsubai = "vsubai", VAccFsI, VuSpecial, Next;
    Vmsubai = "vmsubai", VAccFsI, VuSpecial, Next;
    Vadda = "vadda", VAccFsFt, VuSpecial, Next;
    Vmadda = "vmadda", VAccFsFt, VuSpecial, Next;
    Vmula = "vmula", VAccFsFt, VuSpecial, Next;
    Vsuba = "vsuba", VAccFsFt, VuSpecial, Next;
    Vmsuba = "vmsuba", VAccFsFt, VuSpecial, Next;
    Vopmula = "vopmula", VAccFsFt, VuSpecial, Next;
    Vnop = "vnop", None, VuSpecial, Next;
    Vmove = "vmove", VFtFs, VuSpecial, Next;
    Vmr32 = "vmr32", VFtFs, VuSpecial, Next;
    Vlqi = "vlqi", VLqi, VuSpecial, Next;
    Vsqi = "vsqi", VSqi, VuSpecial, Next;
    Vlqd = "vlqd", VLqd, VuSpecial, Next;
    Vsqd = "vsqd", VSqd, VuSpecial, Next;
    Vdiv = "vdiv", VDiv, VuSpecial, Next;
    Vsqrt = "vsqrt", VSqrt, VuSpecial, Next;
    Vrsqrt = "vrsqrt", VDiv, VuSpecial, Next;
    Vwaitq = "vwaitq", None, VuSpecial, Next;
    Vmtir = "vmtir", VMtir, VuSpecial, Next;
    Vmfir = "vmfir", VMfir, VuSpecial, Next;
    VilwrX = "vilwr.x", VIlwr, VuSpecialDest, Next;
    VilwrY = "vilwr.y", VIlwr, VuSpecialDest, Next;
    VilwrZ = "vilwr.z", VIlwr, VuSpecialDest, Next;
    VilwrW = "vilwr.w", VIlwr, VuSpecialDest, Next;
    ViswrX = "viswr.x", VIlwr, VuSpecialDest, Next;
    ViswrY = "viswr.y", VIlwr, VuSpecialDest, Next;
    ViswrZ = "viswr.z", VIlwr, VuSpecialDest, Next;
    ViswrW = "viswr.w", VIlwr, VuSpecialDest, Next;
    Vrnext = "vrnext", VFtR, VuSpecial, Next;
    Vrget = "vrget", VFtR, VuSpecial, Next;
    Vrinit = "vrinit", VRFs, VuSpecial, Next;
    Vrxor = "vrxor", VRFs, VuSpecial, Next;
}
