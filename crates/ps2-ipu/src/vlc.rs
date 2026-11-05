//! The MPEG-2 variable-length code tables, from PCSX2's
//! `pcsx2/IPU/mpeg2_vlc.h` (which takes them from libmpeg2; GPL like
//! sly-rs). Generated from the header by a script, not retyped.
//!
//! Each entry is what a lookup on the code's leading bits gives: the
//! decoded value and the code's length in bits.

/// A macroblock type: its mode bits and code length (PCSX2's `MBtab`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mb {
    pub(crate) modes: u8,
    pub(crate) len: u8,
}

/// A motion code's magnitude less one, and its length (`MVtab`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mv {
    pub(crate) delta: u8,
    pub(crate) len: u8,
}

/// A dual-prime vector, and its length (`DMVtab`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Dmv {
    pub(crate) dmv: i8,
    pub(crate) len: u8,
}

/// A coded block pattern, and its length (`CBPtab`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Cbp {
    pub(crate) cbp: u8,
    pub(crate) len: u8,
}

/// A DC coefficient's size in bits, and the size code's length (`DCtab`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Dc {
    pub(crate) size: u8,
    pub(crate) len: u8,
}

/// A macroblock address increment less one, and its length (`MBAtab`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mba {
    pub(crate) mba: u8,
    pub(crate) len: u8,
}

/// A DCT coefficient: the run of zeros before it (64 is end of block, 65
/// escape), its level and the code's length (`DCTtab`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Dct {
    pub(crate) run: u8,
    pub(crate) level: u8,
    pub(crate) len: u8,
}

const fn mb(modes: u8, len: u8) -> Mb {
    Mb { modes, len }
}
const fn mv(delta: u8, len: u8) -> Mv {
    Mv { delta, len }
}
const fn dmv(dmv: i8, len: u8) -> Dmv {
    Dmv { dmv, len }
}
const fn cbp(cbp: u8, len: u8) -> Cbp {
    Cbp { cbp, len }
}
const fn dc(size: u8, len: u8) -> Dc {
    Dc { size, len }
}
const fn mba(mba: u8, len: u8) -> Mba {
    Mba { mba, len }
}
const fn dct(run: u8, level: u8, len: u8) -> Dct {
    Dct { run, level, len }
}

#[rustfmt::skip]
pub(crate) const MB_I: [Mb; 2] = [
    mb(17, 2), mb(1, 1),
];

#[rustfmt::skip]
pub(crate) const MB_P: [Mb; 32] = [
    mb(17, 6), mb(18, 5), mb(26, 5), mb(1, 5),
    mb(8, 3), mb(8, 3), mb(8, 3), mb(8, 3),
    mb(2, 2), mb(2, 2), mb(2, 2), mb(2, 2),
    mb(2, 2), mb(2, 2), mb(2, 2), mb(2, 2),
    mb(10, 1), mb(10, 1), mb(10, 1), mb(10, 1),
    mb(10, 1), mb(10, 1), mb(10, 1), mb(10, 1),
    mb(10, 1), mb(10, 1), mb(10, 1), mb(10, 1),
    mb(10, 1), mb(10, 1), mb(10, 1), mb(10, 1),
];

#[rustfmt::skip]
pub(crate) const MB_B: [Mb; 64] = [
    mb(0, 0), mb(17, 6), mb(22, 6), mb(26, 6),
    mb(30, 5), mb(30, 5), mb(1, 5), mb(1, 5),
    mb(8, 4), mb(8, 4), mb(8, 4), mb(8, 4),
    mb(10, 4), mb(10, 4), mb(10, 4), mb(10, 4),
    mb(4, 3), mb(4, 3), mb(4, 3), mb(4, 3),
    mb(4, 3), mb(4, 3), mb(4, 3), mb(4, 3),
    mb(6, 3), mb(6, 3), mb(6, 3), mb(6, 3),
    mb(6, 3), mb(6, 3), mb(6, 3), mb(6, 3),
    mb(12, 2), mb(12, 2), mb(12, 2), mb(12, 2),
    mb(12, 2), mb(12, 2), mb(12, 2), mb(12, 2),
    mb(12, 2), mb(12, 2), mb(12, 2), mb(12, 2),
    mb(12, 2), mb(12, 2), mb(12, 2), mb(12, 2),
    mb(14, 2), mb(14, 2), mb(14, 2), mb(14, 2),
    mb(14, 2), mb(14, 2), mb(14, 2), mb(14, 2),
    mb(14, 2), mb(14, 2), mb(14, 2), mb(14, 2),
    mb(14, 2), mb(14, 2), mb(14, 2), mb(14, 2),
];

#[rustfmt::skip]
pub(crate) const MV_4: [Mv; 8] = [
    mv(3, 6), mv(2, 4), mv(1, 3), mv(1, 3),
    mv(0, 2), mv(0, 2), mv(0, 2), mv(0, 2),
];

#[rustfmt::skip]
pub(crate) const MV_10: [Mv; 48] = [
    mv(0, 10), mv(0, 10), mv(0, 10), mv(0, 10),
    mv(0, 10), mv(0, 10), mv(0, 10), mv(0, 10),
    mv(0, 10), mv(0, 10), mv(0, 10), mv(0, 10),
    mv(15, 10), mv(14, 10), mv(13, 10), mv(12, 10),
    mv(11, 10), mv(10, 10), mv(9, 9), mv(9, 9),
    mv(8, 9), mv(8, 9), mv(7, 9), mv(7, 9),
    mv(6, 7), mv(6, 7), mv(6, 7), mv(6, 7),
    mv(6, 7), mv(6, 7), mv(6, 7), mv(6, 7),
    mv(5, 7), mv(5, 7), mv(5, 7), mv(5, 7),
    mv(5, 7), mv(5, 7), mv(5, 7), mv(5, 7),
    mv(4, 7), mv(4, 7), mv(4, 7), mv(4, 7),
    mv(4, 7), mv(4, 7), mv(4, 7), mv(4, 7),
];

#[rustfmt::skip]
pub(crate) const DMV_2: [Dmv; 4] = [
    dmv(0, 1), dmv(0, 1), dmv(1, 2), dmv(-1, 2),
];

#[rustfmt::skip]
pub(crate) const CBP_7: [Cbp; 112] = [
    cbp(34, 7), cbp(18, 7), cbp(10, 7), cbp(6, 7),
    cbp(33, 7), cbp(17, 7), cbp(9, 7), cbp(5, 7),
    cbp(63, 6), cbp(63, 6), cbp(3, 6), cbp(3, 6),
    cbp(36, 6), cbp(36, 6), cbp(24, 6), cbp(24, 6),
    cbp(62, 5), cbp(62, 5), cbp(62, 5), cbp(62, 5),
    cbp(2, 5), cbp(2, 5), cbp(2, 5), cbp(2, 5),
    cbp(61, 5), cbp(61, 5), cbp(61, 5), cbp(61, 5),
    cbp(1, 5), cbp(1, 5), cbp(1, 5), cbp(1, 5),
    cbp(56, 5), cbp(56, 5), cbp(56, 5), cbp(56, 5),
    cbp(52, 5), cbp(52, 5), cbp(52, 5), cbp(52, 5),
    cbp(44, 5), cbp(44, 5), cbp(44, 5), cbp(44, 5),
    cbp(28, 5), cbp(28, 5), cbp(28, 5), cbp(28, 5),
    cbp(40, 5), cbp(40, 5), cbp(40, 5), cbp(40, 5),
    cbp(20, 5), cbp(20, 5), cbp(20, 5), cbp(20, 5),
    cbp(48, 5), cbp(48, 5), cbp(48, 5), cbp(48, 5),
    cbp(12, 5), cbp(12, 5), cbp(12, 5), cbp(12, 5),
    cbp(32, 4), cbp(32, 4), cbp(32, 4), cbp(32, 4),
    cbp(32, 4), cbp(32, 4), cbp(32, 4), cbp(32, 4),
    cbp(16, 4), cbp(16, 4), cbp(16, 4), cbp(16, 4),
    cbp(16, 4), cbp(16, 4), cbp(16, 4), cbp(16, 4),
    cbp(8, 4), cbp(8, 4), cbp(8, 4), cbp(8, 4),
    cbp(8, 4), cbp(8, 4), cbp(8, 4), cbp(8, 4),
    cbp(4, 4), cbp(4, 4), cbp(4, 4), cbp(4, 4),
    cbp(4, 4), cbp(4, 4), cbp(4, 4), cbp(4, 4),
    cbp(60, 3), cbp(60, 3), cbp(60, 3), cbp(60, 3),
    cbp(60, 3), cbp(60, 3), cbp(60, 3), cbp(60, 3),
    cbp(60, 3), cbp(60, 3), cbp(60, 3), cbp(60, 3),
    cbp(60, 3), cbp(60, 3), cbp(60, 3), cbp(60, 3),
];

#[rustfmt::skip]
pub(crate) const CBP_9: [Cbp; 64] = [
    cbp(0, 0), cbp(0, 9), cbp(39, 9), cbp(27, 9),
    cbp(59, 9), cbp(55, 9), cbp(47, 9), cbp(31, 9),
    cbp(58, 8), cbp(58, 8), cbp(54, 8), cbp(54, 8),
    cbp(46, 8), cbp(46, 8), cbp(30, 8), cbp(30, 8),
    cbp(57, 8), cbp(57, 8), cbp(53, 8), cbp(53, 8),
    cbp(45, 8), cbp(45, 8), cbp(29, 8), cbp(29, 8),
    cbp(38, 8), cbp(38, 8), cbp(26, 8), cbp(26, 8),
    cbp(37, 8), cbp(37, 8), cbp(25, 8), cbp(25, 8),
    cbp(43, 8), cbp(43, 8), cbp(23, 8), cbp(23, 8),
    cbp(51, 8), cbp(51, 8), cbp(15, 8), cbp(15, 8),
    cbp(42, 8), cbp(42, 8), cbp(22, 8), cbp(22, 8),
    cbp(50, 8), cbp(50, 8), cbp(14, 8), cbp(14, 8),
    cbp(41, 8), cbp(41, 8), cbp(21, 8), cbp(21, 8),
    cbp(49, 8), cbp(49, 8), cbp(13, 8), cbp(13, 8),
    cbp(35, 8), cbp(35, 8), cbp(19, 8), cbp(19, 8),
    cbp(11, 8), cbp(11, 8), cbp(7, 8), cbp(7, 8),
];

#[rustfmt::skip]
pub(crate) const MBA_5: [Mba; 30] = [
    mba(6, 5), mba(5, 5), mba(4, 4), mba(4, 4),
    mba(3, 4), mba(3, 4), mba(2, 3), mba(2, 3),
    mba(2, 3), mba(2, 3), mba(1, 3), mba(1, 3),
    mba(1, 3), mba(1, 3), mba(0, 1), mba(0, 1),
    mba(0, 1), mba(0, 1), mba(0, 1), mba(0, 1),
    mba(0, 1), mba(0, 1), mba(0, 1), mba(0, 1),
    mba(0, 1), mba(0, 1), mba(0, 1), mba(0, 1),
    mba(0, 1), mba(0, 1),
];

#[rustfmt::skip]
pub(crate) const MBA_11: [Mba; 104] = [
    mba(32, 11), mba(31, 11), mba(30, 11), mba(29, 11),
    mba(28, 11), mba(27, 11), mba(26, 11), mba(25, 11),
    mba(24, 11), mba(23, 11), mba(22, 11), mba(21, 11),
    mba(20, 10), mba(20, 10), mba(19, 10), mba(19, 10),
    mba(18, 10), mba(18, 10), mba(17, 10), mba(17, 10),
    mba(16, 10), mba(16, 10), mba(15, 10), mba(15, 10),
    mba(14, 8), mba(14, 8), mba(14, 8), mba(14, 8),
    mba(14, 8), mba(14, 8), mba(14, 8), mba(14, 8),
    mba(13, 8), mba(13, 8), mba(13, 8), mba(13, 8),
    mba(13, 8), mba(13, 8), mba(13, 8), mba(13, 8),
    mba(12, 8), mba(12, 8), mba(12, 8), mba(12, 8),
    mba(12, 8), mba(12, 8), mba(12, 8), mba(12, 8),
    mba(11, 8), mba(11, 8), mba(11, 8), mba(11, 8),
    mba(11, 8), mba(11, 8), mba(11, 8), mba(11, 8),
    mba(10, 8), mba(10, 8), mba(10, 8), mba(10, 8),
    mba(10, 8), mba(10, 8), mba(10, 8), mba(10, 8),
    mba(9, 8), mba(9, 8), mba(9, 8), mba(9, 8),
    mba(9, 8), mba(9, 8), mba(9, 8), mba(9, 8),
    mba(8, 7), mba(8, 7), mba(8, 7), mba(8, 7),
    mba(8, 7), mba(8, 7), mba(8, 7), mba(8, 7),
    mba(8, 7), mba(8, 7), mba(8, 7), mba(8, 7),
    mba(8, 7), mba(8, 7), mba(8, 7), mba(8, 7),
    mba(7, 7), mba(7, 7), mba(7, 7), mba(7, 7),
    mba(7, 7), mba(7, 7), mba(7, 7), mba(7, 7),
    mba(7, 7), mba(7, 7), mba(7, 7), mba(7, 7),
    mba(7, 7), mba(7, 7), mba(7, 7), mba(7, 7),
];

#[rustfmt::skip]
pub(crate) const DC_LUM0: [Dc; 32] = [
    dc(1, 2), dc(1, 2), dc(1, 2), dc(1, 2),
    dc(1, 2), dc(1, 2), dc(1, 2), dc(1, 2),
    dc(2, 2), dc(2, 2), dc(2, 2), dc(2, 2),
    dc(2, 2), dc(2, 2), dc(2, 2), dc(2, 2),
    dc(0, 3), dc(0, 3), dc(0, 3), dc(0, 3),
    dc(3, 3), dc(3, 3), dc(3, 3), dc(3, 3),
    dc(4, 3), dc(4, 3), dc(4, 3), dc(4, 3),
    dc(5, 4), dc(5, 4), dc(6, 5), dc(0, 0),
];

#[rustfmt::skip]
pub(crate) const DC_LUM1: [Dc; 16] = [
    dc(7, 6), dc(7, 6), dc(7, 6), dc(7, 6),
    dc(7, 6), dc(7, 6), dc(7, 6), dc(7, 6),
    dc(8, 7), dc(8, 7), dc(8, 7), dc(8, 7),
    dc(9, 8), dc(9, 8), dc(10, 9), dc(11, 9),
];

#[rustfmt::skip]
pub(crate) const DC_CHROM0: [Dc; 32] = [
    dc(0, 2), dc(0, 2), dc(0, 2), dc(0, 2),
    dc(0, 2), dc(0, 2), dc(0, 2), dc(0, 2),
    dc(1, 2), dc(1, 2), dc(1, 2), dc(1, 2),
    dc(1, 2), dc(1, 2), dc(1, 2), dc(1, 2),
    dc(2, 2), dc(2, 2), dc(2, 2), dc(2, 2),
    dc(2, 2), dc(2, 2), dc(2, 2), dc(2, 2),
    dc(3, 3), dc(3, 3), dc(3, 3), dc(3, 3),
    dc(4, 4), dc(4, 4), dc(5, 5), dc(0, 0),
];

#[rustfmt::skip]
pub(crate) const DC_CHROM1: [Dc; 32] = [
    dc(6, 6), dc(6, 6), dc(6, 6), dc(6, 6),
    dc(6, 6), dc(6, 6), dc(6, 6), dc(6, 6),
    dc(6, 6), dc(6, 6), dc(6, 6), dc(6, 6),
    dc(6, 6), dc(6, 6), dc(6, 6), dc(6, 6),
    dc(7, 7), dc(7, 7), dc(7, 7), dc(7, 7),
    dc(7, 7), dc(7, 7), dc(7, 7), dc(7, 7),
    dc(8, 8), dc(8, 8), dc(8, 8), dc(8, 8),
    dc(9, 9), dc(9, 9), dc(10, 10), dc(11, 10),
];

#[rustfmt::skip]
pub(crate) const DCT_FIRST: [Dct; 12] = [
    dct(0, 2, 4), dct(2, 1, 4), dct(1, 1, 3), dct(1, 1, 3),
    dct(0, 1, 1), dct(0, 1, 1), dct(0, 1, 1), dct(0, 1, 1),
    dct(0, 1, 1), dct(0, 1, 1), dct(0, 1, 1), dct(0, 1, 1),
];

#[rustfmt::skip]
pub(crate) const DCT_NEXT: [Dct; 12] = [
    dct(0, 2, 4), dct(2, 1, 4), dct(1, 1, 3), dct(1, 1, 3),
    dct(64, 0, 2), dct(64, 0, 2), dct(64, 0, 2), dct(64, 0, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
];

#[rustfmt::skip]
pub(crate) const DCT_TAB0: [Dct; 60] = [
    dct(65, 0, 6), dct(65, 0, 6), dct(65, 0, 6), dct(65, 0, 6),
    dct(2, 2, 7), dct(2, 2, 7), dct(9, 1, 7), dct(9, 1, 7),
    dct(0, 4, 7), dct(0, 4, 7), dct(8, 1, 7), dct(8, 1, 7),
    dct(7, 1, 6), dct(7, 1, 6), dct(7, 1, 6), dct(7, 1, 6),
    dct(6, 1, 6), dct(6, 1, 6), dct(6, 1, 6), dct(6, 1, 6),
    dct(1, 2, 6), dct(1, 2, 6), dct(1, 2, 6), dct(1, 2, 6),
    dct(5, 1, 6), dct(5, 1, 6), dct(5, 1, 6), dct(5, 1, 6),
    dct(13, 1, 8), dct(0, 6, 8), dct(12, 1, 8), dct(11, 1, 8),
    dct(3, 2, 8), dct(1, 3, 8), dct(0, 5, 8), dct(10, 1, 8),
    dct(0, 3, 5), dct(0, 3, 5), dct(0, 3, 5), dct(0, 3, 5),
    dct(0, 3, 5), dct(0, 3, 5), dct(0, 3, 5), dct(0, 3, 5),
    dct(4, 1, 5), dct(4, 1, 5), dct(4, 1, 5), dct(4, 1, 5),
    dct(4, 1, 5), dct(4, 1, 5), dct(4, 1, 5), dct(4, 1, 5),
    dct(3, 1, 5), dct(3, 1, 5), dct(3, 1, 5), dct(3, 1, 5),
    dct(3, 1, 5), dct(3, 1, 5), dct(3, 1, 5), dct(3, 1, 5),
];

#[rustfmt::skip]
pub(crate) const DCT_TAB0A: [Dct; 252] = [
    dct(65, 0, 6), dct(65, 0, 6), dct(65, 0, 6), dct(65, 0, 6),
    dct(7, 1, 7), dct(7, 1, 7), dct(8, 1, 7), dct(8, 1, 7),
    dct(6, 1, 7), dct(6, 1, 7), dct(2, 2, 7), dct(2, 2, 7),
    dct(0, 7, 6), dct(0, 7, 6), dct(0, 7, 6), dct(0, 7, 6),
    dct(0, 6, 6), dct(0, 6, 6), dct(0, 6, 6), dct(0, 6, 6),
    dct(4, 1, 6), dct(4, 1, 6), dct(4, 1, 6), dct(4, 1, 6),
    dct(5, 1, 6), dct(5, 1, 6), dct(5, 1, 6), dct(5, 1, 6),
    dct(1, 5, 8), dct(11, 1, 8), dct(0, 11, 8), dct(0, 10, 8),
    dct(13, 1, 8), dct(12, 1, 8), dct(3, 2, 8), dct(1, 4, 8),
    dct(2, 1, 5), dct(2, 1, 5), dct(2, 1, 5), dct(2, 1, 5),
    dct(2, 1, 5), dct(2, 1, 5), dct(2, 1, 5), dct(2, 1, 5),
    dct(1, 2, 5), dct(1, 2, 5), dct(1, 2, 5), dct(1, 2, 5),
    dct(1, 2, 5), dct(1, 2, 5), dct(1, 2, 5), dct(1, 2, 5),
    dct(3, 1, 5), dct(3, 1, 5), dct(3, 1, 5), dct(3, 1, 5),
    dct(3, 1, 5), dct(3, 1, 5), dct(3, 1, 5), dct(3, 1, 5),
    dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3),
    dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3),
    dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3),
    dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3),
    dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3),
    dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3),
    dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3),
    dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3), dct(1, 1, 3),
    dct(64, 0, 4), dct(64, 0, 4), dct(64, 0, 4), dct(64, 0, 4),
    dct(64, 0, 4), dct(64, 0, 4), dct(64, 0, 4), dct(64, 0, 4),
    dct(64, 0, 4), dct(64, 0, 4), dct(64, 0, 4), dct(64, 0, 4),
    dct(64, 0, 4), dct(64, 0, 4), dct(64, 0, 4), dct(64, 0, 4),
    dct(0, 3, 4), dct(0, 3, 4), dct(0, 3, 4), dct(0, 3, 4),
    dct(0, 3, 4), dct(0, 3, 4), dct(0, 3, 4), dct(0, 3, 4),
    dct(0, 3, 4), dct(0, 3, 4), dct(0, 3, 4), dct(0, 3, 4),
    dct(0, 3, 4), dct(0, 3, 4), dct(0, 3, 4), dct(0, 3, 4),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2), dct(0, 1, 2),
    dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3),
    dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3),
    dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3),
    dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3),
    dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3),
    dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3),
    dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3),
    dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3), dct(0, 2, 3),
    dct(0, 4, 5), dct(0, 4, 5), dct(0, 4, 5), dct(0, 4, 5),
    dct(0, 4, 5), dct(0, 4, 5), dct(0, 4, 5), dct(0, 4, 5),
    dct(0, 5, 5), dct(0, 5, 5), dct(0, 5, 5), dct(0, 5, 5),
    dct(0, 5, 5), dct(0, 5, 5), dct(0, 5, 5), dct(0, 5, 5),
    dct(9, 1, 7), dct(9, 1, 7), dct(1, 3, 7), dct(1, 3, 7),
    dct(10, 1, 7), dct(10, 1, 7), dct(0, 8, 7), dct(0, 8, 7),
    dct(0, 9, 7), dct(0, 9, 7), dct(0, 12, 8), dct(0, 13, 8),
    dct(2, 3, 8), dct(4, 2, 8), dct(0, 14, 8), dct(0, 15, 8),
];

#[rustfmt::skip]
pub(crate) const DCT_TAB1: [Dct; 8] = [
    dct(16, 1, 10), dct(5, 2, 10), dct(0, 7, 10), dct(2, 3, 10),
    dct(1, 4, 10), dct(15, 1, 10), dct(14, 1, 10), dct(4, 2, 10),
];

#[rustfmt::skip]
pub(crate) const DCT_TAB1A: [Dct; 8] = [
    dct(5, 2, 9), dct(5, 2, 9), dct(14, 1, 9), dct(14, 1, 9),
    dct(2, 4, 10), dct(16, 1, 10), dct(15, 1, 9), dct(15, 1, 9),
];

#[rustfmt::skip]
pub(crate) const DCT_TAB2: [Dct; 16] = [
    dct(0, 11, 12), dct(8, 2, 12), dct(4, 3, 12), dct(0, 10, 12),
    dct(2, 4, 12), dct(7, 2, 12), dct(21, 1, 12), dct(20, 1, 12),
    dct(0, 9, 12), dct(19, 1, 12), dct(18, 1, 12), dct(1, 5, 12),
    dct(3, 3, 12), dct(0, 8, 12), dct(6, 2, 12), dct(17, 1, 12),
];

#[rustfmt::skip]
pub(crate) const DCT_TAB3: [Dct; 16] = [
    dct(10, 2, 13), dct(9, 2, 13), dct(5, 3, 13), dct(3, 4, 13),
    dct(2, 5, 13), dct(1, 7, 13), dct(1, 6, 13), dct(0, 15, 13),
    dct(0, 14, 13), dct(0, 13, 13), dct(0, 12, 13), dct(26, 1, 13),
    dct(25, 1, 13), dct(24, 1, 13), dct(23, 1, 13), dct(22, 1, 13),
];

#[rustfmt::skip]
pub(crate) const DCT_TAB4: [Dct; 16] = [
    dct(0, 31, 14), dct(0, 30, 14), dct(0, 29, 14), dct(0, 28, 14),
    dct(0, 27, 14), dct(0, 26, 14), dct(0, 25, 14), dct(0, 24, 14),
    dct(0, 23, 14), dct(0, 22, 14), dct(0, 21, 14), dct(0, 20, 14),
    dct(0, 19, 14), dct(0, 18, 14), dct(0, 17, 14), dct(0, 16, 14),
];

#[rustfmt::skip]
pub(crate) const DCT_TAB5: [Dct; 16] = [
    dct(0, 40, 15), dct(0, 39, 15), dct(0, 38, 15), dct(0, 37, 15),
    dct(0, 36, 15), dct(0, 35, 15), dct(0, 34, 15), dct(0, 33, 15),
    dct(0, 32, 15), dct(1, 14, 15), dct(1, 13, 15), dct(1, 12, 15),
    dct(1, 11, 15), dct(1, 10, 15), dct(1, 9, 15), dct(1, 8, 15),
];

#[rustfmt::skip]
pub(crate) const DCT_TAB6: [Dct; 16] = [
    dct(1, 18, 16), dct(1, 17, 16), dct(1, 16, 16), dct(1, 15, 16),
    dct(6, 3, 16), dct(16, 2, 16), dct(15, 2, 16), dct(14, 2, 16),
    dct(13, 2, 16), dct(12, 2, 16), dct(11, 2, 16), dct(31, 1, 16),
    dct(30, 1, 16), dct(29, 1, 16), dct(28, 1, 16), dct(27, 1, 16),
];
