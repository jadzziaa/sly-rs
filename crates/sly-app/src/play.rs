//! `play`: the game in a window, in real time, with the keyboard and a
//! gamepad, recorded as a replay.
//!
//! The window's event loop owns the main thread. The machine is built and
//! run on a thread of its own (it is not `Send`), one field at a time
//! through the same step replays use ([`replay::field`]), with the pad's
//! state read once per field, so a recorded session replays exactly. After
//! each field a job on the GS's thread reads out the display and hands the
//! picture to the window, which never holds the machine back. Fields are
//! paced to NTSC's 59.94 Hz; a machine that falls behind is not made to
//! catch up in a burst. Losing focus pauses the game between fields.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use ps2_ee::{FIELD_CYCLES, Stop};
use ps2_gs::display::Frame;
use ps2_iop::pad::PadInput;
use sly_common::Game;
use sly_common::replay::{self, Checkpoint, Recorder, Replay, Script};
use sly_common::state::Sly1State;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::Recompiled;

/// A field: 1001/60000 s (NTSC).
const FIELD: Duration = Duration::from_nanos(16_683_333);

/// What `play` was asked for.
pub(crate) struct Options {
    /// The game.
    pub(crate) game: Game,
    /// Where to write the session as a replay.
    pub(crate) record: Option<PathBuf>,
    /// A replay to play through first, at full speed, before handing over.
    pub(crate) replay: Option<PathBuf>,
    /// The `[play]` settings.
    pub(crate) settings: sly_common::config::Play,
}

/// What the window and the machine's thread share.
#[derive(Default)]
struct Shared {
    /// The pad as the keyboard holds it.
    keyboard: Mutex<PadInput>,
    /// The game waits between fields (the window lost focus, or P).
    paused: AtomicBool,
    /// The window is closing.
    quit: AtomicBool,
}

/// What the machine's thread tells the window.
enum Event {
    /// A field's picture.
    Frame(u64, Frame),
    /// The machine's thread is done (with an error, if it failed).
    Done(Option<String>),
}

pub(crate) fn run(options: Options, recompiled: Option<&'static Recompiled>) -> Result<()> {
    let event_loop = EventLoop::<Event>::with_user_event()
        .build()
        .context("opening the window system")?;
    let shared = Arc::new(Shared::default());
    let proxy = event_loop.create_proxy();
    let bindings = Bindings::new(&options.settings.keys)?;
    let (window_size, pause_on_focus_loss) = (
        options.settings.window,
        options.settings.pause_on_focus_loss,
    );
    let machine = {
        let shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("machine".to_owned())
            .spawn(move || {
                let error = emulate(&options, recompiled, &shared, &proxy)
                    .err()
                    .map(|e| format!("{e:#}"));
                let _ = proxy.send_event(Event::Done(error));
            })
            .context("starting the machine's thread")?
    };
    let mut app = App {
        shared: Arc::clone(&shared),
        window: None,
        surface: None,
        frame: None,
        keys: Vec::new(),
        bindings,
        window_size,
        pause_on_focus_loss,
        error: None,
    };
    let result = event_loop.run_app(&mut app);
    shared.quit.store(true, Ordering::Relaxed);
    let _ = machine.join();
    result.context("running the window")?;
    match app.error {
        Some(error) => Err(anyhow!(error)),
        None => Ok(()),
    }
}

/// The machine's thread: boot, then field after field until the window
/// closes.
fn emulate(
    options: &Options,
    recompiled: Option<&'static Recompiled>,
    shared: &Shared,
    proxy: &EventLoopProxy<Event>,
) -> Result<()> {
    let (mut machine, _) = crate::boot(options.game, recompiled)?;
    let (script, ahead) = match &options.replay {
        Some(path) => {
            let replay = Replay::load(path)?;
            (Script::of(&replay)?, replay.fields)
        }
        None => (Script::default(), 0),
    };
    let mut gamepads = gilrs::Gilrs::new()
        .map_err(|e| tracing::warn!("no gamepads: {e}"))
        .ok();
    let mut recorder = Recorder::default();
    let mut next = Instant::now();
    while !shared.quit.load(Ordering::Relaxed) {
        if shared.paused.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
            next = Instant::now();
            continue;
        }
        let field = machine.cpu.cycles / FIELD_CYCLES;
        let live = field >= ahead;
        let pad = if live {
            let keyboard = *shared.keyboard.lock().map_err(|_| anyhow!("input lock"))?;
            combine(keyboard, gamepads.as_mut().map(gamepad))
        } else {
            script.input_at(field)
        };
        recorder.record(field, pad);
        let stop = replay::field(&mut machine, pad);
        if stop != Stop::Budget {
            return Err(anyhow!("the machine stopped at field {field}: {stop:?}"));
        }
        // While a replay plays through, show one picture in 30.
        if live || field.is_multiple_of(30) {
            let mut hw = machine.hw.borrow_mut();
            let regs = hw.gs_display();
            let proxy = proxy.clone();
            hw.gs.post(move |gs| {
                let _ = proxy.send_event(Event::Frame(field, gs.display(&regs)));
            });
        }
        if live {
            next += FIELD;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else if now - next > FIELD * 6 {
                // Behind by more than a tenth of a second: start afresh
                // rather than rush.
                next = now;
            }
        }
    }
    if let Some(path) = &options.record {
        save(path, options.game, &mut machine, &recorder)?;
    }
    Ok(())
}

/// Writes the session as a replay: its input, and a checkpoint at its end.
fn save(path: &Path, game: Game, machine: &mut ps2_ee::Machine, recorder: &Recorder) -> Result<()> {
    let fields = machine.cpu.cycles / FIELD_CYCLES;
    let state = Sly1State::read(&mut machine.mem);
    let replay = Replay {
        game: game.key().to_owned(),
        fields,
        presses: Vec::new(),
        inputs: recorder.inputs().to_vec(),
        checkpoints: vec![Checkpoint {
            field: fields,
            world: state.world.0,
            level: state.world.1,
            interface: Some(state.ui),
            hash: Some(format!("{:016x}", machine.state_hash())),
        }],
    };
    replay.save(path)?;
    tracing::info!(
        "the session is recorded in {} ({fields} fields)",
        path.display()
    );
    Ok(())
}

/// The keyboard's pad and a gamepad's together: buttons from either, each
/// stick from the gamepad when it is off centre.
fn combine(keyboard: PadInput, gamepad: Option<PadInput>) -> PadInput {
    let Some(g) = gamepad else {
        return keyboard;
    };
    let off = |v: u8| v.abs_diff(0x80) > 0x10;
    let mut sticks = keyboard.sticks;
    for axes in [0..2, 2..4] {
        if axes.clone().any(|k| off(g.sticks[k])) {
            for k in axes {
                sticks[k] = g.sticks[k];
            }
        }
    }
    PadInput {
        buttons: keyboard.buttons | g.buttons,
        sticks,
    }
}

/// The first connected gamepad's state, in the DualShock's terms.
fn gamepad(gilrs: &mut gilrs::Gilrs) -> PadInput {
    use gilrs::{Axis, Button};
    while gilrs.next_event().is_some() {}
    let Some((_, pad)) = gilrs.gamepads().find(|(_, p)| p.is_connected()) else {
        return PadInput::default();
    };
    // Bits as `sly_common::replay::BUTTONS`.
    let buttons = [
        (Button::Select, 0),
        (Button::LeftThumb, 1),
        (Button::RightThumb, 2),
        (Button::Start, 3),
        (Button::DPadUp, 4),
        (Button::DPadRight, 5),
        (Button::DPadDown, 6),
        (Button::DPadLeft, 7),
        (Button::LeftTrigger2, 8),
        (Button::RightTrigger2, 9),
        (Button::LeftTrigger, 10),
        (Button::RightTrigger, 11),
        (Button::North, 12),
        (Button::East, 13),
        (Button::South, 14),
        (Button::West, 15),
    ]
    .into_iter()
    .filter(|&(b, _)| pad.is_pressed(b))
    .fold(0u16, |m, (_, bit)| m | 1 << bit);
    // -1..1 to 0..255; the PS2's sticks read 0 at the top.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "0..=255 by construction"
    )]
    let axis = |a: Axis, flip: bool| {
        let v = pad.value(a).clamp(-1.0, 1.0);
        let v = if flip { -v } else { v };
        ((v + 1.0) * 127.5).round() as u8
    };
    PadInput {
        buttons,
        sticks: [
            axis(Axis::RightStickX, false),
            axis(Axis::RightStickY, true),
            axis(Axis::LeftStickX, false),
            axis(Axis::LeftStickY, true),
        ],
    }
}

/// The keyboard's default layout: each part of the pad, named as in the
/// settings (`[play.keys]`), and its key.
const DEFAULT_KEYS: [(&str, KeyCode); 24] = [
    ("select", KeyCode::Backspace),
    ("l3", KeyCode::KeyG),
    ("r3", KeyCode::KeyH),
    ("start", KeyCode::Enter),
    ("up", KeyCode::ArrowUp),
    ("right", KeyCode::ArrowRight),
    ("down", KeyCode::ArrowDown),
    ("left", KeyCode::ArrowLeft),
    ("l2", KeyCode::Digit1),
    ("r2", KeyCode::Digit4),
    ("l1", KeyCode::Digit2),
    ("r1", KeyCode::Digit3),
    ("triangle", KeyCode::KeyF),
    ("circle", KeyCode::KeyE),
    ("cross", KeyCode::Space),
    ("square", KeyCode::KeyQ),
    ("left-left", KeyCode::KeyA),
    ("left-right", KeyCode::KeyD),
    ("left-up", KeyCode::KeyW),
    ("left-down", KeyCode::KeyS),
    ("right-left", KeyCode::KeyJ),
    ("right-right", KeyCode::KeyL),
    ("right-up", KeyCode::KeyI),
    ("right-down", KeyCode::KeyK),
];

/// What a key does to the pad: a button (its bit), or a stick axis (right
/// x, y, left x, y) pushed to a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Button(u16),
    Stick(usize, u8),
}

/// The part of the pad a settings name means.
fn part(name: &str) -> Option<Part> {
    if let Some(bit) = replay::BUTTONS.iter().position(|b| *b == name) {
        return Some(Part::Button(1 << bit));
    }
    let (stick, direction) = name.split_once('-')?;
    let base = match stick {
        "right" => 0,
        "left" => 2,
        _ => return None,
    };
    Some(match direction {
        "left" => Part::Stick(base, 0x00),
        "right" => Part::Stick(base, 0xFF),
        "up" => Part::Stick(base + 1, 0x00),
        "down" => Part::Stick(base + 1, 0xFF),
        _ => return None,
    })
}

/// Keys a binding may name, by winit's names for them.
fn key(name: &str) -> Option<KeyCode> {
    macro_rules! keys {
        ($($k:ident)*) => {
            match name {
                $(stringify!($k) => Some(KeyCode::$k),)*
                _ => None,
            }
        };
    }
    keys!(
        KeyA KeyB KeyC KeyD KeyE KeyF KeyG KeyH KeyI KeyJ KeyK KeyL KeyM KeyN KeyO KeyP KeyQ
        KeyR KeyS KeyT KeyU KeyV KeyW KeyX KeyY KeyZ Digit0 Digit1 Digit2 Digit3 Digit4 Digit5
        Digit6 Digit7 Digit8 Digit9 ArrowUp ArrowDown ArrowLeft ArrowRight Space Enter Backspace
        Tab ShiftLeft ShiftRight ControlLeft ControlRight AltLeft AltRight CapsLock Comma Period
        Slash Semicolon Quote BracketLeft BracketRight Minus Equal Backquote Backslash F1 F2 F3
        F4 F5 F6 F7 F8 F9 F10 F11 F12 Numpad0 Numpad1 Numpad2 Numpad3 Numpad4 Numpad5 Numpad6
        Numpad7 Numpad8 Numpad9 NumpadEnter NumpadAdd NumpadSubtract Insert Delete Home End
        PageUp PageDown
    )
}

/// Which key does what.
#[derive(Clone, Debug)]
struct Bindings(Vec<(KeyCode, Part)>);

impl Bindings {
    /// The defaults with `overrides` (`[play.keys]`) in place.
    fn new(overrides: &std::collections::BTreeMap<String, String>) -> Result<Self> {
        let mut named: Vec<(String, KeyCode)> = DEFAULT_KEYS
            .iter()
            .map(|&(n, k)| (n.to_owned(), k))
            .collect();
        for (name, key_name) in overrides {
            if part(name).is_none() {
                return Err(anyhow!(
                    "[play.keys]: no pad part {name:?} (buttons as in replays, or left-up, right-down, ...)"
                ));
            }
            let code =
                key(key_name).ok_or_else(|| anyhow!("[play.keys] {name}: no key {key_name:?}"))?;
            if matches!(code, KeyCode::Escape | KeyCode::KeyP) {
                return Err(anyhow!("[play.keys] {name}: {key_name} quits or pauses"));
            }
            for entry in &mut named {
                if entry.0 == *name {
                    entry.1 = code;
                }
            }
        }
        Ok(Self(
            named
                .into_iter()
                .filter_map(|(n, k)| Some((k, part(&n)?)))
                .collect(),
        ))
    }

    /// The pad the held keys make.
    fn pad(&self, keys: &[KeyCode]) -> PadInput {
        let mut pad = PadInput::default();
        for &(k, part) in &self.0 {
            if keys.contains(&k) {
                match part {
                    Part::Button(bit) => pad.buttons |= bit,
                    Part::Stick(axis, value) => pad.sticks[axis] = value,
                }
            }
        }
        pad
    }
}

/// The window.
struct App {
    shared: Arc<Shared>,
    window: Option<Arc<Window>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    /// The latest picture and its field.
    frame: Option<(u64, Frame)>,
    /// Keys held.
    keys: Vec<KeyCode>,
    bindings: Bindings,
    /// The window's size at start (logical pixels).
    window_size: [u32; 2],
    pause_on_focus_loss: bool,
    error: Option<String>,
}

impl ApplicationHandler<Event> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("sly-rs")
            .with_inner_size(LogicalSize::new(
                f64::from(self.window_size[0]),
                f64::from(self.window_size[1]),
            ));
        let made = event_loop
            .create_window(attributes)
            .map(Arc::new)
            .map_err(|e| e.to_string())
            .and_then(|window| {
                let context =
                    softbuffer::Context::new(Arc::clone(&window)).map_err(|e| e.to_string())?;
                let surface = softbuffer::Surface::new(&context, Arc::clone(&window))
                    .map_err(|e| e.to_string())?;
                Ok((window, surface))
            });
        match made {
            Ok((window, surface)) => {
                self.window = Some(window);
                self.surface = Some(surface);
            }
            Err(e) => {
                self.error = Some(format!("opening the window: {e}"));
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Event) {
        match event {
            Event::Frame(field, frame) => {
                self.frame = Some((field, frame));
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            Event::Done(error) => {
                if error.is_some() {
                    self.error = error;
                }
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                // The machine's thread writes the recording, then says it
                // is done.
                self.shared.quit.store(true, Ordering::Relaxed);
            }
            WindowEvent::Focused(focused) => {
                if self.pause_on_focus_loss {
                    self.shared.paused.store(!focused, Ordering::Relaxed);
                }
                if !focused {
                    self.keys.clear();
                    self.input();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                match (code, event.state) {
                    (KeyCode::Escape, ElementState::Pressed) => {
                        self.shared.quit.store(true, Ordering::Relaxed);
                    }
                    (KeyCode::KeyP, ElementState::Pressed) if !event.repeat => {
                        let paused = self.shared.paused.load(Ordering::Relaxed);
                        self.shared.paused.store(!paused, Ordering::Relaxed);
                    }
                    (code, ElementState::Pressed) => {
                        if !self.keys.contains(&code) {
                            self.keys.push(code);
                        }
                        self.input();
                    }
                    (code, ElementState::Released) => {
                        self.keys.retain(|&k| k != code);
                        self.input();
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.draw() {
                    self.error = Some(e);
                    self.shared.quit.store(true, Ordering::Relaxed);
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }
}

impl App {
    fn input(&self) {
        if let Ok(mut pad) = self.shared.keyboard.lock() {
            *pad = self.bindings.pad(&self.keys);
        }
    }

    /// Shows the latest picture: each field's lines doubled (the odd
    /// field one line lower), at 4:3 in the window, black around it.
    fn draw(&mut self) -> Result<(), String> {
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
            return Ok(());
        };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (
            std::num::NonZeroU32::new(size.width),
            std::num::NonZeroU32::new(size.height),
        ) else {
            return Ok(());
        };
        surface.resize(w, h).map_err(|e| e.to_string())?;
        let mut buffer = surface.buffer_mut().map_err(|e| e.to_string())?;
        buffer.fill(0);
        if let Some((field, frame)) = &self.frame {
            paint(
                &mut buffer,
                (size.width, size.height),
                frame,
                field % 2 == 1,
            );
        }
        buffer.present().map_err(|e| e.to_string())
    }
}

/// Paints `frame` (one field) into `buffer` (`width` × `height`, 0RGB):
/// lines doubled, the odd field one line lower, at 4:3, centred.
fn paint(buffer: &mut [u32], (width, height): (u32, u32), frame: &Frame, odd: bool) {
    let (fw, fh) = (frame.width as usize, frame.height as usize);
    if fw == 0 || fh == 0 {
        return;
    }
    let (width, height) = (width as usize, height as usize);
    // A field is half the picture's lines; a frame-mode picture is whole.
    let lines = if fh <= 256 { fh * 2 } else { fh };
    let (tw, th) = if width * 3 >= height * 4 {
        (height * 4 / 3, height)
    } else {
        (width, width * 3 / 4)
    };
    let (ox, oy) = ((width - tw) / 2, (height - th) / 2);
    for y in 0..th {
        let line = y * lines / th;
        let row = if lines == fh * 2 {
            // Bob: picture line `line` comes from field line (line - odd) / 2.
            (line.saturating_sub(usize::from(odd)) / 2).min(fh - 1)
        } else {
            line.min(fh - 1)
        };
        let source = &frame.rgb[row * fw * 3..(row + 1) * fw * 3];
        let target = &mut buffer[(oy + y) * width + ox..(oy + y) * width + ox + tw];
        for (x, pixel) in target.iter_mut().enumerate() {
            let s = (x * fw / tw) * 3;
            *pixel = u32::from(source[s]) << 16
                | u32::from(source[s + 1]) << 8
                | u32::from(source[s + 2]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_make_the_pad() {
        let bindings = Bindings::new(&std::collections::BTreeMap::new()).unwrap();
        let pad = bindings.pad(&[KeyCode::Space, KeyCode::KeyW, KeyCode::KeyL, KeyCode::Enter]);
        assert_eq!(pad.buttons, 1 << 14 | 1 << 3);
        assert_eq!(pad.sticks, [0xFF, 0x80, 0x80, 0x00]);
        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert("cross".to_owned(), "KeyK".to_owned());
        let bindings = Bindings::new(&overrides).unwrap();
        assert_eq!(bindings.pad(&[KeyCode::KeyK]).buttons, 1 << 14);
        assert_eq!(bindings.pad(&[KeyCode::Space]).buttons, 0);
        overrides.insert("jump".to_owned(), "KeyZ".to_owned());
        assert!(Bindings::new(&overrides).is_err());
    }

    #[test]
    fn a_gamepad_stick_off_centre_wins() {
        let keyboard = PadInput {
            buttons: 1,
            sticks: [0x80, 0x80, 0x00, 0x80],
        };
        let gamepad = PadInput {
            buttons: 2,
            sticks: [0xF0, 0x80, 0x80, 0x82],
        };
        let pad = combine(keyboard, Some(gamepad));
        assert_eq!(pad.buttons, 3);
        assert_eq!(pad.sticks, [0xF0, 0x80, 0x00, 0x80]);
    }

    #[test]
    fn fields_are_line_doubled_at_four_by_three() {
        let frame = Frame {
            width: 2,
            height: 2,
            rgb: vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255],
        };
        let mut buffer = vec![0u32; 8 * 4];
        paint(&mut buffer, (8, 4), &frame, false);
        // 4:3 in 8 × 4 is 5 × 4 (rounded down), starting at column 1.
        assert_eq!(buffer[1], 0xFF_0000);
        assert_eq!(buffer[4 * 8 - 1 - 2], 0xFF_FFFF);
        assert_eq!(buffer[0], 0);
    }
}
