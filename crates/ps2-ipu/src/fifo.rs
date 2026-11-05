//! The input and output FIFOs, eight quadwords each (`IPU_Fifo.cpp`).

/// Quadwords each FIFO holds.
pub(crate) const FIFO_QWS: usize = 8;

/// The input FIFO (PCSX2's `IPU_Fifo_Input`). Its count is PCSX2's
/// `g_BP.IFC`; `data_requested` is its `IPUCoreStatus.DataRequested`.
#[derive(Clone, Debug, Default)]
pub(crate) struct InFifo {
    data: [u128; FIFO_QWS],
    read_pos: usize,
    write_pos: usize,
    /// Quadwords held (`IFC`).
    pub(crate) count: usize,
    /// The toIPU DMA may move data: set when the IPU reads with one
    /// quadword or none left, or the FIFO is cleared; cleared when full.
    pub(crate) data_requested: bool,
}

impl InFifo {
    /// Takes as many of `qws` as fit; returns how many.
    pub(crate) fn write(&mut self, qws: &[u128]) -> usize {
        let n = qws.len().min(FIFO_QWS - self.count);
        for &qw in &qws[..n] {
            self.data[self.write_pos] = qw;
            self.write_pos = (self.write_pos + 1) % FIFO_QWS;
        }
        self.count += n;
        if self.count == FIFO_QWS {
            self.data_requested = false;
        }
        n
    }

    /// The next quadword, if any. With one left or none, it also asks the
    /// DMA for more.
    pub(crate) fn read(&mut self) -> Option<u128> {
        if self.count <= 1 {
            self.data_requested = true;
            if self.count == 0 {
                return None;
            }
        }
        let qw = self.data[self.read_pos];
        self.read_pos = (self.read_pos + 1) % FIFO_QWS;
        self.count -= 1;
        Some(qw)
    }

    /// Empties the FIFO, which then asks for data.
    pub(crate) fn clear(&mut self) {
        *self = Self {
            data_requested: true,
            ..Self::default()
        };
    }
}

/// The output FIFO (PCSX2's `IPU_Fifo_Output`). Its count is `IPU_CTRL`'s
/// OFC field.
#[derive(Clone, Debug, Default)]
pub(crate) struct OutFifo {
    data: [u128; FIFO_QWS],
    read_pos: usize,
    write_pos: usize,
    /// Quadwords held (OFC).
    pub(crate) count: usize,
}

impl OutFifo {
    /// Room left, in quadwords.
    pub(crate) fn free(&self) -> usize {
        FIFO_QWS - self.count
    }

    /// Takes quadwords from `next` while there is room, at most `n`;
    /// returns how many.
    pub(crate) fn write(&mut self, n: usize, mut next: impl FnMut(usize) -> u128) -> usize {
        let n = n.min(self.free());
        for k in 0..n {
            self.data[self.write_pos] = next(k);
            self.write_pos = (self.write_pos + 1) % FIFO_QWS;
        }
        self.count += n;
        n
    }

    /// Takes up to `n` quadwords.
    pub(crate) fn read(&mut self, n: usize) -> Vec<u128> {
        let n = n.min(self.count);
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.data[self.read_pos]);
            self.read_pos = (self.read_pos + 1) % FIFO_QWS;
        }
        self.count -= n;
        out
    }

    /// Empties the FIFO.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}
