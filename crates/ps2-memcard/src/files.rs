//! The card's file system as Sony's card manager (`mcman`) keeps it: files
//! and directories opened by path, read and written through handles,
//! listed with wildcards, deleted and renamed. Results are `mcman`'s:
//! a count, a handle or 0 on success, its negative codes otherwise.
//!
//! What it writes follows `mcman` (ps2sdk's reimplementation in
//! `iop/memorycard/mcman`, read for facts): a new file's entry has mode
//! 0x8417 and no cluster until written, a new directory 0x8427 with `.`
//! and `..` pointing back at its parent's entry; deleted entries keep
//! their place and their clusters' chain (bit 31 cleared) until reused;
//! clusters are taken lowest first. So a card written here reads in PCSX2
//! and on a console.

use crate::{CHAIN_END, CLUSTER_SIZE, Card, IN_USE, PAGE_SIZE, PAGES_PER_CLUSTER, mode};

/// `mcman`'s results.
pub mod result {
    /// The card was changed since it was last seen (the first detection).
    pub const CHANGED_CARD: i32 = -1;
    /// Not formatted.
    pub const NO_FORMAT: i32 = -2;
    /// No free cluster.
    pub const FULL_DEVICE: i32 = -3;
    /// No such file or directory.
    pub const NO_ENTRY: i32 = -4;
    /// Not allowed (a mode bit, a handle not open for it, a file in use).
    pub const DENIED_PERMIT: i32 = -5;
    /// A directory with entries left, deleted.
    pub const NOT_EMPTY: i32 = -6;
    /// Every handle is in use.
    pub const UP_LIMIT_HANDLE: i32 = -7;
}

use result::{DENIED_PERMIT, FULL_DEVICE, NO_ENTRY, NOT_EMPTY, UP_LIMIT_HANDLE};

/// Open flags beyond the mode bits ([`mode::READ`], [`mode::WRITE`]).
pub mod open {
    /// Create a directory (`sceMcMkDir`).
    pub const CREATE_DIR: u32 = 0x0040;
    /// Create the file, replacing one of that name.
    pub const CREATE_FILE: u32 = 0x0200;
}

/// `sceMcSetFileInfo`'s flags.
pub mod info {
    /// The creation time.
    pub const CREATED: u32 = 0x01;
    /// The modification time.
    pub const MODIFIED: u32 = 0x02;
    /// The mode's attribute bits.
    pub const ATTRIBUTES: u32 = 0x04;
    /// The copy-protection word (`Reserve2`).
    pub const PROTECTION: u32 = 0x08;
    /// The name (`sceMcRename`).
    pub const NAME: u32 = 0x10;
}

/// Handles open at once: `mcman`'s limit.
const HANDLES: usize = 3;
/// A file's or directory's entries in a cluster.
const ENTRIES_PER_CLUSTER: u32 = 2;
/// Bytes in a cluster.
const CLUSTER_BYTES: u32 = 1024;
const _: () = assert!(
    CLUSTER_BYTES as usize == CLUSTER_SIZE
        && ENTRIES_PER_CLUSTER as usize * PAGE_SIZE == CLUSTER_SIZE
);
/// No cluster (a file not yet written).
const NO_CLUSTER: u32 = 0xFFFF_FFFF;
/// Set on a file closed after being written.
const CLOSED: u16 = 0x0080;
/// Mode bits the creating flags carry over (PDA application, PS1 data).
const CREATE_KEEPS: u32 = 0x0800 | 0x1000;
/// Mode bits `sceMcSetFileInfo` may change.
const SETTABLE: u16 = 0x180F;
/// Bytes in a `sceMcTblGetDir` record.
const TABLE_SIZE: usize = 64;

/// A date and time as the card stores them (`sceMcStDateTime`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Time {
    /// Seconds.
    pub second: u8,
    /// Minutes.
    pub minute: u8,
    /// Hours.
    pub hour: u8,
    /// Day of the month.
    pub day: u8,
    /// Month, 1 to 12.
    pub month: u8,
    /// Year.
    pub year: u16,
}

impl Time {
    fn bytes(self) -> [u8; 8] {
        let [y0, y1] = self.year.to_le_bytes();
        [
            0,
            self.second,
            self.minute,
            self.hour,
            self.day,
            self.month,
            y0,
            y1,
        ]
    }
}

/// Where an entry is: the first cluster of the directory holding it, and
/// its index there. A directory is known by its own entry's place; the
/// root's is its own `.` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct At {
    dir: u32,
    index: u32,
}

/// A directory entry's page.
#[derive(Clone)]
struct Raw([u8; PAGE_SIZE]);

impl Raw {
    fn word(&self, at: usize) -> u32 {
        u32::from_le_bytes([self.0[at], self.0[at + 1], self.0[at + 2], self.0[at + 3]])
    }

    fn set_word(&mut self, at: usize, value: u32) {
        self.0[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn mode(&self) -> u16 {
        u16::from_le_bytes([self.0[0], self.0[1]])
    }

    fn set_mode(&mut self, value: u16) {
        self.0[0..2].copy_from_slice(&value.to_le_bytes());
    }

    fn length(&self) -> u32 {
        self.word(4)
    }

    fn cluster(&self) -> u32 {
        self.word(16)
    }

    fn dir_entry(&self) -> u32 {
        self.word(20)
    }

    fn created(&self) -> [u8; 8] {
        self.0[8..16].try_into().unwrap_or_default()
    }

    fn modified(&self) -> [u8; 8] {
        self.0[24..32].try_into().unwrap_or_default()
    }

    fn name(&self) -> &[u8] {
        self.0[0x40..0x60]
            .split(|&b| b == 0)
            .next()
            .unwrap_or_default()
    }

    fn set_name(&mut self, name: &[u8]) {
        self.0[0x40..0x60].fill(0);
        let n = name.len().min(32);
        self.0[0x40..0x40 + n].copy_from_slice(&name[..n]);
    }

    fn exists(&self) -> bool {
        self.mode() & mode::EXISTS != 0
    }

    fn is_dir(&self) -> bool {
        self.mode() & mode::DIR != 0
    }

    /// A new entry: created and modified now.
    fn new(mode: u16, length: u32, cluster: u32, dir_entry: u32, now: [u8; 8]) -> Self {
        let mut raw = Self([0; PAGE_SIZE]);
        raw.set_mode(mode);
        raw.set_word(4, length);
        raw.0[8..16].copy_from_slice(&now);
        raw.set_word(16, cluster);
        raw.set_word(20, dir_entry);
        raw.0[24..32].copy_from_slice(&now);
        raw
    }
}

/// An open file or directory.
#[derive(Debug, Clone)]
struct Handle {
    /// Its entry.
    entry: At,
    /// The entry of the directory holding it.
    parent: At,
    /// Its first cluster, or [`NO_CLUSTER`].
    first: u32,
    size: u32,
    position: u32,
    read: bool,
    write: bool,
    /// Written since opened or last flushed: its entry is marked not
    /// closed until then.
    written: bool,
}

/// What a path names.
enum Lookup {
    /// An entry, and its place.
    Found(At, Box<Raw>),
    /// Nothing of that name in an existing directory (given by its
    /// entry's place).
    Missing(At, Vec<u8>),
    /// A directory on the way is missing.
    NoParent,
    /// A directory on the way may not be searched.
    Denied,
}

/// A card's file system, with the card manager's handles and current
/// directory.
#[derive(Clone)]
pub struct Files {
    card: Card,
    handles: [Option<Handle>; HANDLES],
    cwd: At,
    /// Not detected yet: the first command reports a changed card.
    fresh: bool,
    /// Counts completed changes, for saving the image.
    changes: u64,
    /// The time stamped on what is created or written.
    pub now: Time,
}

impl Files {
    /// A card's file system, as just inserted.
    #[must_use]
    pub fn new(card: Card, now: Time) -> Self {
        let cwd = At {
            dir: card.root_cluster(),
            index: 0,
        };
        Self {
            card,
            handles: Default::default(),
            cwd,
            fresh: true,
            changes: 0,
            now,
        }
    }

    /// The card.
    #[must_use]
    pub fn card(&self) -> &Card {
        &self.card
    }

    /// How many changes were completed so far (closing a written file,
    /// creating, deleting, renaming): a new count means an image to save.
    #[must_use]
    pub fn changes(&self) -> u64 {
        self.changes
    }

    /// Detection (`McDetectCard2`): the card was changed the first time,
    /// the same card after.
    pub fn detect(&mut self) -> i32 {
        if std::mem::take(&mut self.fresh) {
            result::CHANGED_CARD
        } else {
            0
        }
    }

    fn root(&self) -> At {
        At {
            dir: self.card.root_cluster(),
            index: 0,
        }
    }

    /// The page of an entry: the directory's chain followed to it.
    fn entry_page(&self, at: At) -> Option<usize> {
        let mut cluster = at.dir;
        for _ in 0..at.index / ENTRIES_PER_CLUSTER {
            let next = self.card.fat(cluster);
            if next == CHAIN_END || next & IN_USE == 0 {
                return None;
            }
            cluster = next & !IN_USE;
        }
        Some(Card::cluster_page(cluster) + (at.index % ENTRIES_PER_CLUSTER) as usize)
    }

    fn read(&self, at: At) -> Option<Raw> {
        self.entry_page(at).map(|page| Raw(*self.card.page(page)))
    }

    fn write(&mut self, at: At, raw: &Raw) {
        if let Some(page) = self.entry_page(at) {
            self.card.set_page(page, &raw.0);
        }
    }

    /// The entry of the directory whose entries start at `dir`: where its
    /// `.` entry points.
    fn record_of(&self, dir: u32) -> Option<At> {
        let dot = self.read(At { dir, index: 0 })?;
        Some(At {
            dir: dot.cluster(),
            index: dot.dir_entry(),
        })
    }

    /// The entries of a directory, by index.
    fn entries(&self, dir: &Raw) -> Vec<(At, Raw)> {
        (0..dir.length())
            .filter_map(|index| {
                let at = At {
                    dir: dir.cluster(),
                    index,
                };
                self.read(at).map(|raw| (at, raw))
            })
            .collect()
    }

    /// Finds what a path names, from the root (a leading `/`) or the
    /// current directory. `deleted` finds deleted entries instead.
    fn lookup(&self, path: &[u8], deleted: bool) -> Lookup {
        let (mut here, rest) = match path.strip_prefix(b"/") {
            Some(rest) => (self.root(), rest),
            None => (self.cwd, path),
        };
        let Some(mut dir) = self.read(here) else {
            return Lookup::NoParent;
        };
        let parts: Vec<&[u8]> = rest.split(|&b| b == b'/').collect();
        for (k, part) in parts.iter().enumerate() {
            let last = k + 1 == parts.len();
            // A trailing slash names the directory itself.
            if part.is_empty() {
                if last {
                    break;
                }
                continue;
            }
            if dir.mode() & (mode::READ | mode::EXECUTE) != mode::READ | mode::EXECUTE {
                return Lookup::Denied;
            }
            let next = match *part {
                b"." => Some(here),
                b".." => self.record_of(here.dir),
                name => self
                    .entries(&dir)
                    .into_iter()
                    .find(|(_, e)| {
                        e.exists() != deleted
                            && e.mode() & mode::HIDDEN == 0
                            && e.name() == &name[..name.len().min(32)]
                    })
                    .map(|(at, _)| at),
            };
            let Some(next) = next else {
                return if last {
                    Lookup::Missing(here, part.to_vec())
                } else {
                    Lookup::NoParent
                };
            };
            let Some(raw) = self.read(next) else {
                return Lookup::NoParent;
            };
            if last {
                return Lookup::Found(next, Box::new(raw));
            }
            if !raw.is_dir() {
                return Lookup::NoParent;
            }
            (here, dir) = (next, raw);
        }
        Lookup::Found(here, Box::new(dir))
    }

    /// The lowest free allocatable cluster, taken (the end of a chain).
    fn allocate(&mut self) -> Option<u32> {
        let cluster = (0..self.card.alloc_end()).find(|&c| self.card.fat(c) & IN_USE == 0)?;
        self.card.set_fat(cluster, CHAIN_END);
        Some(cluster)
    }

    /// The `n`th cluster of a chain starting at `first`, the chain grown
    /// to it when `grow` (`first` too, if [`NO_CLUSTER`]).
    fn nth_cluster(&mut self, first: &mut u32, n: u32, grow: bool) -> Result<u32, i32> {
        if *first == NO_CLUSTER {
            if !grow {
                return Err(NO_ENTRY);
            }
            *first = self.allocate().ok_or(FULL_DEVICE)?;
        }
        let mut cluster = *first;
        for _ in 0..n {
            let next = self.card.fat(cluster);
            cluster = if next == CHAIN_END {
                if !grow {
                    return Err(NO_ENTRY);
                }
                let new = self.allocate().ok_or(FULL_DEVICE)?;
                self.card.set_fat(cluster, new | IN_USE);
                new
            } else {
                next & !IN_USE
            };
        }
        Ok(cluster)
    }

    fn handle(&mut self, fd: i32) -> Result<&mut Handle, i32> {
        usize::try_from(fd)
            .ok()
            .and_then(|fd| self.handles.get_mut(fd))
            .and_then(Option::as_mut)
            .ok_or(DENIED_PERMIT)
    }

    fn in_use(&self, at: At, writing: bool) -> bool {
        self.handles
            .iter()
            .flatten()
            .any(|h| h.entry == at && (h.write || !writing))
    }

    /// Opens a file or directory (`sceMcOpen`), or creates a directory
    /// ([`open::CREATE_DIR`], `sceMcMkDir`): a handle, 0 for a directory
    /// created, or an error.
    pub fn open(&mut self, path: &[u8], flags: u32) -> i32 {
        let mut flags = flags;
        if flags & open::CREATE_FILE != 0 {
            flags |= u32::from(mode::WRITE);
        }
        if path.is_empty() {
            return NO_ENTRY;
        }
        let Some(fd) = self.handles.iter().position(Option::is_none) else {
            return UP_LIMIT_HANDLE;
        };
        let read = flags & u32::from(mode::READ) != 0;
        let write = flags & u32::from(mode::WRITE) != 0;
        let (parent, name, slot) = match self.lookup(path, false) {
            Lookup::Denied => return DENIED_PERMIT,
            Lookup::NoParent => return NO_ENTRY,
            Lookup::Found(at, raw) => {
                if flags & open::CREATE_DIR != 0 {
                    return NO_ENTRY;
                }
                if (write && raw.mode() & mode::WRITE == 0)
                    || (read && raw.mode() & mode::READ == 0)
                {
                    return DENIED_PERMIT;
                }
                let Some(parent) = self.record_of(at.dir) else {
                    return NO_ENTRY;
                };
                if raw.is_dir() {
                    self.handles[fd] = Some(Handle {
                        entry: at,
                        parent,
                        first: raw.cluster(),
                        size: raw.length(),
                        position: 0,
                        read: false,
                        write: false,
                        written: false,
                    });
                    return handle_number(fd);
                }
                if write && self.in_use(at, true) {
                    return DENIED_PERMIT;
                }
                if flags & open::CREATE_FILE == 0 {
                    self.handles[fd] = Some(Handle {
                        entry: at,
                        parent,
                        first: raw.cluster(),
                        size: raw.length(),
                        position: 0,
                        read: read && raw.mode() & mode::READ != 0,
                        write: write && raw.mode() & mode::WRITE != 0,
                        written: false,
                    });
                    return handle_number(fd);
                }
                // Created anew: the old file deleted, its place reused.
                if self.in_use(at, false) {
                    return DENIED_PERMIT;
                }
                self.set_exists(at, &raw, false);
                (parent, raw.name().to_vec(), at.index)
            }
            Lookup::Missing(parent, name) => {
                if flags & (open::CREATE_FILE | open::CREATE_DIR) == 0 {
                    return NO_ENTRY;
                }
                (parent, name, u32::MAX)
            }
        };
        match self.create(parent, &name, slot, flags) {
            Ok(Some(handle)) => {
                self.handles[fd] = Some(handle);
                self.changes += 1;
                handle_number(fd)
            }
            Ok(None) => {
                self.changes += 1;
                0
            }
            Err(code) => code,
        }
    }

    /// Creates an entry named `name` in the directory whose entry is at
    /// `parent`, in its first free place (or `slot`, if lower), growing
    /// the directory by one entry when none is free.
    fn create(
        &mut self,
        parent: At,
        name: &[u8],
        slot: u32,
        flags: u32,
    ) -> Result<Option<Handle>, i32> {
        let now = self.now.bytes();
        let mut dir = self.read(parent).ok_or(NO_ENTRY)?;
        let free = self
            .entries(&dir)
            .iter()
            .find(|(_, e)| !e.exists())
            .map_or(dir.length(), |(at, _)| at.index);
        let index = free.min(slot);
        let contents = dir.cluster();
        if index == dir.length() {
            if index > 0 && index % ENTRIES_PER_CLUSTER == 0 {
                let mut first = contents;
                self.nth_cluster(&mut first, index / ENTRIES_PER_CLUSTER, true)?;
            }
            dir.set_word(4, index + 1);
        }
        let new_dir = if flags & open::CREATE_DIR != 0 {
            Some(self.allocate().ok_or(FULL_DEVICE)?)
        } else {
            None
        };
        dir.0[24..32].copy_from_slice(&now);
        self.write(parent, &dir);
        let at = At {
            dir: contents,
            index,
        };
        let keeps = u16::try_from(flags & CREATE_KEEPS).unwrap_or(0);
        let base = mode::READ | mode::WRITE | mode::EXECUTE | mode::UNKNOWN_0400 | mode::EXISTS;
        if let Some(cluster) = new_dir {
            let mut raw = Raw::new(base | mode::DIR | keeps, 2, cluster, 0, now);
            raw.set_name(name);
            self.write(at, &raw);
            // Its `.` points at its entry, its `..` where the parent's `.`
            // points.
            let up = self
                .read(At {
                    dir: contents,
                    index: 0,
                })
                .ok_or(NO_ENTRY)?;
            let mut dot = Raw::new(base | mode::DIR, 0, contents, index, now);
            dot.set_name(b".");
            let mut dotdot = Raw::new(
                base | mode::DIR,
                0,
                up.cluster(),
                up.dir_entry(),
                up.created(),
            );
            dotdot.set_name(b"..");
            let page = Card::cluster_page(cluster);
            self.card.set_page(page, &dot.0);
            self.card.set_page(page + 1, &dotdot.0);
            return Ok(None);
        }
        let mut raw = Raw::new(base | mode::FILE | keeps, 0, NO_CLUSTER, 0, now);
        raw.set_name(name);
        self.write(at, &raw);
        Ok(Some(Handle {
            entry: at,
            parent,
            first: NO_CLUSTER,
            size: 0,
            position: 0,
            read: flags & u32::from(mode::READ) != 0,
            write: true,
            written: false,
        }))
    }

    /// Records a handle's file in its entry: size, first cluster, now as
    /// its and its directory's modification time; marked closed or not.
    fn record(&mut self, handle: &Handle, closed: bool) {
        let now = self.now.bytes();
        if let Some(mut raw) = self.read(handle.entry) {
            let mode = raw.mode();
            raw.set_mode(if closed {
                mode | CLOSED
            } else {
                mode & !CLOSED
            });
            raw.0[24..32].copy_from_slice(&now);
            raw.set_word(16, handle.first);
            raw.set_word(4, handle.size);
            self.write(handle.entry, &raw);
        }
        if let Some(mut dir) = self.read(handle.parent) {
            dir.0[24..32].copy_from_slice(&now);
            self.write(handle.parent, &dir);
        }
    }

    /// Closes a handle (`sceMcClose`).
    pub fn close(&mut self, fd: i32) -> i32 {
        let Ok(handle) = self.handle(fd).cloned() else {
            return DENIED_PERMIT;
        };
        if let Some(slot) = usize::try_from(fd)
            .ok()
            .and_then(|fd| self.handles.get_mut(fd))
        {
            *slot = None;
        }
        if handle.written {
            self.record(&handle, true);
            self.changes += 1;
        }
        0
    }

    /// Writes what a handle wrote to its entry, keeping it open
    /// (`sceMcFlush`).
    pub fn flush(&mut self, fd: i32) -> i32 {
        let Ok(handle) = self.handle(fd) else {
            return DENIED_PERMIT;
        };
        if std::mem::take(&mut handle.written) {
            let handle = handle.clone();
            self.record(&handle, true);
            self.changes += 1;
        }
        0
    }

    /// Moves a handle's position (`sceMcSeek`; origin 0 the start, 1 the
    /// position, 2 the end): the new position.
    pub fn seek(&mut self, fd: i32, offset: i32, origin: u32) -> i32 {
        let Ok(handle) = self.handle(fd) else {
            return DENIED_PERMIT;
        };
        let base = match origin {
            0 => 0,
            2 => i64::from(handle.size),
            _ => i64::from(handle.position),
        };
        let position = (base + i64::from(offset)).clamp(0, i64::from(i32::MAX));
        handle.position = u32::try_from(position).unwrap_or(0);
        i32::try_from(position).unwrap_or(i32::MAX)
    }

    /// Reads up to `n` bytes at a handle's position (`sceMcRead`).
    ///
    /// # Errors
    ///
    /// `mcman`'s code when the handle may not read.
    pub fn read_file(&mut self, fd: i32, n: u32) -> Result<Vec<u8>, i32> {
        let handle = self.handle(fd)?;
        if !handle.read {
            return Err(DENIED_PERMIT);
        }
        let mut handle = handle.clone();
        let n = n.min(handle.size.saturating_sub(handle.position));
        let mut data = Vec::with_capacity(n as usize);
        let mut first = handle.first;
        while data.len() < n as usize {
            let offset = handle.position as usize % CLUSTER_SIZE;
            let take = (CLUSTER_SIZE - offset).min(n as usize - data.len());
            let cluster = self.nth_cluster(&mut first, handle.position / CLUSTER_BYTES, false)?;
            data.extend_from_slice(&self.cluster_bytes(cluster)[offset..offset + take]);
            handle.position += u32::try_from(take).unwrap_or(0);
        }
        *self.handle(fd)? = handle;
        Ok(data)
    }

    fn cluster_bytes(&self, cluster: u32) -> Vec<u8> {
        let page = Card::cluster_page(cluster);
        (0..PAGES_PER_CLUSTER)
            .flat_map(|k| *self.card.page(page + k))
            .collect()
    }

    /// Writes at a handle's position (`sceMcWrite`), growing the file:
    /// the bytes written, or an error ([`result::FULL_DEVICE`]).
    pub fn write_file(&mut self, fd: i32, data: &[u8]) -> i32 {
        let Ok(handle) = self.handle(fd) else {
            return DENIED_PERMIT;
        };
        if !handle.write {
            return DENIED_PERMIT;
        }
        let mut handle = handle.clone();
        if !handle.written {
            handle.written = true;
            self.record(&handle, false);
        }
        let mut done = 0;
        while done < data.len() {
            let offset = handle.position as usize % CLUSTER_SIZE;
            let take = (CLUSTER_SIZE - offset).min(data.len() - done);
            let cluster =
                match self.nth_cluster(&mut handle.first, handle.position / CLUSTER_BYTES, true) {
                    Ok(cluster) => cluster,
                    Err(code) => {
                        // A failed write closes the handle, as in `mcman`.
                        self.record(&handle, false);
                        if let Some(slot) = usize::try_from(fd)
                            .ok()
                            .and_then(|fd| self.handles.get_mut(fd))
                        {
                            *slot = None;
                        }
                        return code;
                    }
                };
            let mut bytes = self.cluster_bytes(cluster);
            bytes[offset..offset + take].copy_from_slice(&data[done..done + take]);
            let page = Card::cluster_page(cluster);
            for (k, chunk) in bytes.as_chunks::<PAGE_SIZE>().0.iter().enumerate() {
                self.card.set_page(page + k, chunk);
            }
            done += take;
            handle.position += u32::try_from(take).unwrap_or(0);
            handle.size = handle.size.max(handle.position);
        }
        self.record(&handle, false);
        if let Ok(slot) = self.handle(fd) {
            *slot = handle;
        }
        i32::try_from(done).unwrap_or(i32::MAX)
    }

    /// Lists the entries matching a path whose last part may hold `?` and
    /// `*` (`sceMcGetDir`), at most `max`, as `sceMcTblGetDir` records.
    ///
    /// # Errors
    ///
    /// [`result::NO_ENTRY`] when the directory does not exist.
    pub fn list(&self, path: &[u8], max: usize) -> Result<Vec<[u8; TABLE_SIZE]>, i32> {
        let (dir_path, pattern): (&[u8], &[u8]) = match path.iter().rposition(|&b| b == b'/') {
            Some(0) => (b"/", &path[1..]),
            Some(k) => (&path[..k], &path[k + 1..]),
            None => (b"", path),
        };
        let Lookup::Found(at, dir) = self.lookup(dir_path, false) else {
            return Err(NO_ENTRY);
        };
        if !dir.is_dir() {
            return Err(NO_ENTRY);
        }
        // The root lists neither `.` nor `..`.
        let start = if at == self.root() { 2 } else { 0 };
        let mut table = Vec::new();
        for (entry_at, entry) in self.entries(&dir).into_iter().skip(start) {
            if table.len() >= max {
                break;
            }
            if !entry.exists()
                || entry.mode() & mode::HIDDEN != 0
                || !matches(entry.name(), pattern)
            {
                continue;
            }
            // `.` and `..` show their directories' entries.
            let shown = match entry_at.index {
                0 => self.record_of(dir.cluster()).and_then(|a| self.read(a)),
                1 => self.record_of(at.dir).and_then(|a| self.read(a)),
                _ => Some(entry.clone()),
            };
            let Some(shown) = shown else { continue };
            let mut row = [0; TABLE_SIZE];
            row[0..8].copy_from_slice(&shown.created());
            row[8..16].copy_from_slice(&shown.modified());
            if !shown.is_dir() {
                row[16..20].copy_from_slice(&shown.length().to_le_bytes());
            }
            row[20..22].copy_from_slice(&shown.mode().to_le_bytes());
            row[22..24].copy_from_slice(&shown.0[2..4]);
            let name = entry.name();
            row[32..32 + name.len()].copy_from_slice(name);
            table.push(row);
        }
        Ok(table)
    }

    /// Changes the current directory (`sceMcChDir`): the new one's path.
    ///
    /// # Errors
    ///
    /// [`result::NO_ENTRY`] or [`result::DENIED_PERMIT`].
    pub fn change_dir(&mut self, path: &[u8]) -> Result<Vec<u8>, i32> {
        let at = match self.lookup(path, false) {
            Lookup::Found(at, raw) if raw.is_dir() => at,
            Lookup::Denied => return Err(DENIED_PERMIT),
            _ => return Err(NO_ENTRY),
        };
        self.cwd = at;
        let mut names = Vec::new();
        let mut here = at;
        while let Some(raw) = self.read(here) {
            if raw.name() == b"." || names.len() > 64 {
                break;
            }
            names.push(raw.name().to_vec());
            let Some(up) = self.record_of(here.dir) else {
                break;
            };
            here = up;
        }
        let mut path = Vec::new();
        for name in names.iter().rev() {
            path.push(b'/');
            path.extend_from_slice(name);
        }
        if path.is_empty() {
            path.push(b'/');
        }
        Ok(path)
    }

    /// Marks an entry existing or deleted, its chain in use or free.
    fn set_exists(&mut self, at: At, raw: &Raw, exists: bool) {
        let mut raw = raw.clone();
        let mode = raw.mode();
        raw.set_mode(if exists {
            mode | mode::EXISTS
        } else {
            mode & !mode::EXISTS
        });
        self.write(at, &raw);
        let mut cluster = raw.cluster();
        let mut steps = 0;
        while cluster & IN_USE == 0 && steps < self.card.alloc_end() {
            let entry = self.card.fat(cluster);
            let entry = if exists {
                entry | IN_USE
            } else {
                entry & !IN_USE
            };
            self.card.set_fat(cluster, entry);
            cluster = entry & !IN_USE;
            if cluster == !IN_USE {
                break;
            }
            steps += 1;
        }
    }

    /// Deletes a file or an empty directory (`sceMcDelete`), or with
    /// `undelete` brings a deleted one back.
    pub fn delete(&mut self, path: &[u8], undelete: bool) -> i32 {
        let (at, raw) = match self.lookup(path, undelete) {
            Lookup::Found(at, raw) => (at, *raw),
            Lookup::Denied => return DENIED_PERMIT,
            _ => return NO_ENTRY,
        };
        if raw.exists() == undelete || at.index < 2 && matches!(raw.name(), b"." | b"..") {
            return NO_ENTRY;
        }
        if !undelete && raw.mode() & mode::WRITE == 0 {
            return DENIED_PERMIT;
        }
        if raw.cluster() == 0 && raw.dir_entry() == 0 {
            return NO_ENTRY;
        }
        if !undelete && raw.is_dir() && self.entries(&raw).iter().skip(2).any(|(_, e)| e.exists()) {
            return NOT_EMPTY;
        }
        if self.in_use(at, false) {
            return DENIED_PERMIT;
        }
        self.set_exists(at, &raw, undelete);
        self.changes += 1;
        0
    }

    /// Changes an entry's times, attributes or name (`sceMcSetFileInfo`)
    /// from a `sceMcTblGetDir` record.
    pub fn set_info(&mut self, path: &[u8], table: &[u8; TABLE_SIZE], flags: u32) -> i32 {
        let (at, mut raw) = match self.lookup(path, false) {
            Lookup::Found(at, raw) => (at, *raw),
            Lookup::Denied => return DENIED_PERMIT,
            // `mcman` passes its lookup's own results on.
            Lookup::Missing(..) => return 1,
            Lookup::NoParent => return 2,
        };
        let name = table[32..64].split(|&b| b == 0).next().unwrap_or_default();
        if flags & info::NAME != 0 {
            if matches!(name, b"" | b"." | b"..") || name.contains(&b'/') || at.index < 2 {
                return NO_ENTRY;
            }
            let Some(dir) = self.record_of(at.dir).and_then(|d| self.read(d)) else {
                return NO_ENTRY;
            };
            if self
                .entries(&dir)
                .iter()
                .any(|(_, e)| e.exists() && e.name() == name)
            {
                return NO_ENTRY;
            }
        }
        if flags & info::PROTECTION != 0 {
            raw.0[32..36].copy_from_slice(&table[24..28]);
        }
        if flags & (info::ATTRIBUTES | open::CREATE_FILE) != 0 {
            let wanted = u16::from_le_bytes([table[20], table[21]]);
            raw.set_mode((raw.mode() & !SETTABLE) | (wanted & SETTABLE));
        }
        if flags & info::CREATED != 0 {
            raw.0[8..16].copy_from_slice(&table[0..8]);
        }
        if flags & info::MODIFIED != 0 {
            raw.0[24..32].copy_from_slice(&table[8..16]);
        }
        if flags & info::NAME != 0 {
            raw.set_name(&name[..name.len().min(31)]);
        }
        self.write(at, &raw);
        self.changes += 1;
        0
    }

    /// Free entries in a directory (`sceMcGetEntSpace`).
    #[must_use]
    pub fn free_entries(&self, path: &[u8]) -> i32 {
        let Lookup::Found(_, dir) = self.lookup(path, false) else {
            return NO_ENTRY;
        };
        let free = self
            .entries(&dir)
            .iter()
            .filter(|(_, e)| !e.exists())
            .count();
        i32::try_from(free).unwrap_or(0) + i32::from(dir.length() & 1 == 1)
    }

    /// Formats the card (`sceMcFormat`): everything on it is lost.
    pub fn format(&mut self) -> i32 {
        self.card = Card::formatted();
        self.handles = Default::default();
        self.cwd = self.root();
        self.changes += 1;
        0
    }
}

fn handle_number(fd: usize) -> i32 {
    i32::try_from(fd).unwrap_or(i32::MAX)
}

/// Whether a name matches a pattern of `?` (one character, or the end of
/// the name, as in `mcman`) and `*` (any characters).
fn matches(name: &[u8], pattern: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|k| matches(&name[k..], rest)),
        Some((b'?', rest)) => name.split_first().is_none_or(|(_, n)| matches(n, rest)),
        Some((c, rest)) => name
            .split_first()
            .is_some_and(|(n, more)| n == c && matches(more, rest)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Time = Time {
        second: 0,
        minute: 0,
        hour: 12,
        day: 24,
        month: 8,
        year: 2002,
    };

    fn files() -> Files {
        let mut files = Files::new(Card::formatted(), NOW);
        assert_eq!(files.detect(), result::CHANGED_CARD);
        assert_eq!(files.detect(), 0);
        files
    }

    fn names(table: &[[u8; 64]]) -> Vec<String> {
        table
            .iter()
            .map(|row| {
                String::from_utf8_lossy(row[32..].split(|&b| b == 0).next().unwrap_or_default())
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn wildcards_match_as_mcman_does() {
        assert!(matches(b"BASCUS-97198", b"BASCUS-97198"));
        assert!(matches(b"BASCUS-97198", b"BASCUS*"));
        assert!(matches(b"BASCUS-97198", b"*97198"));
        assert!(matches(b"BASCUS-97198", b"BASCUS-9719?"));
        assert!(matches(b"BASCUS", b"BASCUS?"));
        assert!(!matches(b"BASCUS-97198", b"BESCUS*"));
        assert!(matches(b"anything", b"*"));
    }

    #[test]
    fn a_save_is_written_listed_and_read_back() {
        let mut files = files();
        assert_eq!(files.open(b"/BASCUS-97198", open::CREATE_DIR), 0);
        assert_eq!(files.open(b"/BASCUS-97198", open::CREATE_DIR), NO_ENTRY);
        let fd = files.open(b"/BASCUS-97198/save", open::CREATE_FILE | 3);
        assert_eq!(fd, 0);
        let data: Vec<u8> = (0..3000u32)
            .map(|i| u8::try_from(i * 7 % 256).unwrap_or(0))
            .collect();
        assert_eq!(files.write_file(fd, &data), 3000);
        assert_eq!(files.close(fd), 0);
        // The root and the directory each grew a cluster for their third
        // entry; the file takes three.
        assert_eq!(files.free_clusters_used(), 2 + 2 + 3);

        let root = files.list(b"/*", 10).unwrap_or_default();
        assert_eq!(names(&root), ["BASCUS-97198"]);
        assert_eq!(u16::from_le_bytes([root[0][20], root[0][21]]), 0x8427);
        let dir = files.list(b"/BASCUS-97198/*", 10).unwrap_or_default();
        assert_eq!(names(&dir), [".", "..", "save"]);
        assert_eq!(
            u32::from_le_bytes([dir[2][16], dir[2][17], dir[2][18], dir[2][19]]),
            3000
        );
        assert_eq!(u16::from_le_bytes([dir[2][20], dir[2][21]]), 0x8497);

        let fd = files.open(b"/BASCUS-97198/save", 1);
        assert_eq!(files.seek(fd, 1000, 0), 1000);
        assert_eq!(files.read_file(fd, 5000), Ok(data[1000..].to_vec()));
        assert_eq!(files.read_file(fd, 10), Ok(Vec::new()));
        assert_eq!(files.write_file(fd, b"x"), DENIED_PERMIT);
        assert_eq!(files.close(fd), 0);

        // The image still reads as a card, its ECC kept up.
        assert!(Card::new(files.card().image().to_vec()).is_ok());
    }

    #[test]
    fn directories_grow_and_deleted_places_are_reused() {
        let mut files = files();
        assert_eq!(files.open(b"/D", open::CREATE_DIR), 0);
        assert_eq!(files.change_dir(b"/D"), Ok(b"/D".to_vec()));
        for k in 0..5 {
            let fd = files.open(format!("f{k}").as_bytes(), open::CREATE_FILE | 2);
            assert_eq!(files.write_file(fd, &[k; 10]), 10);
            assert_eq!(files.close(fd), 0);
        }
        assert_eq!(
            names(&files.list(b"*", 10).unwrap_or_default()),
            [".", "..", "f0", "f1", "f2", "f3", "f4"]
        );
        assert_eq!(files.delete(b"/D", false), NOT_EMPTY);
        assert_eq!(files.delete(b"f1", false), 0);
        assert_eq!(files.free_entries(b"/D"), 2);
        let fd = files.open(b"g", open::CREATE_FILE | 2);
        assert_eq!(files.close(fd), 0);
        assert_eq!(
            names(&files.list(b"/D/*", 10).unwrap_or_default()),
            [".", "..", "f0", "g", "f2", "f3", "f4"]
        );
        assert_eq!(files.change_dir(b".."), Ok(b"/".to_vec()));
        let fd = files.open(b"D/f3", 1);
        assert_eq!(files.read_file(fd, 100), Ok(vec![3; 10]));
        assert_eq!(files.close(fd), 0);
        assert!(Card::new(files.card().image().to_vec()).is_ok());
    }

    #[test]
    fn creating_over_a_file_replaces_it() {
        let mut files = files();
        let fd = files.open(b"/a", open::CREATE_FILE | 2);
        assert_eq!(files.write_file(fd, &[1; 2000]), 2000);
        assert_eq!(files.close(fd), 0);
        let before = files.free_clusters_used();
        let fd = files.open(b"/a", open::CREATE_FILE | 2);
        assert_eq!(files.write_file(fd, &[2; 10]), 10);
        assert_eq!(files.close(fd), 0);
        assert_eq!(files.free_clusters_used(), before - 1);
        let fd = files.open(b"/a", 1);
        assert_eq!(files.read_file(fd, 4000), Ok(vec![2; 10]));
        // Not while it is open.
        assert_eq!(files.open(b"/a", open::CREATE_FILE | 2), DENIED_PERMIT);
        assert_eq!(files.delete(b"/a", false), DENIED_PERMIT);
        assert_eq!(files.close(fd), 0);
        assert_eq!(files.delete(b"/a", false), 0);
        assert_eq!(files.open(b"/a", 1), NO_ENTRY);
        assert_eq!(files.delete(b"/a", true), 0);
        let fd = files.open(b"/a", 1);
        assert_eq!(files.read_file(fd, 4000), Ok(vec![2; 10]));
    }

    #[test]
    fn handles_run_out_and_renames_check_names() {
        let mut files = files();
        for name in [&b"/a"[..], b"/b", b"/c"] {
            assert!(files.open(name, open::CREATE_FILE | 2) >= 0);
        }
        assert_eq!(files.open(b"/d", open::CREATE_FILE | 2), UP_LIMIT_HANDLE);
        for fd in 0..3 {
            assert_eq!(files.close(fd), 0);
        }
        let mut table = [0; 64];
        table[32] = b'b';
        assert_eq!(files.set_info(b"/a", &table, info::NAME), NO_ENTRY);
        table[32] = b'z';
        assert_eq!(files.set_info(b"/a", &table, info::NAME), 0);
        assert_eq!(
            names(&files.list(b"/*", 10).unwrap_or_default()),
            ["z", "b", "c"]
        );
    }

    impl Files {
        fn free_clusters_used(&self) -> u32 {
            self.card.alloc_end() - self.card.free_clusters()
        }
    }
}
