//! The IOP side of the SIF, high-level emulated.
//!
//! The game's own SIF library (Sony's `libsif`, linked into the executable)
//! runs in the EE interpreter. It talks to the IOP with command packets it
//! sends by SIF DMA; the IOP answers with packets it writes into the EE's
//! receive buffer, then raises the SIF0 DMA interrupt, whose handler (game
//! code again) dispatches them. This crate plays the IOP's part of that
//! protocol, as Sony's `sifcmd`/`sifrpc` modules do, and hands RPC calls to
//! [`Server`]s written in Rust. Packet layouts follow the SDK's
//! (`SifCmdHeader`, `SifRpcBindPkt`, `SifRpcCallPkt`, `SifRpcRendPkt`).
//!
//! The IOP's own processor ([`IopCore`]) runs the modules the game ships
//! for itself (`libsd`, `989snd`) on a high-level emulation of the IOP
//! kernel; the system modules stay Rust servers.

// Packets, buffers and IOP addresses are small 32-bit quantities.
#![allow(
    clippy::cast_possible_truncation,
    reason = "IOP addresses and sizes are 32-bit"
)]

use std::collections::{BTreeMap, VecDeque};

use ps2_core::Memory;

pub mod cdvd;
mod core;
mod format;
mod hle;
pub mod irx;
pub mod kernel;
pub mod loadfile;
pub mod mcserv;
pub mod pad;
mod snd;
pub mod spu2;

pub use crate::core::{GuestServer, IopCore, Module, RpcRequest, Started};

/// Modules the IOP side emulates in Rust (their RPC servers): loading one
/// only records it.
const RUST_MODULES: &[&str] = &["sio2man", "padman", "mcman", "mcserv"];

/// The sound modules the Rust 989snd replaces, when it plays: `libsd` and
/// 989snd itself.
const SOUND_MODULES: &[&str] = &["Sound_Device_Library", "989snd_Library"];

/// IOP cycles the disc's 989snd takes to answer a call (its RPC thread
/// runs at the IOP's next slice: 2,343 cycles in Sly 1's traces).
const CALL_LATENCY: u64 = 2343;
/// IOP cycles it takes to load a bank (the disc read: 222,573 to 276,460
/// in Sly 1's traces).
const BANK_LATENCY: u64 = 250_000;

/// The reply header of a call [`Iop::inject_989snd`] made: its answer goes
/// nowhere.
const INJECTED: [u32; 4] = [u32::MAX; 4];

/// Whether `sid` is one of 989snd's servers.
fn is_989snd(sid: u32) -> bool {
    sid == ps2_snd989::SERVER || sid == ps2_snd989::LOADER
}

/// Logs a call to 989snd: the commands of a batch (function 0x4D: a
/// count, then `{id: u16, bytes: u16, data}` each), or a single call's
/// words.
fn trace_989snd(mem: &Memory, buffer: u32, sid: u32, function: u32, size: u32) {
    if !tracing::enabled!(target: "iop::snd989", tracing::Level::DEBUG) {
        return;
    }
    if function == 0x4D {
        let mut at = buffer + 4;
        for _ in 0..iop_word(mem, buffer).min(256) {
            let head = iop_word(mem, at);
            let (id, len) = (
                head & 0xFFFF,
                u32::try_from((head >> 16) as i16).unwrap_or(0),
            );
            let words: Vec<u32> = (0..len.div_ceil(4))
                .map(|k| iop_word(mem, at + 4 + 4 * k))
                .collect();
            tracing::debug!(target: "iop::snd989", "batch: command {id} {words:x?}");
            at += 4 + len.div_ceil(4) * 4;
        }
    } else {
        let words: Vec<u32> = (0..size.div_ceil(4).min(16))
            .map(|k| iop_word(mem, buffer + 4 * k))
            .collect();
        tracing::debug!(target: "iop::snd989", "server {sid:#x}: command {function} {words:x?}");
    }
}

/// Where module files come from: `host:` paths (the folder a test program
/// runs from) and `cdrom0:` paths (the disc).
pub trait Files {
    /// The file at `path` (`cdrom0:\LIBSD.IRX;1`, `host:iopconf.irx`).
    fn read(&self, path: &str) -> Option<Vec<u8>>;
}

/// `host:` files from a folder on the host, as PCSX2's `HostFs` serves them.
pub struct HostFiles(pub std::path::PathBuf);

impl Files for HostFiles {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let name = path.strip_prefix("host:")?.trim_start_matches(['/', '\\']);
        std::fs::read(self.0.join(name)).ok()
    }
}

/// A loader RPC waiting for its module's start function.
struct PendingLoad {
    /// The `RPC_END` packet's words 4..=7 (request id, packet, rpc id,
    /// client).
    header: [u32; 4],
    /// The EE's receive buffer and size.
    recv: (u32, u32),
}

/// Where the IOP receives command packets (its `pktbuf`), published in the
/// SMCOM mailbox at boot.
pub const IOP_PACKET_BUFFER: u32 = 0x0001_0000;
/// Where the Rust servers' receive buffers start in IOP memory, 16 KiB
/// each, up to the IOP heap (24 servers).
const SERVER_BUFFERS: u32 = 0x0002_0000;
/// The size of a Rust server's receive buffer.
const SERVER_BUFFER_SIZE: u32 = 0x4000;

/// System command ids.
pub mod cid {
    /// The other side's packet buffer moved (data: its address).
    pub const CHANGE_SADDR: u32 = 0x8000_0000;
    /// Set a software register (data: index, value).
    pub const SET_SREG: u32 = 0x8000_0001;
    /// Command layer initialisation (opt 0: the EE's buffer; opt 1: RPC).
    pub const INIT_CMD: u32 = 0x8000_0002;
    /// Reboot the IOP.
    pub const RESET: u32 = 0x8000_0003;
    /// An RPC finished (IOP to EE).
    pub const RPC_END: u32 = 0x8000_0008;
    /// Bind to an RPC server.
    pub const RPC_BIND: u32 = 0x8000_0009;
    /// Call an RPC server.
    pub const RPC_CALL: u32 = 0x8000_000A;
    /// Fetch data from IOP memory.
    pub const RPC_RDATA: u32 = 0x8000_000C;
}

/// An RPC call's input.
pub struct Call<'a> {
    /// The server id.
    pub sid: u32,
    /// The function number.
    pub function: u32,
    /// The data the EE sent.
    pub data: &'a [u8],
    /// How many bytes the EE wants back.
    pub reply_size: u32,
}

/// An RPC server on the IOP.
pub trait Server {
    /// A name for the log.
    fn name(&self) -> &'static str;
    /// Handles a call; returns the reply data (at most `reply_size` bytes
    /// are sent back). `mem` is the EE's memory, for servers that move data
    /// there directly (disc reads).
    fn call(&mut self, call: &Call<'_>, mem: &mut Memory) -> Vec<u8>;
    /// Runs at each V-blank (pads report their state then).
    fn vblank(&mut self, _mem: &mut Memory) {}
}

/// The modules behind well-known server ids, for the log.
#[must_use]
pub fn server_name(sid: u32) -> &'static str {
    match sid {
        0x8000_0001 => "fileio",
        0x8000_0006 => "loadfile",
        0x8000_0100 | 0x8000_0101 => "padman",
        0x8000_0400 => "mcserv",
        0x8000_0592..=0x8000_059A => "cdvdfsv",
        0x0012_3456 | 0x0012_3457 => "989snd",
        _ => "unknown",
    }
}

/// Whether `sid` belongs to a system module sly-rs emulates in Rust (whose
/// unhandled servers answer zeros rather than never being found).
fn emulated_in_rust(sid: u32) -> bool {
    matches!(
        sid,
        0x8000_0001 | 0x8000_0006 | 0x8000_0100 | 0x8000_0101 | 0x8000_0400 | 0x8000_0592
            ..=0x8000_059A
    )
}

/// A server not emulated yet: answers every call with zeros and logs it.
pub struct Unknown {
    sid: u32,
}

impl Server for Unknown {
    fn name(&self) -> &'static str {
        server_name(self.sid)
    }

    fn call(&mut self, call: &Call<'_>, _: &mut Memory) -> Vec<u8> {
        tracing::debug!(target: "iop::rpc", "{} ({:#x}, not emulated yet): function {} with {} bytes, {} wanted back", server_name(self.sid), self.sid, call.function, call.data.len(), call.reply_size);
        vec![0; call.reply_size as usize]
    }
}

/// One SIF DMA transfer, EE to IOP.
#[derive(Clone, Debug)]
pub struct Transfer {
    /// Source in EE memory.
    pub src: u32,
    /// Destination in IOP memory.
    pub dest: u32,
    /// Bytes.
    pub size: u32,
}

/// Something the machine must do for the IOP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// The IOP is rebooting: after a while, set the SBUS boot flags and
    /// publish the packet buffer again (the machine does both).
    Reboot,
}

/// A packet waiting to go to the EE, with extra data to copy first.
struct Outgoing {
    packet: Vec<u32>,
    extra: Option<(u32, Vec<u8>)>,
}

/// The IOP's SIF state.
pub struct Iop {
    /// The EE's packet buffer (from `INIT_CMD`).
    ee_buffer: Option<u32>,
    servers: Vec<Box<dyn Server>>,
    /// Server id to (server index, receive buffer in IOP memory).
    sids: BTreeMap<u32, (usize, u32)>,
    outgoing: VecDeque<Outgoing>,
    /// RPC calls made, by (server, function), for the report.
    pub calls: BTreeMap<(u32, u32), u64>,
    /// Commands seen, by id.
    pub commands: BTreeMap<u32, u64>,
    /// The IOP's processor.
    pub core: IopCore,
    /// What the pad in port 0 reports.
    pad: std::rc::Rc<std::cell::Cell<pad::PadInput>>,
    /// The memory card in port 0, slot 0, if one is inserted.
    card: mcserv::Slot,
    files: Option<Box<dyn Files>>,
    loads: BTreeMap<u64, PendingLoad>,
    next_load: u64,
    /// Sound is played by the Rust 989snd, not the disc's driver.
    rust_sound: bool,
    /// The machine's time (IOP cycles) as it hands over a transfer.
    now: u64,
    /// The disc's libsd, kept for the Rust driver's reverb presets.
    libsd: Option<Vec<u8>>,
}

impl Default for Iop {
    fn default() -> Self {
        Self::new()
    }
}

impl Iop {
    /// An IOP with no servers registered.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ee_buffer: None,
            servers: Vec::new(),
            sids: BTreeMap::new(),
            outgoing: VecDeque::new(),
            calls: BTreeMap::new(),
            commands: BTreeMap::new(),
            core: IopCore::new(),
            pad: std::rc::Rc::default(),
            card: mcserv::Slot::default(),
            files: None,
            loads: BTreeMap::new(),
            next_load: 0,
            rust_sound: false,
            now: 0,
            libsd: None,
        }
    }

    /// Plays sound with the Rust 989snd instead of the disc's driver: the
    /// sound modules the game loads are only recorded, and the driver's
    /// servers are answered in Rust. Call before the game loads them.
    pub fn use_rust_sound(&mut self) {
        if !self.rust_sound {
            self.rust_sound = true;
            for sid in [ps2_snd989::SERVER, ps2_snd989::LOADER] {
                self.register(&[sid], Box::new(Unknown { sid }));
            }
        }
    }

    /// Calls 989snd's command server as the EE would, with no one waiting
    /// for the answer: a check of a command the game does not send, the
    /// same on both drivers. False if the disc's driver is busy with a
    /// call of the game's (try again later).
    pub fn inject_989snd(&mut self, mem: &mut Memory, function: u32, words: &[u32]) -> bool {
        let data: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        if self.rust_sound {
            let now = self.now.max(self.core.cpu.cycles);
            if let Err(error) = self.core.run_until(&mut mem.regions, now) {
                tracing::warn!(target: "iop::snd989", "{error}");
            }
            self.core.sound_call(ps2_snd989::SERVER, function, &data, 0);
            if let Some(driver) = self.core.sound.as_mut() {
                driver.take_ee_writes();
            }
            return true;
        }
        let server = ps2_snd989::SERVER;
        let Some(buffer) = self.core.servers.get(&server).map(|g| g.buffer) else {
            return false;
        };
        if !self.core.rpc_idle(server) {
            return false;
        }
        for (i, &b) in data.iter().enumerate() {
            mem.regions.iop_ram[(buffer as usize + i) & 0x1F_FFFF] = b;
        }
        trace_989snd(mem, buffer, server, function, data.len() as u32);
        self.core.request(RpcRequest {
            sid: server,
            function,
            size: data.len() as u32,
            reply: (INJECTED, 0, 0),
        })
    }

    /// Whether the Rust 989snd plays sound.
    #[must_use]
    pub fn rust_sound(&self) -> bool {
        self.rust_sound
    }

    /// The machine's time, in IOP cycles, before it hands over SIF
    /// transfers: what the Rust driver's calls happen at.
    pub fn set_now(&mut self, cycles: u64) {
        self.now = cycles;
    }

    /// Registers the pad servers (`padman`) and the memory card server
    /// (`mcserv`).
    pub fn attach_pad(&mut self) {
        self.register(
            &pad::PADMAN,
            Box::new(pad::Padman::new(std::rc::Rc::clone(&self.pad))),
        );
        self.register(
            &[mcserv::MCSERV],
            Box::new(mcserv::Mcserv::new(std::rc::Rc::clone(&self.card))),
        );
    }

    /// Inserts a memory card in port 0, slot 0 (none is, at first), or
    /// takes it out.
    pub fn insert_card(&self, card: Option<ps2_memcard::Card>) {
        *self.card.borrow_mut() = card.map(|card| ps2_memcard::Files::new(card, cdvd::CLOCK));
    }

    /// The memory card in port 0, slot 0, and how many changes were
    /// completed on it: a new count means an image to save.
    #[must_use]
    pub fn card(&self) -> Option<(ps2_memcard::Card, u64)> {
        self.card
            .borrow()
            .as_ref()
            .map(|files| (files.card().clone(), files.changes()))
    }

    /// How many changes were completed on the memory card, if one is in.
    #[must_use]
    pub fn card_changes(&self) -> Option<u64> {
        self.card.borrow().as_ref().map(ps2_memcard::Files::changes)
    }

    /// Sets what the pad in port 0 reports from the next V-blank on.
    pub fn set_pad(&self, input: pad::PadInput) {
        self.pad.set(input);
    }

    /// V-blank: the servers that report at V-blank do (the pads).
    pub fn vblank(&mut self, mem: &mut Memory) {
        for server in &mut self.servers {
            server.vblank(mem);
        }
    }

    /// Gives the loader its module files.
    pub fn attach_files(&mut self, files: Box<dyn Files>) {
        self.files = Some(files);
    }

    /// Runs the IOP's processor until its clock reaches `until` (IOP
    /// cycles), then answers the module loads that finished.
    ///
    /// # Errors
    ///
    /// Something the IOP emulation does not support yet.
    pub fn run(&mut self, mem: &mut Memory, until: u64) -> Result<(), String> {
        self.core.run_until(&mut mem.regions, until)?;
        while let Some((_, packet, extra, writes)) = self.core.sound_ready.pop_front() {
            for (dest, bytes) in writes {
                for (i, &b) in bytes.iter().enumerate() {
                    mem.write8(dest.wrapping_add(i as u32), b);
                }
            }
            self.send(packet, extra);
        }
        while let Some(done) = self.core.started.pop_front() {
            if let Some(load) = self.loads.remove(&done.token) {
                self.answer_load(&load, done.id.cast_signed(), done.result);
            }
        }
        while let Some((request, data)) = self.core.answered.pop_front() {
            let ([a, b, c, d], recv_buf, recv_size) = request.reply;
            if [a, b, c, d] == INJECTED {
                tracing::debug!(target: "iop::snd989", "injected {} answered: {:#x}", request.function, iop_word(mem, data + 4));
                continue;
            }
            tracing::debug!(target: "iop::rpc", "{:#x} function {:#x} answered at IOP cycle {}: {recv_size} bytes from {data:#x} ({:#x}...) to EE {recv_buf:#x}", request.sid, request.function, self.core.cpu.cycles, iop_word(mem, data));
            if is_989snd(request.sid) {
                let words: Vec<u32> = (0..(recv_size / 4).min(40))
                    .map(|k| iop_word(mem, data + 4 * k))
                    .collect();
                tracing::debug!(target: "iop::snd989", "reply to {} {words:x?}", request.function);
            }
            let extra = (recv_buf != 0 && recv_size > 0 && data != 0).then(|| {
                let bytes = (0..recv_size).map(|i| iop_byte(mem, data + i)).collect();
                (recv_buf, bytes)
            });
            self.send(
                vec![
                    0,
                    0,
                    cid::RPC_END,
                    0,
                    a,
                    b,
                    c,
                    d,
                    cid::RPC_CALL,
                    request.sid,
                    0,
                    0,
                ],
                extra,
            );
        }
        Ok(())
    }

    /// Brings the SPU2 up to `until` (IOP cycles), the machine's time at a
    /// V-blank. An IOP with nothing due by then idles to it, as it would
    /// when next run.
    pub fn sound_until(&mut self, until: u64) {
        if !self.core.busy(until) {
            self.core.cpu.cycles = self.core.cpu.cycles.max(until);
        }
        let now = self.core.cpu.cycles;
        self.core.hw.now = now;
        self.core.hw.catch_up(now);
    }

    /// Keeps the SPU2's samples for [`Iop::take_samples`] from now on, or
    /// stops keeping them (the default: headless runs need no sound).
    pub fn keep_samples(&mut self, keep: bool) {
        self.core.hw.keep_samples(keep);
    }

    /// The SPU2's 48 kHz samples made since the last call, if kept.
    pub fn take_samples(&mut self) -> Vec<[i16; 2]> {
        self.core.hw.take_samples()
    }

    /// Whether the IOP's processor has something to do by `until`.
    #[must_use]
    pub fn busy(&self, until: u64) -> bool {
        self.core.busy(until)
    }

    /// Replies to a module load: the module id (or an error) and its start
    /// function's result.
    fn answer_load(&mut self, load: &PendingLoad, id: i32, result: u32) {
        let (recv_buf, recv_size) = load.recv;
        let extra = (recv_buf != 0 && recv_size > 0).then(|| {
            let mut bytes = [id.to_le_bytes(), result.to_le_bytes()].concat();
            bytes.resize(recv_size as usize, 0);
            (recv_buf, bytes)
        });
        let [a, b, c, d] = load.header;
        self.send(
            vec![
                0,
                0,
                cid::RPC_END,
                0,
                a,
                b,
                c,
                d,
                cid::RPC_CALL,
                loadfile::LOADFILE,
                0,
                0,
            ],
            extra,
        );
    }

    /// Loader function 0: load and start a module from a file. Modules the
    /// IOP side emulates in Rust are only recorded; the others run, and the
    /// reply waits for their start function.
    fn load_module(&mut self, words: &[u32], data: &[u8], mem: &mut Memory) -> bool {
        let path: Vec<u8> = data
            .iter()
            .skip(8)
            .take_while(|&&b| b != 0)
            .copied()
            .collect();
        let path = String::from_utf8_lossy(&path).into_owned();
        let Some(bytes) = self.files.as_ref().and_then(|f| f.read(&path)) else {
            return false;
        };
        let module = irx::Irx::parse(&bytes).ok();
        let name = module.as_ref().map(|m| m.name.clone()).unwrap_or_default();
        if RUST_MODULES.contains(&name.as_str()) {
            return false;
        }
        if self.rust_sound && SOUND_MODULES.contains(&name.as_str()) {
            if name == SOUND_MODULES[0] {
                self.libsd = module.map(|m| m.image);
            } else {
                self.core.start_sound(
                    module.as_ref().map(|m| m.image.as_slice()),
                    self.libsd.as_deref(),
                );
            }
            return false;
        }
        let load = PendingLoad {
            header: [words[4], words[5], words[6], words[7]],
            recv: (words[10], words[11]),
        };
        self.next_load += 1;
        let token = self.next_load;
        match self
            .core
            .load(&mut mem.regions.iop_ram, &bytes, &path, token)
        {
            Ok(_) => {
                self.loads.insert(token, load);
            }
            Err(error) => {
                tracing::warn!(target: "iop::loadfile", "{error}");
                // KE_LINKERR: the module could not be linked.
                self.answer_load(&load, -200, 0);
            }
        }
        true
    }

    /// Registers a server answering the ids `sids`.
    ///
    /// # Panics
    ///
    /// If the servers' receive buffers would reach the IOP heap (more than
    /// 24 server ids).
    pub fn register(&mut self, sids: &[u32], server: Box<dyn Server>) {
        let index = self.servers.len();
        self.servers.push(server);
        for &sid in sids {
            let buffer = SERVER_BUFFERS + SERVER_BUFFER_SIZE * self.sids.len() as u32;
            assert!(
                buffer + SERVER_BUFFER_SIZE <= kernel::HEAP_START,
                "too many RPC servers for the space below the IOP heap"
            );
            self.sids.insert(sid, (index, buffer));
        }
    }

    /// Registers the module loader, reporting `version`.
    pub fn attach_loader(&mut self, version: [u8; 4]) {
        self.register(
            &[loadfile::LOADFILE],
            Box::new(loadfile::Loadfile::new(version)),
        );
    }

    /// Registers the CD/DVD servers, reading from `disc`.
    pub fn attach_disc(&mut self, disc: std::rc::Rc<dyn cdvd::Sectors>) {
        use cdvd::{DISK_READY, INIT, NCMD, POWER_OFF, SCMD, SEARCH_FILE};
        self.register(
            &[INIT, SCMD, NCMD, POWER_OFF, SEARCH_FILE, DISK_READY],
            Box::new(cdvd::Cdvd::new(std::rc::Rc::clone(&disc))),
        );
        self.core.disc = Some(disc);
    }

    /// Handles a transfer the EE's kernel made with `sceSifSetDma` (the
    /// data is already in IOP memory). Packets to the IOP's packet buffer
    /// are commands.
    pub fn receive(&mut self, transfer: &Transfer, mem: &mut Memory) -> Option<Action> {
        tracing::trace!(target: "iop::sif", "cycle {}: from EE to {:#x}, {} bytes", self.now.max(self.core.cpu.cycles), transfer.dest, transfer.size);
        if transfer.dest & 0x1F_FFFF != IOP_PACKET_BUFFER {
            return None;
        }
        let words: Vec<u32> = (0..transfer.size / 4)
            .map(|i| iop_word(mem, IOP_PACKET_BUFFER + 4 * i))
            .collect();
        if words.len() < 4 {
            return None;
        }
        let (cid, opt) = (words[2], words[3]);
        *self.commands.entry(cid).or_default() += 1;
        tracing::debug!(target: "iop::sif", "command {cid:#x} opt {opt:#x}: {words:x?}");
        match cid {
            cid::INIT_CMD if opt == 0 => self.ee_buffer = words.get(4).copied(),
            cid::INIT_CMD => self.send(vec![0, 0, cid::SET_SREG, 0, 0, 1], None),
            cid::RPC_BIND => self.bind(&words, mem),
            cid::RPC_CALL => self.call(&words, mem),
            cid::RESET => {
                // sceSifRebootIop: the modules restart; the EE initialises the
                // command layer again once the IOP reports it has booted.
                let path: Vec<u8> = (16..transfer.size.min(96))
                    .map(|i| iop_byte(mem, IOP_PACKET_BUFFER + i))
                    .take_while(|&b| b != 0)
                    .collect();
                tracing::debug!(target: "iop::sif", "reboot with {:?}", String::from_utf8_lossy(&path));
                self.ee_buffer = None;
                self.outgoing.clear();
                return Some(Action::Reboot);
            }
            _ => tracing::warn!(target: "iop::sif", "unhandled command {cid:#x}"),
        }
        None
    }

    /// `RPC_BIND`: reply with the server's address and its receive buffer:
    /// a module's own buffer for servers modules registered, a buffer of
    /// ours for the Rust servers. A server nobody serves yet answers 0, and
    /// the EE binds again until a module registers it.
    fn bind(&mut self, words: &[u32], mem: &mut Memory) {
        let (client, sid) = (words[7], words[8]);
        let _ = mem;
        let found = if let Some(server) = self.core.servers.get(&sid) {
            Some(server.buffer)
        } else {
            if !self.sids.contains_key(&sid) && emulated_in_rust(sid) {
                self.register(&[sid], Box::new(Unknown { sid }));
            }
            self.sids.get(&sid).map(|&(_, buffer)| buffer)
        };
        tracing::debug!(target: "iop::rpc", "bind {sid:#x} ({}): {found:x?}", server_name(sid));
        let (server, buffer) = found.map_or((0, 0), |buffer| (sid, buffer));
        // RendPkt: header, rec_id, pkt_addr, rpc_id, client, cid, server, buf, cbuf.
        self.send(
            vec![
                0,
                0,
                cid::RPC_END,
                0,
                words[4],
                words[5],
                words[6],
                client,
                cid::RPC_BIND,
                server,
                buffer,
                0,
            ],
            None,
        );
    }

    /// `RPC_CALL`: run the server on the data the EE sent to its buffer and
    /// send the reply to the EE's receive buffer.
    fn call(&mut self, words: &[u32], mem: &mut Memory) {
        let (client, function, send_size, recv_buf, recv_size, server) = (
            words[7], words[8], words[9], words[10], words[11], words[13],
        );
        tracing::debug!(target: "iop::rpc", "call {server:#x} function {function:#x} at IOP cycle {}", self.core.cpu.cycles);
        if is_989snd(server) {
            let buffer = self
                .core
                .servers
                .get(&server)
                .map(|g| g.buffer)
                .or_else(|| self.sids.get(&server).map(|&(_, b)| b));
            if let Some(buffer) = buffer {
                trace_989snd(mem, buffer, server, function, send_size);
            }
        }
        if self.core.request(RpcRequest {
            sid: server,
            function,
            size: send_size,
            reply: (
                [words[4], words[5], words[6], words[7]],
                recv_buf,
                recv_size,
            ),
        }) {
            *self.calls.entry((server, function)).or_default() += 1;
            return;
        }
        let Some(&(index, buffer)) = self.sids.get(&server) else {
            tracing::warn!(target: "iop::rpc", "call to unbound server {server:#x}");
            return;
        };
        *self.calls.entry((server, function)).or_default() += 1;
        let data: Vec<u8> = (0..send_size).map(|i| iop_byte(mem, buffer + i)).collect();
        if server == loadfile::LOADFILE && function == 0 && self.load_module(words, &data, mem) {
            return;
        }
        let rust_sound = self.rust_sound && is_989snd(server);
        let reply = if rust_sound {
            // The driver's state is brought to the EE's time first: its
            // ticks due by then run, at their own cycles.
            let now = self.now.max(self.core.cpu.cycles);
            if let Err(error) = self.core.run_until(&mut mem.regions, now) {
                tracing::warn!(target: "iop::snd989", "{error}");
            }
            self.core.sound_call(server, function, &data, recv_size)
        } else {
            self.servers[index].call(
                &Call {
                    sid: server,
                    function,
                    data: &data,
                    reply_size: recv_size,
                },
                mem,
            )
        };
        let extra = (recv_buf != 0 && recv_size > 0).then(|| {
            let mut bytes = reply;
            bytes.resize(recv_size as usize, 0);
            (recv_buf, bytes)
        });
        let packet = vec![
            0,
            0,
            cid::RPC_END,
            0,
            words[4],
            words[5],
            words[6],
            client,
            cid::RPC_CALL,
            server,
            0,
            0,
        ];
        if rust_sound {
            // The disc's driver answers after its RPC thread has run (a
            // bank after its disc read): the reply waits as long.
            let latency = if server == ps2_snd989::LOADER {
                BANK_LATENCY
            } else {
                CALL_LATENCY
            };
            let writes = self
                .core
                .sound
                .as_mut()
                .map(|driver| driver.take_ee_writes())
                .unwrap_or_default();
            self.core.sound_replies.push_back((
                self.core.cpu.cycles + latency,
                packet,
                extra,
                writes,
            ));
        } else {
            self.send(packet, extra);
        }
    }

    fn send(&mut self, mut packet: Vec<u32>, extra: Option<(u32, Vec<u8>)>) {
        packet[0] = 4 * packet.len() as u32;
        self.outgoing.push_back(Outgoing { packet, extra });
    }

    /// Delivers the next packet to the EE if its buffer is free (the EE's
    /// handler clears the packet's size field when it has read it). Returns
    /// whether one was delivered: the caller raises the SIF0 DMA interrupt.
    pub fn deliver(&mut self, mem: &mut Memory) -> bool {
        let Some(buffer) = self.ee_buffer else {
            return false;
        };
        if self.outgoing.is_empty() || mem.read32(buffer) & 0xFF != 0 {
            return false;
        }
        let Some(out) = self.outgoing.pop_front() else {
            return false;
        };
        if let Some((dest, bytes)) = &out.extra {
            for (i, &b) in bytes.iter().enumerate() {
                mem.write8(dest + i as u32, b);
            }
        }
        for (i, &word) in out.packet.iter().enumerate().skip(1) {
            mem.write32(buffer + 4 * i as u32, word);
        }
        // The size field last: it marks the packet complete.
        mem.write32(buffer, out.packet[0]);
        true
    }

    /// Whether packets wait to go to the EE.
    #[must_use]
    pub fn pending(&self) -> bool {
        !self.outgoing.is_empty()
    }

    /// The packets waiting to go to the EE: command id and words 4.. .
    #[must_use]
    pub fn waiting(&self) -> Vec<(u32, Vec<u32>)> {
        self.outgoing
            .iter()
            .map(|o| (o.packet[2], o.packet[4..].to_vec()))
            .collect()
    }
}

fn iop_byte(mem: &Memory, address: u32) -> u8 {
    mem.regions.iop_ram[(address & 0x1F_FFFF) as usize]
}

fn iop_word(mem: &Memory, address: u32) -> u32 {
    let at = (address & 0x1F_FFFF) as usize;
    let b = &mem.regions.iop_ram[at..at + 4];
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
