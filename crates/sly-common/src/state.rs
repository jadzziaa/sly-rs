//! What the game is doing, read from its memory: observables for tests and
//! reports while nothing is drawn yet. Addresses come from the Sly 1
//! decompilation (github.com/TheOnlyZac/sly1, CC0: `config/symbol_addrs.txt`,
//! `include/ui.h`, `include/joy.h`) and hold for the supported build only.

use ps2_core::Memory;

/// `g_ui.uis`: the user interface's state. `UpdateUi` switches on the word
/// at offset 4 of `g_ui` (the decompilation's `UI` struct, marked
/// approximate, has it first).
const SLY1_UI_STATE: u32 = 0x0027_5BC0 + 4;
/// `g_joy.joys`: the first joypad's state.
const SLY1_JOY_STATE: u32 = 0x0026_2C70 + 0x14;

/// `g_save`: the save system (memory card); the splash screen waits for its
/// fields 0x18 and 0xC.
const SLY1_SAVE: u32 = 0x0026_4A40;

/// `g_pgsCur`: the game state (`GS`): current world and level at 0x19D8 and
/// 0x19DC, lives at 0x19E0.
const SLY1_GAME_STATE: u32 = 0x0026_23C0;

/// Sly 1's interface states (`UIS`).
const UI_STATES: [&str; 9] = [
    "Splash",
    "Attract",
    "Playing",
    "Pausing",
    "Paused",
    "Unpausing",
    "WorldMap",
    "7",
    "Wiping",
];
/// Its joypad states (`JOYS`).
const JOY_STATES: [&str; 4] = ["Initing", "Searching", "Waiting", "Ready"];

/// Sly 1's state as far as sly-rs reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sly1State {
    /// The interface state (`UIS`), as a number.
    pub ui: i32,
    /// The first joypad's state (`JOYS`), as a number.
    pub joy: i32,
    /// `g_save`'s fields 0x18 and 0xC (meaning not decompiled yet).
    pub save: (i32, i32),
    /// The game state's world, level and lives (`gameworldCur`,
    /// `worldlevelCur`, `clife`).
    pub world: (i32, i32, i32),
}

impl Sly1State {
    /// Reads the state from the game's memory.
    pub fn read(mem: &mut Memory) -> Self {
        let gs = mem.read32(SLY1_GAME_STATE);
        let world = if gs == 0 {
            (-1, -1, -1)
        } else {
            (
                mem.read32(gs + 0x19D8).cast_signed(),
                mem.read32(gs + 0x19DC).cast_signed(),
                mem.read32(gs + 0x19E0).cast_signed(),
            )
        };
        Self {
            world,
            ui: mem.read32(SLY1_UI_STATE).cast_signed(),
            joy: mem.read32(SLY1_JOY_STATE).cast_signed(),
            save: (
                mem.read32(SLY1_SAVE + 0x18).cast_signed(),
                mem.read32(SLY1_SAVE + 0xC).cast_signed(),
            ),
        }
    }

    /// The interface state's name.
    #[must_use]
    pub fn ui_name(&self) -> &'static str {
        usize::try_from(self.ui)
            .ok()
            .and_then(|i| UI_STATES.get(i))
            .copied()
            .unwrap_or("?")
    }

    /// The joypad state's name.
    #[must_use]
    pub fn joy_name(&self) -> &'static str {
        usize::try_from(self.joy)
            .ok()
            .and_then(|i| JOY_STATES.get(i))
            .copied()
            .unwrap_or("?")
    }
}

impl std::fmt::Display for Sly1State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "interface {} ({}), joypad {} ({}), save {}/{}, world {} level {} lives {}",
            self.ui_name(),
            self.ui,
            self.joy_name(),
            self.joy,
            self.save.0,
            self.save.1,
            self.world.0,
            self.world.1,
            self.world.2
        )
    }
}
