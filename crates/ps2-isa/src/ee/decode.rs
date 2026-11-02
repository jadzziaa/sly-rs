//! The R5900 opcode tables as a decode tree. Encodings from the EE Core
//! Instruction Set Manual's opcode maps, as also tabulated by PCSX2 and
//! rabbitizer.

use super::Op;

/// Identifies the operation of `w`, without checking reserved bits.
pub(super) const fn op(w: u32) -> Op {
    if w == 0 {
        return Op::Nop;
    }
    match w >> 26 {
        0x00 => special(w),
        0x01 => regimm(w),
        0x02 => Op::J,
        0x03 => Op::Jal,
        0x04 => Op::Beq,
        0x05 => Op::Bne,
        0x06 => Op::Blez,
        0x07 => Op::Bgtz,
        0x08 => Op::Addi,
        0x09 => Op::Addiu,
        0x0A => Op::Slti,
        0x0B => Op::Sltiu,
        0x0C => Op::Andi,
        0x0D => Op::Ori,
        0x0E => Op::Xori,
        0x0F => Op::Lui,
        0x10 => cop0(w),
        0x11 => cop1(w),
        0x12 => cop2(w),
        0x14 => Op::Beql,
        0x15 => Op::Bnel,
        0x16 => Op::Blezl,
        0x17 => Op::Bgtzl,
        0x18 => Op::Daddi,
        0x19 => Op::Daddiu,
        0x1A => Op::Ldl,
        0x1B => Op::Ldr,
        0x1C => mmi(w),
        0x1E => Op::Lq,
        0x1F => Op::Sq,
        0x20 => Op::Lb,
        0x21 => Op::Lh,
        0x22 => Op::Lwl,
        0x23 => Op::Lw,
        0x24 => Op::Lbu,
        0x25 => Op::Lhu,
        0x26 => Op::Lwr,
        0x27 => Op::Lwu,
        0x28 => Op::Sb,
        0x29 => Op::Sh,
        0x2A => Op::Swl,
        0x2B => Op::Sw,
        0x2C => Op::Sdl,
        0x2D => Op::Sdr,
        0x2E => Op::Swr,
        0x2F => Op::Cache,
        0x31 => Op::Lwc1,
        0x33 => Op::Pref,
        0x36 => Op::Lqc2,
        0x37 => Op::Ld,
        0x39 => Op::Swc1,
        0x3E => Op::Sqc2,
        0x3F => Op::Sd,
        _ => Op::Invalid,
    }
}

const fn special(w: u32) -> Op {
    match w & 0x3F {
        0x00 => Op::Sll,
        0x02 => Op::Srl,
        0x03 => Op::Sra,
        0x04 => Op::Sllv,
        0x06 => Op::Srlv,
        0x07 => Op::Srav,
        0x08 => Op::Jr,
        0x09 => Op::Jalr,
        0x0A => Op::Movz,
        0x0B => Op::Movn,
        0x0C => Op::Syscall,
        0x0D => Op::Break,
        0x0F => match (w >> 6) & 0x1F {
            0x00 => Op::Sync,
            0x10 => Op::SyncP,
            _ => Op::Invalid,
        },
        0x10 => Op::Mfhi,
        0x11 => Op::Mthi,
        0x12 => Op::Mflo,
        0x13 => Op::Mtlo,
        0x14 => Op::Dsllv,
        0x16 => Op::Dsrlv,
        0x17 => Op::Dsrav,
        0x18 => Op::Mult,
        0x19 => Op::Multu,
        0x1A => Op::Div,
        0x1B => Op::Divu,
        0x20 => Op::Add,
        0x21 => Op::Addu,
        0x22 => Op::Sub,
        0x23 => Op::Subu,
        0x24 => Op::And,
        0x25 => Op::Or,
        0x26 => Op::Xor,
        0x27 => Op::Nor,
        0x28 => Op::Mfsa,
        0x29 => Op::Mtsa,
        0x2A => Op::Slt,
        0x2B => Op::Sltu,
        0x2C => Op::Dadd,
        0x2D => Op::Daddu,
        0x2E => Op::Dsub,
        0x2F => Op::Dsubu,
        0x30 => Op::Tge,
        0x31 => Op::Tgeu,
        0x32 => Op::Tlt,
        0x33 => Op::Tltu,
        0x34 => Op::Teq,
        0x36 => Op::Tne,
        0x38 => Op::Dsll,
        0x3A => Op::Dsrl,
        0x3B => Op::Dsra,
        0x3C => Op::Dsll32,
        0x3E => Op::Dsrl32,
        0x3F => Op::Dsra32,
        _ => Op::Invalid,
    }
}

const fn regimm(w: u32) -> Op {
    match (w >> 16) & 0x1F {
        0x00 => Op::Bltz,
        0x01 => Op::Bgez,
        0x02 => Op::Bltzl,
        0x03 => Op::Bgezl,
        0x08 => Op::Tgei,
        0x09 => Op::Tgeiu,
        0x0A => Op::Tlti,
        0x0B => Op::Tltiu,
        0x0C => Op::Teqi,
        0x0E => Op::Tnei,
        0x10 => Op::Bltzal,
        0x11 => Op::Bgezal,
        0x12 => Op::Bltzall,
        0x13 => Op::Bgezall,
        0x18 => Op::Mtsab,
        0x19 => Op::Mtsah,
        _ => Op::Invalid,
    }
}

const fn cop0(w: u32) -> Op {
    match (w >> 21) & 0x1F {
        0x00 => Op::Mfc0,
        0x04 => Op::Mtc0,
        0x08 => match (w >> 16) & 0x1F {
            0 => Op::Bc0f,
            1 => Op::Bc0t,
            2 => Op::Bc0fl,
            3 => Op::Bc0tl,
            _ => Op::Invalid,
        },
        0x10 => match w & 0x3F {
            0x01 => Op::Tlbr,
            0x02 => Op::Tlbwi,
            0x06 => Op::Tlbwr,
            0x08 => Op::Tlbp,
            0x18 => Op::Eret,
            0x38 => Op::Ei,
            0x39 => Op::Di,
            _ => Op::Invalid,
        },
        _ => Op::Invalid,
    }
}

const fn cop1(w: u32) -> Op {
    match (w >> 21) & 0x1F {
        0x00 => Op::Mfc1,
        0x02 => Op::Cfc1,
        0x04 => Op::Mtc1,
        0x06 => Op::Ctc1,
        0x08 => match (w >> 16) & 0x1F {
            0 => Op::Bc1f,
            1 => Op::Bc1t,
            2 => Op::Bc1fl,
            3 => Op::Bc1tl,
            _ => Op::Invalid,
        },
        0x10 => cop1_s(w),
        0x14 => match w & 0x3F {
            0x20 => Op::CvtSW,
            _ => Op::Invalid,
        },
        _ => Op::Invalid,
    }
}

/// COP1 operations on single-precision values (`fmt` = S).
const fn cop1_s(w: u32) -> Op {
    match w & 0x3F {
        0x00 => Op::AddS,
        0x01 => Op::SubS,
        0x02 => Op::MulS,
        0x03 => Op::DivS,
        0x04 => Op::SqrtS,
        0x05 => Op::AbsS,
        0x06 => Op::MovS,
        0x07 => Op::NegS,
        0x16 => Op::RsqrtS,
        0x18 => Op::AddaS,
        0x19 => Op::SubaS,
        0x1A => Op::MulaS,
        0x1C => Op::MaddS,
        0x1D => Op::MsubS,
        0x1E => Op::MaddaS,
        0x1F => Op::MsubaS,
        0x24 => Op::CvtWS,
        0x28 => Op::MaxS,
        0x29 => Op::MinS,
        0x30 => Op::CFS,
        0x32 => Op::CEqS,
        0x34 => Op::CLtS,
        0x36 => Op::CLeS,
        _ => Op::Invalid,
    }
}

const fn mmi(w: u32) -> Op {
    match w & 0x3F {
        0x00 => Op::Madd,
        0x01 => Op::Maddu,
        0x04 => Op::Plzcw,
        0x08 => mmi0(w),
        0x09 => mmi2(w),
        0x10 => Op::Mfhi1,
        0x11 => Op::Mthi1,
        0x12 => Op::Mflo1,
        0x13 => Op::Mtlo1,
        0x18 => Op::Mult1,
        0x19 => Op::Multu1,
        0x1A => Op::Div1,
        0x1B => Op::Divu1,
        0x20 => Op::Madd1,
        0x21 => Op::Maddu1,
        0x28 => mmi1(w),
        0x29 => mmi3(w),
        0x30 => match (w >> 6) & 0x1F {
            0 => Op::PmfhlLw,
            1 => Op::PmfhlUw,
            2 => Op::PmfhlSlw,
            3 => Op::PmfhlLh,
            4 => Op::PmfhlSh,
            _ => Op::Invalid,
        },
        0x31 => match (w >> 6) & 0x1F {
            0 => Op::PmthlLw,
            _ => Op::Invalid,
        },
        0x34 => Op::Psllh,
        0x36 => Op::Psrlh,
        0x37 => Op::Psrah,
        0x3C => Op::Psllw,
        0x3E => Op::Psrlw,
        0x3F => Op::Psraw,
        _ => Op::Invalid,
    }
}

const fn mmi0(w: u32) -> Op {
    match (w >> 6) & 0x1F {
        0x00 => Op::Paddw,
        0x01 => Op::Psubw,
        0x02 => Op::Pcgtw,
        0x03 => Op::Pmaxw,
        0x04 => Op::Paddh,
        0x05 => Op::Psubh,
        0x06 => Op::Pcgth,
        0x07 => Op::Pmaxh,
        0x08 => Op::Paddb,
        0x09 => Op::Psubb,
        0x0A => Op::Pcgtb,
        0x10 => Op::Paddsw,
        0x11 => Op::Psubsw,
        0x12 => Op::Pextlw,
        0x13 => Op::Ppacw,
        0x14 => Op::Paddsh,
        0x15 => Op::Psubsh,
        0x16 => Op::Pextlh,
        0x17 => Op::Ppach,
        0x18 => Op::Paddsb,
        0x19 => Op::Psubsb,
        0x1A => Op::Pextlb,
        0x1B => Op::Ppacb,
        0x1E => Op::Pext5,
        0x1F => Op::Ppac5,
        _ => Op::Invalid,
    }
}

const fn mmi1(w: u32) -> Op {
    match (w >> 6) & 0x1F {
        0x01 => Op::Pabsw,
        0x02 => Op::Pceqw,
        0x03 => Op::Pminw,
        0x04 => Op::Padsbh,
        0x05 => Op::Pabsh,
        0x06 => Op::Pceqh,
        0x07 => Op::Pminh,
        0x0A => Op::Pceqb,
        0x10 => Op::Padduw,
        0x11 => Op::Psubuw,
        0x12 => Op::Pextuw,
        0x14 => Op::Padduh,
        0x15 => Op::Psubuh,
        0x16 => Op::Pextuh,
        0x18 => Op::Paddub,
        0x19 => Op::Psubub,
        0x1A => Op::Pextub,
        0x1B => Op::Qfsrv,
        _ => Op::Invalid,
    }
}

const fn mmi2(w: u32) -> Op {
    match (w >> 6) & 0x1F {
        0x00 => Op::Pmaddw,
        0x02 => Op::Psllvw,
        0x03 => Op::Psrlvw,
        0x04 => Op::Pmsubw,
        0x08 => Op::Pmfhi,
        0x09 => Op::Pmflo,
        0x0A => Op::Pinth,
        0x0C => Op::Pmultw,
        0x0D => Op::Pdivw,
        0x0E => Op::Pcpyld,
        0x10 => Op::Pmaddh,
        0x11 => Op::Phmadh,
        0x12 => Op::Pand,
        0x13 => Op::Pxor,
        0x14 => Op::Pmsubh,
        0x15 => Op::Phmsbh,
        0x1A => Op::Pexeh,
        0x1B => Op::Prevh,
        0x1C => Op::Pmulth,
        0x1D => Op::Pdivbw,
        0x1E => Op::Pexew,
        0x1F => Op::Prot3w,
        _ => Op::Invalid,
    }
}

const fn mmi3(w: u32) -> Op {
    match (w >> 6) & 0x1F {
        0x00 => Op::Pmadduw,
        0x03 => Op::Psravw,
        0x08 => Op::Pmthi,
        0x09 => Op::Pmtlo,
        0x0A => Op::Pinteh,
        0x0C => Op::Pmultuw,
        0x0D => Op::Pdivuw,
        0x0E => Op::Pcpyud,
        0x12 => Op::Por,
        0x13 => Op::Pnor,
        0x1A => Op::Pexch,
        0x1B => Op::Pcpyh,
        0x1E => Op::Pexcw,
        _ => Op::Invalid,
    }
}

const fn cop2(w: u32) -> Op {
    if w & (1 << 25) != 0 {
        return cop2_macro(w);
    }
    let interlock = w & 1 != 0;
    match (w >> 21) & 0x1F {
        0x01 if interlock => Op::Qmfc2I,
        0x01 => Op::Qmfc2Ni,
        0x02 if interlock => Op::Cfc2I,
        0x02 => Op::Cfc2Ni,
        0x05 if interlock => Op::Qmtc2I,
        0x05 => Op::Qmtc2Ni,
        0x06 if interlock => Op::Ctc2I,
        0x06 => Op::Ctc2Ni,
        0x08 => match (w >> 16) & 0x1F {
            0 => Op::Bc2f,
            1 => Op::Bc2t,
            2 => Op::Bc2fl,
            3 => Op::Bc2tl,
            _ => Op::Invalid,
        },
        _ => Op::Invalid,
    }
}

/// VU0 macro-mode operations (`CO` bit set).
const fn cop2_macro(w: u32) -> Op {
    const BC: [[Op; 4]; 7] = [
        [Op::Vaddx, Op::Vaddy, Op::Vaddz, Op::Vaddw],
        [Op::Vsubx, Op::Vsuby, Op::Vsubz, Op::Vsubw],
        [Op::Vmaddx, Op::Vmaddy, Op::Vmaddz, Op::Vmaddw],
        [Op::Vmsubx, Op::Vmsuby, Op::Vmsubz, Op::Vmsubw],
        [Op::Vmaxx, Op::Vmaxy, Op::Vmaxz, Op::Vmaxw],
        [Op::Vminix, Op::Vminiy, Op::Vminiz, Op::Vminiw],
        [Op::Vmulx, Op::Vmuly, Op::Vmulz, Op::Vmulw],
    ];
    let funct = w & 0x3F;
    match funct {
        0x00..=0x1B => BC[(funct >> 2) as usize][(funct & 3) as usize],
        0x1C => Op::Vmulq,
        0x1D => Op::Vmaxi,
        0x1E => Op::Vmuli,
        0x1F => Op::Vminii,
        0x20 => Op::Vaddq,
        0x21 => Op::Vmaddq,
        0x22 => Op::Vaddi,
        0x23 => Op::Vmaddi,
        0x24 => Op::Vsubq,
        0x25 => Op::Vmsubq,
        0x26 => Op::Vsubi,
        0x27 => Op::Vmsubi,
        0x28 => Op::Vadd,
        0x29 => Op::Vmadd,
        0x2A => Op::Vmul,
        0x2B => Op::Vmax,
        0x2C => Op::Vsub,
        0x2D => Op::Vmsub,
        0x2E => Op::Vopmsub,
        0x2F => Op::Vmini,
        0x30 => Op::Viadd,
        0x31 => Op::Visub,
        0x32 => Op::Viaddi,
        0x34 => Op::Viand,
        0x35 => Op::Vior,
        0x38 => Op::Vcallms,
        0x39 => Op::Vcallmsr,
        0x3C..=0x3F => cop2_special(w),
        _ => Op::Invalid,
    }
}

/// The macro-mode special table, indexed by bits 10..6 and 1..0.
const fn cop2_special(w: u32) -> Op {
    const BC: [[Op; 4]; 5] = [
        [Op::Vaddax, Op::Vadday, Op::Vaddaz, Op::Vaddaw],
        [Op::Vsubax, Op::Vsubay, Op::Vsubaz, Op::Vsubaw],
        [Op::Vmaddax, Op::Vmadday, Op::Vmaddaz, Op::Vmaddaw],
        [Op::Vmsubax, Op::Vmsubay, Op::Vmsubaz, Op::Vmsubaw],
        [Op::Vmulax, Op::Vmulay, Op::Vmulaz, Op::Vmulaw],
    ];
    let index = (((w >> 6) & 0x1F) << 2) | (w & 3);
    match index {
        0x00..=0x0F => BC[(index >> 2) as usize][(index & 3) as usize],
        0x10 => Op::Vitof0,
        0x11 => Op::Vitof4,
        0x12 => Op::Vitof12,
        0x13 => Op::Vitof15,
        0x14 => Op::Vftoi0,
        0x15 => Op::Vftoi4,
        0x16 => Op::Vftoi12,
        0x17 => Op::Vftoi15,
        0x18..=0x1B => BC[4][(index & 3) as usize],
        0x1C => Op::Vmulaq,
        0x1D => Op::Vabs,
        0x1E => Op::Vmulai,
        0x1F => Op::Vclipw,
        0x20 => Op::Vaddaq,
        0x21 => Op::Vmaddaq,
        0x22 => Op::Vaddai,
        0x23 => Op::Vmaddai,
        0x24 => Op::Vsubaq,
        0x25 => Op::Vmsubaq,
        0x26 => Op::Vsubai,
        0x27 => Op::Vmsubai,
        0x28 => Op::Vadda,
        0x29 => Op::Vmadda,
        0x2A => Op::Vmula,
        0x2C => Op::Vsuba,
        0x2D => Op::Vmsuba,
        0x2E => Op::Vopmula,
        0x2F => Op::Vnop,
        0x30 => Op::Vmove,
        0x31 => Op::Vmr32,
        0x34 => Op::Vlqi,
        0x35 => Op::Vsqi,
        0x36 => Op::Vlqd,
        0x37 => Op::Vsqd,
        0x38 => Op::Vdiv,
        0x39 => Op::Vsqrt,
        0x3A => Op::Vrsqrt,
        0x3B => Op::Vwaitq,
        0x3C => Op::Vmtir,
        0x3D => Op::Vmfir,
        0x3E => single_dest(w, [Op::VilwrX, Op::VilwrY, Op::VilwrZ, Op::VilwrW]),
        0x3F => single_dest(w, [Op::ViswrX, Op::ViswrY, Op::ViswrZ, Op::ViswrW]),
        0x40 => Op::Vrnext,
        0x41 => Op::Vrget,
        0x42 => Op::Vrinit,
        0x43 => Op::Vrxor,
        _ => Op::Invalid,
    }
}

/// `vilwr`/`viswr` name their one destination component.
const fn single_dest(w: u32, ops: [Op; 4]) -> Op {
    match (w >> 21) & 0xF {
        0b1000 => ops[0],
        0b0100 => ops[1],
        0b0010 => ops[2],
        0b0001 => ops[3],
        _ => Op::Invalid,
    }
}
