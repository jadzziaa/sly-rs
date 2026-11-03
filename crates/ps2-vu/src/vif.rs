//! The VIF: a FIFO of 32-bit words read as VIF codes and their data.
//!
//! A command runs once all its data is in the FIFO. UNPACK writes vectors
//! into VU data memory (with the MASK, MODE, ROW and COL registers and the
//! CYCLE skipping and filling), MPG loads micro memory, MSCAL and friends
//! start the VU (which runs to its end before the VIF goes on), and DIRECT
//! hands GIF packets to the GS on PATH2. A code with its interrupt bit set
//! stops the VIF after it runs until the EE cancels the stall (FBRST STC).
//!
//! Behaviour follows PCSX2's VIF (`Vif_Codes.cpp`, `Vif_Unpack.cpp`), read
//! for facts: V2 unpacks write v1v0v1v0, V3 unpacks take W from the next
//! element, fill cycles write the next vector's data again, and MODE 3
//! sets ROW without adding.

use std::collections::VecDeque;

use ps2_gs::thread::Sink;

use crate::{Memories, Regs, micro};

/// `VIF_STAT` bits.
pub mod stat {
    /// MARK was written by a MARK code.
    pub const MRK: u32 = 1 << 6;
    /// Double buffer flag.
    pub const DBF: u32 = 1 << 7;
    /// Stalled by STOP.
    pub const VSS: u32 = 1 << 8;
    /// Stalled by `ForceBreak`.
    pub const VFS: u32 = 1 << 9;
    /// Stalled by an interrupt bit.
    pub const VIS: u32 = 1 << 10;
    /// An interrupt bit was seen.
    pub const INT: u32 = 1 << 11;
}

/// Why the VIF stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// It needs more words.
    NeedData,
    /// A code with the interrupt bit ran: the VIF is stalled and the
    /// interrupt should be raised.
    Interrupt,
    /// It is stalled (an interrupt not yet cancelled).
    Stalled,
}

/// A VIF (VIF0 when `vif1` is false: no DIRECT, no double buffering,
/// 4 KiB of VU memory).
#[derive(Clone, Debug)]
pub struct Vif {
    vif1: bool,
    /// `VIF_STAT`.
    pub stat: u32,
    /// `VIF_ERR`.
    pub err: u32,
    /// CYCLE: CL and WL.
    cl: u32,
    wl: u32,
    mode: u32,
    mask: u32,
    row: [u32; 4],
    col: [u32; 4],
    code: u32,
    mark: u32,
    num: u32,
    itop: u32,
    itops: u32,
    base: u32,
    ofst: u32,
    top: u32,
    tops: u32,
    /// MSKPATH3.
    pub mask_path3: bool,
    fifo: VecDeque<u32>,
    /// Commands run, for reports.
    pub commands: u64,
    /// VU programs started, for reports.
    pub programs: u64,
    /// Instruction pairs those programs ran, for reports.
    pub pairs: u64,
    /// Host time those programs took, for reports.
    pub run_time: std::time::Duration,
    /// The VU's decoded pairs.
    pub cache: micro::Cache,
    /// Translated microcode for the VU.
    pub translations: &'static [micro::Translation],
    /// Programs being recorded (`$SLY_RS_VU_RECORD`).
    recording: Option<crate::record::Recording>,
}

impl Vif {
    /// A VIF as the BIOS leaves it when it starts a game: CYCLE is 4, 4
    /// (PCSX2's savestates of Sly 1 show it so on VIF0, which the game
    /// never sets itself; with CL and WL at 0, its first UNPACK would read
    /// no data and the rest of the stream would be misread).
    #[must_use]
    pub fn new(vif1: bool) -> Self {
        Self {
            vif1,
            stat: 0,
            err: 0,
            cl: 4,
            wl: 4,
            mode: 0,
            mask: 0,
            row: [0; 4],
            col: [0; 4],
            code: 0,
            mark: 0,
            num: 0,
            itop: 0,
            itops: 0,
            base: 0,
            ofst: 0,
            top: 0,
            tops: 0,
            mask_path3: false,
            fifo: VecDeque::new(),
            commands: 0,
            programs: 0,
            run_time: std::time::Duration::ZERO,
            pairs: 0,
            cache: micro::Cache::default(),
            translations: &[],
            recording: None,
        }
    }

    /// Queues words from DMA.
    pub fn push(&mut self, words: &[u32]) {
        self.fifo.extend(words);
    }

    /// Whether it is stalled.
    #[must_use]
    pub const fn stalled(&self) -> bool {
        self.stat & (stat::VIS | stat::VSS | stat::VFS) != 0
    }

    /// Reads a register by its offset from the VIF's base (`0x10003800` or
    /// `0x10003C00`).
    #[must_use]
    pub fn read(&self, offset: u32) -> u32 {
        match offset {
            0x00 => self.stat | ((self.fifo.len().div_ceil(4).min(16) as u32) << 24),
            0x20 => self.err,
            0x30 => self.mark,
            0x40 => self.cl | (self.wl << 8),
            0x50 => self.mode,
            0x60 => self.num,
            0x70 => self.mask,
            0x80 => self.code,
            0x90 => self.itops,
            0xA0 => self.base,
            0xB0 => self.ofst,
            0xC0 => self.tops,
            0xD0 => self.itop,
            0xE0 => self.top,
            0x100..=0x130 => self.row[((offset - 0x100) / 16) as usize],
            0x140..=0x170 => self.col[((offset - 0x140) / 16) as usize],
            _ => 0,
        }
    }

    /// Writes a register by its offset.
    pub fn write(&mut self, offset: u32, value: u32) {
        match offset {
            0x10 => {
                if value & 1 != 0 {
                    // RST, as PCSX2 resets a VIF: the FIFO, ERR, MSKPATH3
                    // and the status (VIF0 keeps MRK, DBF and VEW). CYCLE,
                    // MODE, MASK, ROW, COL and the other registers keep
                    // their values: Sly 1 resets VIF0 and relies on the
                    // CYCLE the BIOS left.
                    self.fifo.clear();
                    self.err = 0;
                    self.mask_path3 = false;
                    self.stat = if self.vif1 {
                        0
                    } else {
                        self.stat & (stat::MRK | stat::DBF | 0x4)
                    };
                }
                if value & 8 != 0 {
                    // STC: cancel the stalls and their status.
                    self.stat &= !(stat::VSS | stat::VFS | stat::VIS | stat::INT | 0x3000);
                }
            }
            0x20 => self.err = value & 7,
            0x30 => {
                self.mark = value & 0xFFFF;
                self.stat &= !stat::MRK;
            }
            0x100..=0x130 => self.row[((offset - 0x100) / 16) as usize] = value,
            0x140..=0x170 => self.col[((offset - 0x140) / 16) as usize] = value,
            _ => {}
        }
    }

    /// Words of data that follow `code`.
    fn data_words(&self, code: u32) -> usize {
        let cmd = (code >> 24) & 0x7F;
        let imm = code & 0xFFFF;
        let num = match (code >> 16) & 0xFF {
            0 => 256,
            n => n,
        };
        match cmd {
            0x20 => 1,
            0x30 | 0x31 => 4,
            0x4A => 2 * num as usize,
            0x50 | 0x51 if self.vif1 => 4 * if imm == 0 { 65_536 } else { imm as usize },
            0x60..=0x7F => {
                let bytes = vector_bytes(cmd) * self.vectors_read(num);
                bytes.div_ceil(4) as usize
            }
            _ => 0,
        }
    }

    /// Vectors an UNPACK of `num` writes that take data (filling writes
    /// past CL take none).
    fn vectors_read(&self, num: u32) -> u32 {
        let wl = if self.wl == 0 { 256 } else { self.wl };
        if wl <= self.cl {
            num
        } else {
            self.cl * (num / wl) + (num % wl).min(self.cl)
        }
    }

    /// Runs commands until the FIFO runs dry or a stall. `regs` and `mem`
    /// are the VU this VIF feeds.
    pub fn run(&mut self, regs: &mut Regs, mem: &mut Memories<'_>, gs: &mut dyn Sink) -> Stop {
        loop {
            if self.stalled() {
                return Stop::Stalled;
            }
            let Some(&code) = self.fifo.front() else {
                return Stop::NeedData;
            };
            let n = self.data_words(code);
            if self.fifo.len() < 1 + n {
                return Stop::NeedData;
            }
            self.fifo.pop_front();
            let data: Vec<u32> = self.fifo.drain(..n).collect();
            self.code = code;
            self.commands += 1;
            tracing::trace!(target: "vif", "VIF{} code {code:#010x} ({n} data words)", u8::from(self.vif1));
            self.execute(code, &data, regs, mem, gs);
            if code & 0x8000_0000 != 0 && self.err & 1 == 0 {
                tracing::debug!(target: "vif", "VIF{} interrupt at code {code:#010x}", u8::from(self.vif1));
                self.stat |= stat::VIS | stat::INT;
                return Stop::Interrupt;
            }
        }
    }

    fn execute(
        &mut self,
        code: u32,
        data: &[u32],
        regs: &mut Regs,
        mem: &mut Memories<'_>,
        gs: &mut dyn Sink,
    ) {
        let cmd = (code >> 24) & 0x7F;
        let imm = code & 0xFFFF;
        match cmd {
            0x01 => {
                self.cl = imm & 0xFF;
                self.wl = imm >> 8;
            }
            0x02 if self.vif1 => {
                self.ofst = imm & 0x3FF;
                self.stat &= !stat::DBF;
                self.tops = self.base;
            }
            0x03 if self.vif1 => self.base = imm & 0x3FF,
            0x04 => self.itops = imm & 0x3FF,
            0x05 => self.mode = imm & 3,
            0x06 if self.vif1 => self.mask_path3 = imm & 0x8000 != 0,
            0x07 => {
                self.mark = imm;
                self.stat |= stat::MRK;
            }
            0x14 | 0x15 => self.start(regs, mem, gs, Some(imm * 8)),
            0x17 => self.start(regs, mem, gs, None),
            0x20 => self.mask = data[0],
            0x30 => self.row.copy_from_slice(data),
            0x31 => self.col.copy_from_slice(data),
            0x4A => {
                let mut at = imm * 8;
                for pair in data.as_chunks::<2>().0 {
                    mem.write_micro(at, pair[0], pair[1]);
                    at += 8;
                }
            }
            0x50 | 0x51 if self.vif1 => {
                let qwords = data.as_chunks::<4>().0.iter().map(|w| {
                    u128::from(w[0])
                        | u128::from(w[1]) << 32
                        | u128::from(w[2]) << 64
                        | u128::from(w[3]) << 96
                });
                gs.gif(2, qwords.collect());
            }
            0x60..=0x7F => self.unpack(code, data, mem),
            // NOP, FLUSHE, FLUSH, FLUSHA (the VU has finished: it runs to
            // its end when started), and codes with no effect here.
            _ => {}
        }
    }

    /// Starts the VU at `pc` (or where it stopped), after updating the
    /// double-buffer registers.
    fn start(
        &mut self,
        regs: &mut Regs,
        mem: &mut Memories<'_>,
        gs: &mut dyn Sink,
        pc: Option<u32>,
    ) {
        self.itop = self.itops;
        if self.vif1 {
            self.top = self.tops & 0x3FF;
            if self.stat & stat::DBF != 0 {
                self.tops = self.base;
                self.stat &= !stat::DBF;
            } else {
                self.tops = self.base + self.ofst;
                self.stat |= stat::DBF;
            }
        }
        tracing::trace!(target: "vu", "VIF{} starts the VU at {pc:x?} (TPC {:#x})", u8::from(self.vif1), regs.tpc);
        let pc = pc.unwrap_or(regs.tpc);
        if self.vif1
            && let Some((path, skip, count)) = crate::record::wanted()
            && (*skip..skip + count).contains(&self.programs)
        {
            let rec = self.recording.get_or_insert_default();
            rec.add(mem.code, mem.data, regs, pc, self.itop, self.top);
            if self.programs + 1 == skip + count {
                let _ = std::fs::write(path, rec.to_bytes());
            }
        }
        let vu1 = self.vif1.then_some(micro::Vu1Link {
            itop: self.itop,
            top: self.top,
            gs,
        });
        let started = std::time::Instant::now();
        let run = micro::run(
            &mut micro::Unit {
                regs,
                code: mem.code,
                data: mem.data,
                vu1,
                cache: &mut self.cache,
                translations: self.translations,
                code_writes: *mem.code_writes,
            },
            pc,
        );
        self.run_time += started.elapsed();
        self.programs += 1;
        self.pairs += run.pairs;
        if run.runaway {
            tracing::warn!(target: "vu", "VU{} program at {pc:#06x} ran {} pairs without ending", u8::from(self.vif1), run.pairs);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one unpack, as the hardware's loop runs it"
    )]
    fn unpack(&mut self, code: u32, data: &[u32], mem: &mut Memories<'_>) {
        let cmd = (code >> 24) & 0x7F;
        let imm = code & 0xFFFF;
        let num = match (code >> 16) & 0xFF {
            0 => 256,
            n => n,
        };
        let (vl, vn, masked) = (cmd & 3, (cmd >> 2) & 3, cmd & 0x10 != 0);
        let usn = imm & 0x4000 != 0;
        let mut addr = imm & 0x3FF;
        if self.vif1 && imm & 0x8000 != 0 {
            addr += self.tops;
        }
        let mem_mask = if self.vif1 { 0x3FF0 } else { 0xFF0 };
        let mut at = addr * 16;
        let bytes: Vec<u8> = data.iter().flat_map(|w| w.to_le_bytes()).collect();
        let size = vector_bytes(cmd) as usize;
        // One element of the source (`k`th of the vector at byte `offset`).
        let element = |offset: usize, k: usize| -> u32 {
            let width = match vl {
                0 => 4,
                1 => 2,
                _ => 1,
            };
            let at = offset + k * width;
            let Some(raw) = bytes.get(at..at + width) else {
                return 0;
            };
            match (width, usn) {
                (4, _) => u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]),
                (2, true) => u32::from(u16::from_le_bytes([raw[0], raw[1]])),
                (2, false) => i32::from(i16::from_le_bytes([raw[0], raw[1]])) as u32,
                (_, true) => u32::from(raw[0]),
                (_, false) => i32::from(raw[0] as i8) as u32,
            }
        };
        let wl = if self.wl == 0 { 256 } else { self.wl };
        let fill = self.cl < wl;
        let skip = if fill { 0 } else { (self.cl - wl) * 16 };
        let mut cl = 0u32;
        let mut offset = 0usize;
        for _ in 0..num {
            let fields: [u32; 4] = match (vn, vl) {
                (0, _) => [element(offset, 0); 4],
                (1, _) => {
                    let (x, y) = (element(offset, 0), element(offset, 1));
                    [x, y, x, y]
                }
                (2, _) => [
                    element(offset, 0),
                    element(offset, 1),
                    element(offset, 2),
                    element(offset, 3),
                ],
                (_, 3) => {
                    let v = element(offset, 0) & 0xFFFF;
                    [
                        (v & 0x1F) << 3,
                        (v & 0x3E0) >> 2,
                        (v & 0x7C00) >> 7,
                        (v & 0x8000) >> 8,
                    ]
                }
                _ => [
                    element(offset, 0),
                    element(offset, 1),
                    element(offset, 2),
                    element(offset, 3),
                ],
            };
            let mode = if (vn, vl) == (3, 3) { 0 } else { self.mode };
            let base = (at & mem_mask) as usize;
            for (k, &value) in fields.iter().enumerate() {
                let sel = if masked {
                    (self.mask >> (2 * (k as u32 + 4 * cl.min(3)))) & 3
                } else {
                    0
                };
                let word = base + 4 * k;
                let out = match sel {
                    0 => match mode {
                        1 => value.wrapping_add(self.row[k]),
                        2 => {
                            self.row[k] = self.row[k].wrapping_add(value);
                            self.row[k]
                        }
                        3 => {
                            self.row[k] = value;
                            value
                        }
                        _ => value,
                    },
                    1 => self.row[k],
                    2 => self.col[cl.min(3) as usize],
                    _ => continue,
                };
                mem.write_data32(word as u32, out);
            }
            at += 16;
            cl += 1;
            if fill {
                if cl <= self.cl {
                    offset += size;
                } else if cl == wl {
                    cl = 0;
                }
            } else {
                offset += size;
                if cl >= wl {
                    at += skip;
                    cl = 0;
                }
            }
        }
        self.num = 0;
    }
}

/// Bytes of source data per vector of an UNPACK command.
const fn vector_bytes(cmd: u32) -> u32 {
    let (vl, vn) = (cmd & 3, (cmd >> 2) & 3);
    if vl == 3 {
        2
    } else {
        ((32 >> vl) * (vn + 1)) / 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A VIF1, VU1 registers and memories, and a GS.
    struct Rig {
        vif: Vif,
        regs: Regs,
        code: Vec<u8>,
        data: Vec<u8>,
        gs: ps2_gs::Gs,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                vif: Vif::new(true),
                regs: crate::reset_regs(),
                code: vec![0; 0x4000],
                data: vec![0; 0x4000],
                gs: ps2_gs::Gs::default(),
            }
        }

        fn run(&mut self, words: &[u32]) -> Stop {
            self.vif.push(words);
            self.resume()
        }

        fn resume(&mut self) -> Stop {
            let mut writes = 0;
            let mut mem = Memories {
                code: &mut self.code,
                data: &mut self.data,
                code_writes: &mut writes,
            };
            self.vif.run(&mut self.regs, &mut mem, &mut self.gs)
        }

        fn word(&self, address: u32) -> u32 {
            let a = address as usize;
            u32::from_le_bytes([
                self.data[a],
                self.data[a + 1],
                self.data[a + 2],
                self.data[a + 3],
            ])
        }
    }

    #[test]
    fn unpack_v4_32_with_skipping() {
        let mut rig = Rig::new();
        // STCYCL CL=2 WL=1: write one, skip one. V4-32, two vectors at
        // qword 4.
        let stop = rig.run(&[0x0100_0102, 0x6C02_0004, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(stop, Stop::NeedData);
        assert_eq!(rig.word(4 * 16), 1);
        assert_eq!(rig.word(4 * 16 + 12), 4);
        assert_eq!(rig.word(6 * 16), 5);
    }

    #[test]
    fn unpack_s16_signed_v2_and_masks() {
        let mut rig = Rig::new();
        // STCYCL 1, 1; S-16 signed, one vector at 0.
        rig.run(&[0x0100_0101, 0x6101_0000, 0xFFFE]);
        assert_eq!(rig.word(0), 0xFFFF_FFFE);
        assert_eq!(rig.word(12), 0xFFFF_FFFE);
        // V2-32 at 1: x y x y.
        rig.run(&[0x6401_0001, 7, 9]);
        assert_eq!((rig.word(16 + 8), rig.word(16 + 12)), (7, 9));
        // Masked V4-32 at 2: X from ROW, Y from COL, Z protected.
        rig.run(&[
            0x3000_0000,
            10,
            11,
            12,
            13,
            0x2000_0000,
            0b11_10_01,
            0x3101_0000,
            20,
            21,
            22,
            23,
        ]);
        rig.run(&[0x7C01_0002, 1, 2, 3, 4]);
        assert_eq!([0, 4, 8, 12].map(|k| rig.word(32 + k)), [10, 20, 0, 4]);
    }

    #[test]
    fn cycle_starts_at_four_and_survives_a_reset() {
        let mut rig = Rig::new();
        rig.vif.vif1 = false;
        assert_eq!(rig.vif.read(0x40), 0x0404, "as the BIOS leaves it");
        // RST: the FIFO and the status go, CYCLE stays; an UNPACK V4-32 of
        // one quadword then reads its four words.
        rig.vif.write(0x10, 1);
        assert_eq!(rig.vif.read(0x40), 0x0404);
        assert_eq!(rig.run(&[0x6C01_0002, 1, 2, 3, 4]), Stop::NeedData);
        assert_eq!([0, 4, 8, 12].map(|k| rig.word(32 + k)), [1, 2, 3, 4]);
    }

    #[test]
    fn interrupt_bit_stalls_until_stc() {
        let mut rig = Rig::new();
        // MARK with the interrupt bit, then STMOD.
        assert_eq!(rig.run(&[0x8700_1234, 0x0500_0002]), Stop::Interrupt);
        assert_eq!(rig.vif.read(0x30), 0x1234);
        assert_eq!(rig.resume(), Stop::Stalled);
        rig.vif.write(0x10, 8);
        assert_eq!(rig.resume(), Stop::NeedData);
        assert_eq!(rig.vif.read(0x50), 2);
    }

    #[test]
    fn mscal_swaps_double_buffers_and_runs_the_program() {
        let mut rig = Rig::new();
        // MPG one pair at 0: `nop[e] | nop`, then the delay slot.
        let nop_e = 0x4000_02FF;
        rig.run(&[0x4A02_0000, 0x8000_033C, nop_e, 0x8000_033C, 0x0000_02FF]);
        // BASE 0x10, OFFSET 0x20, then two MSCALs.
        rig.run(&[0x0300_0010, 0x0200_0020, 0x1400_0000]);
        assert_eq!((rig.vif.read(0xE0), rig.vif.read(0xC0)), (0x10, 0x30));
        rig.run(&[0x1400_0000]);
        assert_eq!((rig.vif.read(0xE0), rig.vif.read(0xC0)), (0x30, 0x10));
        assert_eq!(rig.vif.programs, 2);
        assert_eq!(rig.regs.tpc, 16);
    }
}
