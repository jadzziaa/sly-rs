use std::fs::File;
use std::io;

/// A source of bytes that can be read at any offset without a cursor, so one
/// source can serve reads from several threads.
pub trait ReadAt {
    /// Fills `buf` with the bytes starting at `offset`.
    ///
    /// # Errors
    ///
    /// Any I/O error, or [`io::ErrorKind::UnexpectedEof`] if the source ends
    /// before `buf` is full.
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()>;

    /// Total size of the source in bytes.
    ///
    /// # Errors
    ///
    /// Any I/O error.
    fn size(&self) -> io::Result<u64>;
}

impl ReadAt for File {
    #[cfg(unix)]
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        std::os::unix::fs::FileExt::read_exact_at(self, buf, offset)
    }

    #[cfg(windows)]
    fn read_exact_at(&self, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
        use std::os::windows::fs::FileExt;
        while !buf.is_empty() {
            match self.seek_read(buf, offset) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    buf = &mut buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn size(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }
}

impl ReadAt for [u8] {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let src = usize::try_from(offset)
            .ok()
            .and_then(|start| self.get(start..start.checked_add(buf.len())?))
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        buf.copy_from_slice(src);
        Ok(())
    }

    fn size(&self) -> io::Result<u64> {
        Ok(self.len() as u64)
    }
}

impl ReadAt for Vec<u8> {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        self.as_slice().read_exact_at(buf, offset)
    }

    fn size(&self) -> io::Result<u64> {
        self.as_slice().size()
    }
}

impl<T: ReadAt + ?Sized> ReadAt for &T {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        (**self).read_exact_at(buf, offset)
    }

    fn size(&self) -> io::Result<u64> {
        (**self).size()
    }
}
