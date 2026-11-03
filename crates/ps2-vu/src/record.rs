//! Recordings of VU programs as they start: their memories, registers and
//! entry point, enough to run them again away from the machine. `sly-rs
//! vu-bench` times them through the interpreter and through translated
//! microcode and checks that both give the same result.
//!
//! With `$SLY_RS_VU_RECORD` set to `PATH,SKIP,COUNT`, VIF1 skips `SKIP`
//! programs, records the next `COUNT` and writes them to `PATH`. A
//! recording holds the game's microcode and data: keep it out of the
//! repository.

use std::sync::OnceLock;

use crate::Regs;

/// One program as it started.
#[derive(Clone, Debug)]
pub struct Start {
    /// Byte address in micro memory.
    pub pc: u32,
    /// What XITOP and XTOP read.
    pub itop: u32,
    /// What XTOP reads.
    pub top: u32,
    /// The registers.
    pub regs: Regs,
    /// Which of the recording's micro memories.
    pub code: usize,
    /// Data memory.
    pub data: Vec<u8>,
}

/// Programs as they started, with the micro memories they ran from.
#[derive(Clone, Debug, Default)]
pub struct Recording {
    /// The distinct micro memories.
    pub codes: Vec<Vec<u8>>,
    /// The programs, in order.
    pub starts: Vec<Start>,
}

const MAGIC: &[u8; 8] = b"SLYVUREC";

fn regs_words(r: &Regs) -> Vec<u32> {
    let mut w: Vec<u32> = r.vf.iter().flatten().copied().collect();
    w.extend_from_slice(&r.vi);
    w.extend_from_slice(&r.acc);
    w.extend_from_slice(&[
        r.q, r.p, r.i, r.r, r.status, r.mac, r.clip, r.cmsar0, r.fbrst, r.tpc,
    ]);
    w
}

fn regs_from(w: &[u32]) -> Regs {
    let mut r = Regs::default();
    for (k, v) in r.vf.iter_mut().enumerate() {
        v.copy_from_slice(&w[4 * k..4 * k + 4]);
    }
    r.vi.copy_from_slice(&w[128..144]);
    r.acc.copy_from_slice(&w[144..148]);
    [
        r.q, r.p, r.i, r.r, r.status, r.mac, r.clip, r.cmsar0, r.fbrst, r.tpc,
    ] = std::array::from_fn(|k| w[148 + k]);
    r
}

/// Registers as words, in [`Recording::to_bytes`]'s order (for comparing).
#[must_use]
pub fn registers(r: &Regs) -> Vec<u32> {
    regs_words(r)
}

impl Recording {
    /// Adds a program starting at `pc` with these memories and registers.
    pub fn add(&mut self, code: &[u8], data: &[u8], regs: &Regs, pc: u32, itop: u32, top: u32) {
        let index = self
            .codes
            .iter()
            .position(|c| c == code)
            .unwrap_or_else(|| {
                self.codes.push(code.to_vec());
                self.codes.len() - 1
            });
        self.starts.push(Start {
            pc,
            itop,
            top,
            regs: regs.clone(),
            code: index,
            data: data.to_vec(),
        });
    }

    /// The recording as bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        let put = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
        let len = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        put(&mut out, len(self.codes.len()));
        for c in &self.codes {
            put(&mut out, len(c.len()));
            out.extend_from_slice(c);
        }
        put(&mut out, len(self.starts.len()));
        for s in &self.starts {
            for v in [s.pc, s.itop, s.top, len(s.code), len(s.data.len())] {
                put(&mut out, v);
            }
            for v in regs_words(&s.regs) {
                put(&mut out, v);
            }
            out.extend_from_slice(&s.data);
        }
        out
    }

    /// Reads a recording.
    ///
    /// # Errors
    ///
    /// If the bytes are not a recording.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let mut cur = Cursor { bytes, at: 0 };
        if cur.take(8)? != MAGIC {
            return Err("not a VU recording".to_owned());
        }
        let mut rec = Self::default();
        for _ in 0..cur.word()? {
            let n = cur.word()? as usize;
            rec.codes.push(cur.take(n)?.to_vec());
        }
        for _ in 0..cur.word()? {
            let (pc, itop, top) = (cur.word()?, cur.word()?, cur.word()?);
            let (code, data) = (cur.word()? as usize, cur.word()? as usize);
            let mut regs = [0; 158];
            for v in &mut regs {
                *v = cur.word()?;
            }
            let data = cur.take(data)?.to_vec();
            if code >= rec.codes.len() {
                return Err("bad micro memory index".to_owned());
            }
            rec.starts.push(Start {
                pc,
                itop,
                top,
                regs: regs_from(&regs),
                code,
                data,
            });
        }
        Ok(rec)
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let b = self
            .bytes
            .get(self.at..self.at + n)
            .ok_or("truncated recording")?;
        self.at += n;
        Ok(b)
    }

    fn word(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Where to record VU1's programs: path, programs to skip, programs to
/// record (`$SLY_RS_VU_RECORD`).
pub(crate) fn wanted() -> Option<&'static (String, u64, u64)> {
    static WANTED: OnceLock<Option<(String, u64, u64)>> = OnceLock::new();
    WANTED
        .get_or_init(|| {
            let v = std::env::var("SLY_RS_VU_RECORD").ok()?;
            let mut parts = v.rsplitn(3, ',');
            let count = parts.next()?.parse().ok()?;
            let skip = parts.next()?.parse().ok()?;
            Some((parts.next()?.to_owned(), skip, count))
        })
        .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recordings_read_back() {
        let mut rec = Recording::default();
        let mut regs = Regs::default();
        regs.vf[3] = [1, 2, 3, 4];
        regs.tpc = 9;
        rec.add(&[1, 2, 3, 4, 5, 6, 7, 8], &[9; 16], &regs, 8, 1, 2);
        rec.add(&[1, 2, 3, 4, 5, 6, 7, 8], &[7; 16], &regs, 0, 3, 4);
        let back = Recording::from_bytes(&rec.to_bytes()).unwrap();
        assert_eq!(back.codes.len(), 1);
        assert_eq!(back.starts.len(), 2);
        assert_eq!(registers(&back.starts[1].regs), registers(&regs));
        assert_eq!(
            (back.starts[1].pc, back.starts[1].top, &back.starts[1].data),
            (0, 4, &vec![7; 16])
        );
    }
}
