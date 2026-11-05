//! The PlayStation 2's image processing unit, the IPU: an MPEG-2
//! macroblock decoder with colour conversion, fed and drained through two
//! eight-quadword FIFOs by the toIPU and fromIPU DMA channels.
//!
//! This is a port of PCSX2's IPU (`pcsx2/IPU`, GPL-3.0-or-later like
//! sly-rs; parts of it come from libmpeg2). It keeps PCSX2's bitstream
//! reader, integer IDCT and colour arithmetic, and its commands as
//! resumable steps that stop when input runs out or the output FIFO is
//! full and go on when more arrives, so the output matches PCSX2's quadword
//! for quadword. The modules follow these files:
//!
//! - `regs`: register reads and writes and the soft reset (`IPU.cpp`,
//!   `IPU.h`).
//! - `commands`: starting commands and running them: BCLR, SETTH, SETIQ,
//!   SETVQ, CSC, PACK (`IPU.cpp`'s `IPUCMD_WRITE`, `IPU_MultiISA.cpp`'s
//!   `IPUWorker` and the command functions).
//! - `decode`: IDEC, BDEC, VDEC and FDEC, macroblock and coefficient
//!   decoding, inverse quantisation (`IPU_MultiISA.cpp`).
//! - `idct`: the inverse DCT and scan orders (`IPU_MultiISA.cpp`).
//! - `color`: YCbCr to RGB, thresholds, dithering to RGB16 and VQ
//!   (`yuv2rgb.cpp`, `IPUdither.cpp`, `IPU_MultiISA.cpp`), from PCSX2's
//!   reference C versions, which match its SIMD ones.
//! - `bitstream`: the bit pointer and two-quadword bit buffer (`IPU.h`'s
//!   `tIPU_BP`, the bit readers in `IPU_MultiISA.cpp`).
//! - `fifo`: the input and output FIFOs (`IPU_Fifo.cpp`).
//! - `vlc`: the variable-length code tables (`mpeg2_vlc.h`).
//!
//! Left out: PCSX2's logging, savestates and FMV detection, and the DMA
//! channels themselves (`IPUdma.cpp`), which belong to the EE's DMA
//! controller; what they have to do is below.
//!
//! # Driving it
//!
//! Quadwords are 16 bytes as the EE stores them: the stream's first byte is
//! the `u128`'s least significant byte.
//!
//! PCSX2 runs the command in progress (its `IPU_PROCESS` event, here
//! [`Ipu::run`]) when data arrives or output space frees up; there is no
//! clock here, so the caller calls [`Ipu::run`] at those points. To
//! reproduce PCSX2's order of events:
//!
//! - **`IPU_CMD` write**: [`Ipu::write`]. BCLR and SETTH finish there;
//!   SETIQ, SETVQ, VDEC, FDEC, CSC and PACK run as far as the FIFOs allow
//!   there. IDEC and BDEC do not: PCSX2 starts them itself 64 cycles later,
//!   so after writing one the caller calls [`Ipu::run`] once, whatever the
//!   flags below say. (The other delay PCSX2 schedules itself, before each
//!   decoded macroblock is output, happens inside [`Ipu::run`].)
//! - **toIPU DMA** (channel 4, PCSX2's `IPU1dma`): it moves data only while
//!   [`Ipu::data_requested`] holds (from a BCLR or reset until the FIFO is
//!   full, then again once a read finds one quadword or none left). Each
//!   time it runs it pushes as much of the current tag's data as fits
//!   ([`Ipu::in_free`]) with [`Ipu::push_in`], then calls [`Ipu::run`] if
//!   [`Ipu::waiting_on_input`] and the FIFO holds data.
//! - **`IPU_in_FIFO` write** by the CPU: [`Ipu::push_in`], then
//!   [`Ipu::run`] if [`Ipu::busy`].
//! - **fromIPU DMA** (channel 3, `IPU0dma`): keep
//!   [`Ipu::set_out_dma_ready`] up to date (STR set and QWC not zero): IDEC
//!   and BDEC decode a macroblock only while it holds and the output FIFO is
//!   empty. Each time output appears it pops up to QWC quadwords with
//!   [`Ipu::pop_out`], then calls [`Ipu::run`] if [`Ipu::busy`] and
//!   [`Ipu::waiting_on_output`]; with nothing to pop it calls [`Ipu::run`]
//!   if [`Ipu::waiting_on_output`] (as when the channel is started).
//! - **`IPU_out_FIFO` read** by the CPU: [`Ipu::pop_out`] of one quadword.
//!   PCSX2 does not resume the IPU for this; calling [`Ipu::run`] after it
//!   anyway is harmless for the data.
//! - After every write and run, [`Ipu::take_irq`]: when it returns true,
//!   raise the INTC's IPU interrupt (bit 8).
//!
//! A simpler loop that gives the same data (and only shifts when the busy
//! bit falls relative to the last drain) is: feed what fits while
//! requested, [`Ipu::run`], drain, [`Ipu::run`], repeat while either moved
//! something.

// The IPU works on bytes, 16-bit samples and 32-bit registers held in
// wider integers, as PCSX2 does; the casts are its truncations.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "bytes, samples and registers in wider integers, as in PCSX2"
)]

mod bitstream;
mod color;
mod commands;
mod decode;
mod fifo;
mod idct;
mod regs;
mod vlc;

#[cfg(test)]
mod tests;

use bitstream::Bitstream;
use commands::Cmd;
use decode::Decoder;
use fifo::OutFifo;
use regs::{CTRL, CTRL_BUSY, TOP};

/// Address of `IPU_CMD`.
pub const IPU_CMD: u32 = 0x1000_2000;
/// Address of `IPU_CTRL`.
pub const IPU_CTRL: u32 = 0x1000_2010;
/// Address of `IPU_BP`.
pub const IPU_BP: u32 = 0x1000_2020;
/// Address of `IPU_TOP`.
pub const IPU_TOP: u32 = 0x1000_2030;

/// The IPU: registers, FIFOs, decoder state and the command in progress.
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools, reason = "PCSX2's status flags")]
pub struct Ipu {
    /// The register page (`ipuRegs`), as 32-bit words.
    regs: [u32; 64],
    bs: Bitstream,
    out: OutFifo,
    cmd: Cmd,
    dec: Decoder,
    /// SETVQ's colour table (`g_ipu_vqclut`).
    vqclut: [u16; 16],
    /// SETTH's thresholds (`g_ipu_thresh`).
    thresh: [u16; 2],
    /// The last macroblock's coded block pattern (`coded_block_pattern`).
    cbp: i32,
    /// The last AC code looked up (PCSX2's static `tab`), kept across a
    /// stall between a code and its level.
    tab: vlc::Dct,
    /// Skipped macroblocks counted in IDEC (`mbaCount`).
    mba_count: i32,
    /// The yields before IDEC's and BDEC's output (`ready_to_decode`).
    ready_idec: bool,
    ready_bdec: bool,
    /// The command stopped for lack of output room or a ready fromIPU
    /// channel (`IPUCoreStatus.WaitingOnIPUFrom`).
    waiting_out: bool,
    /// The command yielded and asked to be run again.
    process_pending: bool,
    irq: bool,
    out_dma_ready: bool,
}

impl Default for Ipu {
    fn default() -> Self {
        Self::new()
    }
}

impl Ipu {
    /// An IPU after reset (`ipuReset`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            regs: [0; 64],
            bs: Bitstream::default(),
            out: OutFifo::default(),
            cmd: Cmd::default(),
            dec: Decoder::default(),
            vqclut: [0; 16],
            thresh: [0; 2],
            cbp: 0,
            tab: vlc::Dct::default(),
            mba_count: 0,
            ready_idec: true,
            ready_bdec: true,
            waiting_out: false,
            process_pending: false,
            irq: false,
            out_dma_ready: true,
        }
    }

    /// A 32- or 64-bit read of an IPU register: `address` is `0x1000_2000`
    /// (`IPU_CMD`), `0x1000_2010` (`IPU_CTRL`), `0x1000_2020` (`IPU_BP`),
    /// `0x1000_2030` (`IPU_TOP`); returns 64 bits (the caller takes the low
    /// word for 32-bit reads).
    ///
    /// The upper words are `IPU_CMD`'s and `IPU_TOP`'s busy bits. Reading
    /// `IPU_CMD` while no VDEC or FDEC result is there loads and shows the
    /// next 32 bits of the stream. `IPU_CTRL` and `IPU_BP` are refreshed as
    /// for PCSX2's 32-bit reads (its 64-bit reads show them as last
    /// refreshed).
    pub fn read(&mut self, address: u32) -> u64 {
        self.read_reg(address)
    }

    /// A write to `IPU_CMD` (starts a command) or `IPU_CTRL` (reset bit,
    /// etc.). Only the low 32 bits count.
    pub fn write(&mut self, address: u32, value: u64) {
        self.write_reg(address, value);
    }

    /// Room in the input FIFO, in quadwords (8 at most).
    #[must_use]
    pub fn in_free(&self) -> usize {
        fifo::FIFO_QWS - self.bs.fifo.count
    }

    /// Quadwords into the input FIFO (`IPU_in_FIFO` writes or toIPU DMA):
    /// at most `in_free()`; the rest are dropped.
    pub fn push_in(&mut self, qws: &[u128]) {
        self.bs.fifo.write(qws);
    }

    /// Quadwords waiting in the output FIFO.
    #[must_use]
    pub fn out_len(&self) -> usize {
        self.out.count
    }

    /// Takes up to `n` quadwords from the output FIFO (`IPU_out_FIFO` reads
    /// or fromIPU DMA).
    pub fn pop_out(&mut self, n: usize) -> Vec<u128> {
        self.out.read(n)
    }

    /// Runs the current command as far as its input and output allow;
    /// returns whether it made progress. The caller calls it after feeding
    /// input or draining output.
    pub fn run(&mut self) -> bool {
        if !self.busy() {
            return false;
        }
        let before = self.progress_key();
        loop {
            self.bs.waiting = false;
            self.waiting_out = false;
            self.process_pending = false;
            self.worker();
            if !(self.busy() && self.process_pending) {
                break;
            }
        }
        self.progress_key() != before
    }

    /// Whether a command finished and the IPU's interrupt is to be raised
    /// (taken: cleared on return).
    pub fn take_irq(&mut self) -> bool {
        std::mem::take(&mut self.irq)
    }

    /// Whether a command is in progress (`IPU_CTRL`'s busy bit).
    #[must_use]
    pub fn busy(&self) -> bool {
        self.ctrl_flag(CTRL_BUSY)
    }

    /// Whether the IPU wants input: the toIPU DMA moves data only while
    /// this holds (PCSX2's `IPUCoreStatus.DataRequested`).
    #[must_use]
    pub fn data_requested(&self) -> bool {
        self.bs.fifo.data_requested
    }

    /// Whether the command stopped on an empty input FIFO
    /// (`WaitingOnIPUTo`).
    #[must_use]
    pub fn waiting_on_input(&self) -> bool {
        self.bs.waiting
    }

    /// Whether the command stopped for output room or for the fromIPU
    /// channel (`WaitingOnIPUFrom`).
    #[must_use]
    pub fn waiting_on_output(&self) -> bool {
        self.waiting_out
    }

    /// Whether the fromIPU DMA channel is running with quadwords left to
    /// move (STR set, QWC not zero). IDEC and BDEC wait for it before each
    /// macroblock, as PCSX2's do. True after reset.
    pub fn set_out_dma_ready(&mut self, ready: bool) {
        self.out_dma_ready = ready;
    }

    /// What changes when a command gets anywhere.
    fn progress_key(&self) -> impl PartialEq + use<> {
        (
            (self.bs.bp, self.bs.fp, self.bs.fifo.count, self.out.count),
            (self.cmd.pos, self.cmd.index),
            (self.ready_idec, self.ready_bdec),
            (self.regs[CTRL], self.regs[TOP], self.dec.out_left),
        )
    }
}
