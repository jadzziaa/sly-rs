use std::fmt;

use sha1::Digest as _;

/// A SHA-1 digest, shown as 40 lowercase hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha1Digest(pub [u8; 20]);

impl Sha1Digest {
    /// Parses 40 hex digits at compile time, for tables of known hashes.
    ///
    /// # Panics
    ///
    /// If `hex` is not exactly 40 hex digits (a compile error in `const`
    /// context).
    #[must_use]
    pub const fn from_hex(hex: &str) -> Self {
        const fn nibble(c: u8) -> u8 {
            match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => panic!("SHA-1 digest: not a hex digit"),
            }
        }
        let hex = hex.as_bytes();
        assert!(hex.len() == 40, "SHA-1 digest: need 40 hex digits");
        let mut out = [0; 20];
        let mut i = 0;
        while i < 20 {
            out[i] = (nibble(hex[2 * i]) << 4) | nibble(hex[2 * i + 1]);
            i += 1;
        }
        Self(out)
    }

    /// SHA-1 of `data`.
    #[must_use]
    pub fn of(data: &[u8]) -> Self {
        let mut hasher = Hasher::new();
        hasher.update(data);
        hasher.finish()
    }
}

impl fmt::Display for Sha1Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl fmt::Debug for Sha1Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha1Digest({self})")
    }
}

/// Incremental SHA-1.
pub(crate) struct Hasher(sha1::Sha1);

impl Hasher {
    pub(crate) fn new() -> Self {
        Self(sha1::Sha1::new())
    }

    pub(crate) fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    pub(crate) fn finish(self) -> Sha1Digest {
        let mut out = [0; 20];
        out.copy_from_slice(&self.0.finalize());
        Sha1Digest(out)
    }
}
