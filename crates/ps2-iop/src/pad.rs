//! Sony's `padman` RPC servers (`0x80000100`, `0x80000101`), the newer
//! protocol the EE's `libpad` speaks: every call is RPC function 1 with the
//! command in the first word of a 128-byte buffer, and results come back in
//! the same buffer. The protocol and the pad record follow ps2sdk's
//! `libpad.c`, which documents it (read for facts, not copied).
//!
//! Pad state reaches the EE without a call: each V-blank the server writes
//! the pad's record into the area the EE gave `OPEN`, alternating its two
//! copies and advancing `frame` (the EE reads the newer copy). The pad is a
//! DualShock 2 in port 0, slot 0, stable from the start.

use std::cell::Cell;
use std::rc::Rc;

use ps2_core::Memory;

use crate::{Call, Server};

/// The servers.
pub const PADMAN: [u32; 2] = [0x8000_0100, 0x8000_0101];

/// The module version `libpad` checks (major 4): the disc's PADMAN 4.3.
const VERSION: u32 = 0x0403;

/// Commands.
mod cmd {
    pub(super) const OPEN: u32 = 0x01;
    pub(super) const SET_MAIN_MODE: u32 = 0x06;
    pub(super) const SET_ACT_DIRECT: u32 = 0x07;
    pub(super) const SET_ACT_ALIGN: u32 = 0x08;
    pub(super) const GET_BUTTON_MASK: u32 = 0x09;
    pub(super) const SET_BUTTON_INFO: u32 = 0x0A;
    pub(super) const SET_VREF: u32 = 0x0B;
    pub(super) const GET_PORT_MAX: u32 = 0x0C;
    pub(super) const GET_SLOT_MAX: u32 = 0x0D;
    pub(super) const CLOSE: u32 = 0x0E;
    pub(super) const END: u32 = 0x0F;
    pub(super) const INIT: u32 = 0x10;
    pub(super) const GET_MOD_VERSION: u32 = 0x12;
}

/// Pad states (`PAD_STATE_*`).
const STATE_STABLE: u8 = 6;
/// Request state: complete.
const REQUEST_COMPLETE: u8 = 0;

/// The buttons whose pressure a DualShock 2 reports: all eighteen bits.
const BUTTON_MASK: u32 = 0x3_FFFF;

/// Mode ids: digital, analog, analog with pressure.
const DIGITAL: u8 = 0x41;
const ANALOG: u8 = 0x73;
const PRESSURE: u8 = 0x79;

/// What a pad reports: buttons (bit set = pressed, in the DualShock's
/// order: Select, L3, R3, Start, Up, Right, Down, Left, L2, R2, L1, R1,
/// Triangle, Circle, Cross, Square) and the sticks (right x, y, left x, y;
/// 0x80 is centred).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PadInput {
    /// Pressed buttons.
    pub buttons: u16,
    /// Stick positions.
    pub sticks: [u8; 4],
}

impl Default for PadInput {
    fn default() -> Self {
        Self {
            buttons: 0,
            sticks: [0x80; 4],
        }
    }
}

/// An opened port.
#[derive(Clone, Copy, Debug)]
struct Port {
    /// The EE's area for the pad record (two copies of 128 bytes).
    area: u32,
    /// The current mode id.
    mode: u8,
    /// The mode lock `SET_MAIN_MODE` asked for.
    lock: u8,
    /// Records written.
    frame: u32,
}

/// The pad servers.
#[derive(Default)]
pub struct Padman {
    /// The EE's connection status buffer (`INIT`).
    status: u32,
    ports: [Option<Port>; 2],
    /// What port 0's pad reports, set by whoever drives the input.
    input: Rc<Cell<PadInput>>,
    /// Status records written.
    frame: u32,
}

fn word(data: &[u8], index: usize) -> u32 {
    data.get(4 * index..4 * index + 4)
        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

impl Padman {
    /// Pad servers whose port-0 pad reports `input`.
    #[must_use]
    pub fn new(input: Rc<Cell<PadInput>>) -> Self {
        Self {
            input,
            ..Self::default()
        }
    }

    /// The 128-byte pad record for `port`.
    fn record(&self, port: &Port) -> [u8; 128] {
        let mut r = [0u8; 128];
        // data[0..32]: status, mode id, buttons (active low), sticks,
        // pressures.
        let input = self.input.get();
        let pressed = input.buttons;
        r[0] = 0;
        r[1] = port.mode;
        r[2..4].copy_from_slice(&(!pressed).to_le_bytes());
        r[4..8].copy_from_slice(&input.sticks);
        // Pressure bytes: Right, Left, Up, Down, Triangle, Circle, Cross,
        // Square, L1, R1, L2, R2.
        let order = [5, 7, 4, 6, 12, 13, 14, 15, 10, 11, 8, 9];
        for (k, bit) in order.iter().enumerate() {
            r[8 + k] = if pressed & (1 << bit) != 0 { 0xFF } else { 0 };
        }
        // actData: the two motors (small, then large) and their
        // combination, as the disc's padman reports a DualShock 2 (read
        // from a PCSX2 savestate).
        r[48..52].copy_from_slice(&[1, 2, 0, 0x0A]);
        r[52..56].copy_from_slice(&[1, 1, 1, 0x14]);
        r[64..68].copy_from_slice(&[2, 0, 1, 0]);
        // modeTable: digital, analog.
        r[80..82].copy_from_slice(&u16::from(DIGITAL >> 4).to_le_bytes());
        r[82..84].copy_from_slice(&u16::from(ANALOG >> 4).to_le_bytes());
        r[88..92].copy_from_slice(&port.frame.to_le_bytes());
        // length: two bytes of header, then two per halfword the id counts.
        let length = 2 + 2 * u32::from(port.mode & 0xF);
        r[96..100].copy_from_slice(&length.to_le_bytes());
        // modeConfig 2: modes and actuators can be configured. With 1,
        // libpad reports no actuators and Sly 1 takes the pad for a
        // digital one and never asks for analog mode.
        r[100] = 2;
        r[101] = port.mode; // modeCurId
        r[102] = 3; // model: DualShock 2
        r[103] = 1; // buttonDataReady
        r[104] = 2; // nrOfModes
        r[105] = u8::from(port.mode != DIGITAL); // modeCurOffs
        r[106] = 2; // nrOfActuators
        r[107] = 1; // numActComb
        r[108] = 1; // val_c6
        r[109] = u8::from(port.mode != DIGITAL); // mode
        r[110] = port.lock;
        r[111] = 6; // actDirSize
        r[112] = STATE_STABLE;
        r[113] = REQUEST_COMPLETE;
        r[114] = 1; // currentTask
        // The buttons that report pressure: all of a DualShock 2's. The
        // disc's libpad reads this mask from the record
        // (`scePadInfoPressMode` is 1 only for 0x3FFFF), where ps2sdk's
        // layout has padding; without it Sly 1 never asks for pressure
        // mode and takes the pad for a Shock, not a Shock2.
        r[121..125].copy_from_slice(&BUTTON_MASK.to_le_bytes());
        r
    }
}

impl Server for Padman {
    fn name(&self) -> &'static str {
        "padman"
    }

    fn call(&mut self, call: &Call<'_>, _: &mut Memory) -> Vec<u8> {
        let mut reply = call.data.to_vec();
        reply.resize(128.max(call.reply_size as usize), 0);
        let mut set = |index: usize, value: u32| {
            reply[4 * index..4 * index + 4].copy_from_slice(&value.to_le_bytes());
        };
        let (command, port, slot) = (word(call.data, 0), word(call.data, 1), word(call.data, 2));
        let port_index = (port as usize).min(1);
        match command {
            cmd::GET_MOD_VERSION => set(3, VERSION),
            cmd::INIT => {
                self.status = word(call.data, 4);
                set(3, 1);
            }
            cmd::OPEN => {
                let ok = port < 2 && slot == 0;
                if ok {
                    self.ports[port_index] = Some(Port {
                        area: word(call.data, 4),
                        mode: DIGITAL,
                        lock: 0,
                        frame: 0,
                    });
                }
                set(3, u32::from(ok));
                // The IOP-side buffer, which the EE never reads.
                set(5, 0x0001_F000);
            }
            cmd::CLOSE => {
                self.ports[port_index] = None;
                set(3, 1);
            }
            cmd::SET_MAIN_MODE => {
                if let Some(p) = self.ports[port_index].as_mut() {
                    p.mode = if word(call.data, 3) == 1 {
                        ANALOG
                    } else {
                        DIGITAL
                    };
                    p.lock = word(call.data, 4) as u8;
                }
                set(5, 1);
            }
            cmd::SET_BUTTON_INFO => {
                if let Some(p) = self.ports[port_index].as_mut()
                    && p.mode != DIGITAL
                {
                    p.mode = if word(call.data, 3) != 0 {
                        PRESSURE
                    } else {
                        ANALOG
                    };
                }
                set(4, 1);
            }
            cmd::GET_BUTTON_MASK => set(3, BUTTON_MASK),
            cmd::SET_ACT_DIRECT | cmd::SET_ACT_ALIGN | cmd::SET_VREF => set(5, 1),
            cmd::GET_PORT_MAX => set(3, 2),
            cmd::GET_SLOT_MAX | cmd::END => set(3, 1),
            other => {
                tracing::warn!(target: "iop::pad", "padman command {other:#x} not implemented");
            }
        }
        tracing::debug!(target: "iop::pad", "padman command {command:#x} (port {port}, slot {slot})");
        reply.truncate(call.reply_size as usize);
        reply
    }

    fn vblank(&mut self, mem: &mut Memory) {
        for p in self.ports.iter_mut().flatten() {
            p.frame += 1;
        }
        let records: Vec<(u32, [u8; 128])> = self
            .ports
            .iter()
            .flatten()
            .map(|p| (p.area + 128 * (p.frame % 2), self.record(p)))
            .collect();
        for (at, record) in records {
            for (i, &b) in record.iter().enumerate() {
                mem.write8(at + i as u32, b);
            }
        }
        if self.status != 0 {
            // The connection status: { frame, open slots of port 0 and 1 }.
            self.frame += 1;
            let at = self.status + 128 * (self.frame % 2);
            mem.write32(at, self.frame);
            for (k, p) in self.ports.iter().enumerate() {
                mem.write32(at + 4 + 4 * k as u32, u32::from(p.is_some()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(pad: &mut Padman, mem: &mut Memory, words: &[u32]) -> Vec<u8> {
        let data: Vec<u8> = words
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .chain([0; 128])
            .take(128)
            .collect();
        pad.call(
            &Call {
                sid: PADMAN[0],
                function: 1,
                data: &data,
                reply_size: 128,
            },
            mem,
        )
    }

    #[test]
    fn a_dualshock_2_offers_its_modes_and_takes_analog() {
        let mut mem = Memory::new(Box::new(ps2_core::NoMmio));
        let mut pad = Padman::new(Rc::default());
        call(&mut pad, &mut mem, &[cmd::OPEN, 0, 0, 0, 0x1000]);
        let port = pad.ports[0].unwrap_or_else(|| panic!("port 0 opened"));
        let r = pad.record(&port);
        // modeConfig 2: libpad then reports the actuators and the modes,
        // which Sly 1 needs to take the pad for a DualShock.
        assert_eq!((r[100], r[104], r[106]), (2, 2, 2));
        assert_eq!(r[1], DIGITAL);
        // SET_MAIN_MODE analog, locked.
        call(&mut pad, &mut mem, &[cmd::SET_MAIN_MODE, 0, 0, 1, 3]);
        let port = pad.ports[0].unwrap_or_else(|| panic!("port 0 open"));
        let r = pad.record(&port);
        assert_eq!((r[1], r[101], r[109], r[110]), (ANALOG, ANALOG, 1, 3));
        // Every button reports pressure, as libpad reads it from the
        // record; SET_BUTTON_INFO then switches to pressure mode.
        assert_eq!(
            u32::from_le_bytes([r[121], r[122], r[123], r[124]]),
            0x3_FFFF
        );
        call(&mut pad, &mut mem, &[cmd::SET_BUTTON_INFO, 0, 0, 0xFFF]);
        let port = pad.ports[0].unwrap_or_else(|| panic!("port 0 open"));
        let r = pad.record(&port);
        assert_eq!((r[1], r[96]), (PRESSURE, 20));
    }
}
