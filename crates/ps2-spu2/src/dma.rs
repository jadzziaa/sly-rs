//! DMA and `AutoDMA` transfers, as PCSX2's `pcsx2/SPU2/Dma.cpp` does them,
//! and the cores' input buffers (`ReadInput.cpp`).
//!
//! PCSX2 copies a plain transfer in pieces as its DMA counter
//! (`DMAICounter`, 24 IOP cycles per halfword) runs down, and raises the
//! interrupts a piece owes for passing IRQA when the counter runs out.
//! Here the caller times the transfer: the data moves when it is handed
//! over, piece by piece as PCSX2 cuts it (so the IRQ tests see the same
//! boundaries), and the owed interrupts are raised when the caller says the
//! transfer has finished ([`Spu2::set_dma_busy`]). The IOP-side MADR
//! bookkeeping PCSX2 does here is the caller's.

use crate::mixer::Stereo;
use crate::{ADDR_MASK, Spu2};

/// Halfwords per side in one `AutoDMA` block.
const ADMA_BLOCK: u32 = 0x100;

impl Spu2 {
    /// PCSX2's `DoDMAwrite`.
    pub(crate) fn do_dma_write(&mut self, core: usize, data: &[u16]) {
        if data.len() < 2 {
            // PCSX2 moves nothing and completes after one word's time.
            self.cores[core].statx &= !0x80;
            return;
        }
        let c = &mut self.cores[core];
        c.active_tsa = c.tsa & ADDR_MASK;
        let bit = (core + 1) as u16;
        if c.auto_dma_ctrl & bit == bit {
            self.start_adma_write(core, data);
        } else {
            self.plain_dma_write(core, data);
        }
    }

    /// PCSX2's `StartADMAWrite`: the block waits in the core and moves into
    /// the input buffer as the output asks for it.
    fn start_adma_write(&mut self, core: usize, data: &[u16]) {
        let c = &mut self.cores[core];
        c.input_data_progress = 0;
        c.adma_data = data.to_vec();
        let size = u32::try_from(data.len()).unwrap_or(u32::MAX);
        if size >= 256 {
            c.input_data_left = size;
            if c.input_pos_write != 0xFFFF {
                self.auto_dma_read_buffer(core);
            }
            self.cores[core].adma_in_progress = true;
        } else {
            // "ADMA Error Size too small": PCSX2 completes it on its DMA
            // counter.
            c.input_data_left = 0;
        }
    }

    /// PCSX2's `PlainDMAWrite` and `FinishDMAwrite`, every piece at once.
    fn plain_dma_write(&mut self, core: usize, data: &[u16]) {
        {
            let c = &mut self.cores[core];
            c.statx = (c.statx & !0x80) | 0x400;
        }
        let mut pos = 0_usize;
        let mut read_size = u32::try_from(data.len()).unwrap_or(u32::MAX);
        while read_size > 0 {
            let piece = read_size.min(0x100 + read_size / 24);
            let start = self.cores[core].active_tsa;
            let (tda, moved) = self.transfer_piece(start, piece, |spu, at, n| {
                for i in 0..n {
                    let value = data.get(pos + at + i).copied().unwrap_or(0);
                    spu.set_mem(spu_addr(start, at + i), value);
                }
            });
            pos += moved as usize;
            read_size -= moved;
            let c = &mut self.cores[core];
            c.active_tsa = tda & ADDR_MASK;
            c.tsa = c.active_tsa;
        }
    }

    /// PCSX2's `DoDMAread` and `FinishDMAread`, every piece at once.
    pub(crate) fn do_dma_read(&mut self, core: usize, halfwords: usize) -> Vec<u16> {
        {
            let c = &mut self.cores[core];
            c.active_tsa = c.tsa & ADDR_MASK;
            c.statx = (c.statx & !0x80) | 0x400;
        }
        let mut out = Vec::with_capacity(halfwords);
        let mut read_size = u32::try_from(halfwords).unwrap_or(u32::MAX);
        while read_size > 0 {
            // PCSX2 reads each piece when its counter has run out (at 0).
            let piece = read_size.min(0x100);
            let start = self.cores[core].active_tsa;
            let (tda, moved) = self.transfer_piece(start, piece, |spu, at, n| {
                for i in 0..n {
                    out.push(spu.mem(spu_addr(start, at + i)) as u16);
                }
            });
            read_size -= moved;
            let c = &mut self.cores[core];
            c.active_tsa = tda & ADDR_MASK;
            c.tsa = c.active_tsa;
        }
        out
    }

    /// One piece of a transfer from `start`, wrapping at the end of sound
    /// memory; `copy(spu, offset, n)` moves `n` halfwords from `offset`
    /// into the piece. Flags the interrupts owed for an IRQA strictly
    /// inside the piece, on both cores. Returns where it ended and how many
    /// halfwords it moved.
    fn transfer_piece(
        &mut self,
        start: u32,
        piece: u32,
        mut copy: impl FnMut(&mut Self, usize, usize),
    ) -> (u32, u32) {
        let mut buff1end = start + piece;
        let mut buff2end = 0;
        if buff1end > 0x10_0000 {
            buff2end = buff1end - 0x10_0000;
            buff1end = 0x10_0000;
        }
        copy(self, 0, (buff1end - start) as usize);
        if buff2end > 0 {
            copy(self, (buff1end - start) as usize, buff2end as usize);
            let tda = buff2end & ADDR_MASK;
            // The piece wraps, so either end will do.
            for i in 0..2 {
                let irqa = self.cores[i].irqa;
                if self.cores[i].irq_enable && (irqa > start || irqa < tda) {
                    self.cores[i].irq_dma_pending = true;
                }
            }
            (tda, (buff1end - start) + buff2end)
        } else {
            let tda = buff1end;
            for i in 0..2 {
                let irqa = self.cores[i].irqa;
                if self.cores[i].irq_enable && irqa > start && irqa < tda {
                    self.cores[i].irq_dma_pending = true;
                }
            }
            (tda, buff1end - start)
        }
    }

    /// Raises the interrupts transfers owed (PCSX2's `has_to_call_irq_dma`,
    /// checked when a DMA counter runs out).
    pub(crate) fn raise_dma_irqs(&mut self) {
        for i in 0..2 {
            if std::mem::take(&mut self.cores[i].irq_dma_pending) {
                self.set_irq_call(i);
            }
        }
    }

    /// Fills the next half of the input buffer from the `AutoDMA` block,
    /// left side then right (PCSX2's `AutoDMAReadBuffer`, split stereo).
    fn auto_dma_read_buffer(&mut self, core: usize) {
        let c = &mut self.cores[core];
        if c.input_pos_write == 0xFFFF {
            // Not asked for yet.
            return;
        }
        let mut spos = c.input_pos_write & 0x100;
        let mut left_buffer = c.input_pos_write & 0x80 == 0;
        c.auto_dma_ctrl &= 3;

        let mut size = c.input_data_left.min(0x200) as i32;
        if !left_buffer {
            size = size.min(0x100);
        }
        // PCSX2 loops `while (size)`, which never ends for a block that is
        // not a whole number of 0x100 halfwords; this stops instead, and
        // `input_data_left` stops at 0 rather than wrapping.
        while size > 0 {
            if left_buffer {
                spos &= !0x200;
            } else {
                spos |= 0x200;
            }
            let c = &self.cores[core];
            let base = 0x2000 + ((core as u32) << 10) + spos;
            let from = c.input_data_progress as usize;
            for i in 0..ADMA_BLOCK as usize {
                // Past the block's end PCSX2 reads whatever IOP memory
                // follows; zeros here.
                let value = self.cores[core]
                    .adma_data
                    .get(from + i)
                    .copied()
                    .unwrap_or(0);
                self.set_mem(base + i as u32, value);
            }
            let c = &mut self.cores[core];
            c.input_data_transferred = c.input_data_transferred.wrapping_add(0x200);
            c.input_data_left = c.input_data_left.saturating_sub(ADMA_BLOCK);
            c.input_data_progress += ADMA_BLOCK;
            left_buffer = !left_buffer;
            size -= ADMA_BLOCK as i32;
            c.input_pos_write += 0x80;
        }
        let c = &mut self.cores[core];
        if c.input_pos_write & 0x80 == 0 {
            c.input_pos_write = 0xFFFF;
        }
    }

    /// The core's input sample at the output position, and `AutoDMA`'s
    /// refills at each quarter of the buffer (PCSX2's `V_Core::ReadInput`,
    /// in the normal play mode).
    pub(crate) fn read_input(&mut self, core: usize) -> Stereo {
        let read_index = u32::from(self.out_pos);
        let base = 0x2000 + ((core as u32) << 10);

        for i in 0..2 {
            if self.cores[i].irq_enable && base + read_index == self.cores[i].irqa & 0xFFFF_FDFF {
                self.set_irq_call(i);
            }
        }

        let ret = [
            i32::from(self.mem(base + read_index)),
            i32::from(self.mem(base + 0x200 + read_index)),
        ];

        // PCSX2's simulated MADR progress: the IOP transfer completes when
        // it has run out with nothing left to read in (`adma_wants_data`).
        let c = &mut self.cores[core];
        if c.input_data_transferred != 0 {
            let amount = c.input_data_transferred.min(0x180);
            c.input_data_transferred -= amount;
        }

        if matches!(read_index, 0x100 | 0x0 | 0x80 | 0x180) {
            if read_index == 0x100 {
                c.input_pos_write = 0;
            } else if read_index == 0 {
                c.input_pos_write = 0x100;
            }

            if c.input_data_left >= ADMA_BLOCK {
                self.auto_dma_read_buffer(core);
                let c = &mut self.cores[core];
                c.adma_in_progress = true;
                if c.input_data_left < ADMA_BLOCK {
                    c.input_data_left = 0;
                }
            } else if u32::from(c.auto_dma_ctrl) & (core as u32 + 1) != 0 {
                // Out of data: ADMAS reads back with its high bits set.
                c.auto_dma_ctrl |= !3;
            }
        }
        ret
    }
}

/// A transfer's sound memory address `at` halfwords on from `start`.
fn spu_addr(start: u32, at: usize) -> u32 {
    start.wrapping_add(at as u32) & ADDR_MASK
}
