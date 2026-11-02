use crate::DiscError;

/// The boot configuration of a PS2 disc (`SYSTEM.CNF`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemCnf {
    /// The `BOOT2` path, such as `cdrom0:\SCUS_971.98;1`.
    pub boot2: String,
    /// The `VER` field, if present.
    pub ver: Option<String>,
    /// The `VMODE` field (`NTSC` or `PAL`), if present.
    pub vmode: Option<String>,
}

impl SystemCnf {
    /// Parses `KEY = VALUE` lines; spaces around `=` are optional.
    ///
    /// # Errors
    ///
    /// [`DiscError::BadSystemCnf`] if there is no `BOOT2` line.
    pub fn parse(text: &str) -> Result<Self, DiscError> {
        let mut boot2 = None;
        let mut ver = None;
        let mut vmode = None;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = Some(value.trim().to_owned());
            match key.trim().to_ascii_uppercase().as_str() {
                "BOOT2" => boot2 = value,
                "VER" => ver = value,
                "VMODE" => vmode = value,
                _ => {}
            }
        }
        let boot2 = boot2.ok_or_else(|| DiscError::BadSystemCnf("no BOOT2 line".to_owned()))?;
        Ok(Self { boot2, ver, vmode })
    }

    /// The boot executable's file name in the root directory: `BOOT2` without
    /// its device prefix and version suffix (`SCUS_971.98`).
    #[must_use]
    pub fn boot_file(&self) -> &str {
        let path = self
            .boot2
            .rsplit_once(['\\', '/', ':'])
            .map_or(self.boot2.as_str(), |(_, file)| file);
        crate::iso9660::strip_version(path)
    }
}
