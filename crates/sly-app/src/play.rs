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
use crate::screen::Screen;

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
    Frame(Field),
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
    GamepadMap::new(&options.settings.gamepad)?;
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
        screen: None,
        fields: Vec::new(),
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
    if options.settings.gpu {
        let name = crate::use_gpu(&machine)?;
        tracing::info!("drawing on the GPU: {name}");
    }
    let (script, ahead) = match &options.replay {
        Some(path) => {
            let replay = Replay::load(path)?;
            (Script::of(&replay)?, replay.fields)
        }
        None => (Script::default(), 0),
    };
    let gamepad_map = GamepadMap::new(&options.settings.gamepad)?;
    let mut gamepads = gilrs::Gilrs::new()
        .map_err(|e| tracing::warn!("no gamepads: {e}"))
        .ok();
    for (_, pad) in gamepads.iter().flat_map(gilrs::Gilrs::gamepads) {
        report(&pad);
    }
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
            combine(
                keyboard,
                gamepads.as_mut().map(|g| gamepad(g, &gamepad_map)),
            )
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
            // CSR's FIELD: the field the display shows next, as the game's
            // V-blank handler has just set it up.
            let odd = hw.gs_csr >> 13 & 1 == 1;
            let proxy = proxy.clone();
            hw.gs.post(move |gs| {
                gs.show(&regs, move |frame| {
                    let _ = proxy.send_event(Event::Frame(Field { frame, odd }));
                });
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

/// Says which gamepad is in use and how its buttons are placed.
fn report(pad: &gilrs::Gamepad<'_>) {
    let placed = match pad.mapping_source() {
        gilrs::MappingSource::SdlMappings => "buttons placed by SDL's controller database",
        _ => {
            "not in SDL's controller database: buttons as its driver reports them; \
             if they are in the wrong places, see [play.gamepad] in sly-rs.example.toml"
        }
    };
    println!("gamepad: {} ({placed})", pad.name());
}

/// The gamepad's buttons, by gilrs's names, and the DualShock button
/// (its bit, as `sly_common::replay::BUTTONS`) each gives.
const DEFAULT_GAMEPAD: [(&str, gilrs::Button, &str); 16] = [
    ("select", gilrs::Button::Select, "select"),
    ("l3", gilrs::Button::LeftThumb, "l3"),
    ("r3", gilrs::Button::RightThumb, "r3"),
    ("start", gilrs::Button::Start, "start"),
    ("up", gilrs::Button::DPadUp, "up"),
    ("right", gilrs::Button::DPadRight, "right"),
    ("down", gilrs::Button::DPadDown, "down"),
    ("left", gilrs::Button::DPadLeft, "left"),
    ("l2", gilrs::Button::LeftTrigger2, "l2"),
    ("r2", gilrs::Button::RightTrigger2, "r2"),
    ("l1", gilrs::Button::LeftTrigger, "l1"),
    ("r1", gilrs::Button::RightTrigger, "r1"),
    ("north", gilrs::Button::North, "triangle"),
    ("east", gilrs::Button::East, "circle"),
    ("south", gilrs::Button::South, "cross"),
    ("west", gilrs::Button::West, "square"),
];

/// Which DualShock button each gamepad button gives.
struct GamepadMap(Vec<(gilrs::Button, u16)>);

impl GamepadMap {
    /// The defaults (by position: the bottom face button is Cross) with
    /// `overrides` (`[play.gamepad]`) in place.
    fn new(overrides: &std::collections::BTreeMap<String, String>) -> Result<Self> {
        for name in overrides.keys() {
            if !DEFAULT_GAMEPAD.iter().any(|&(n, _, _)| n == name) {
                return Err(anyhow!(
                    "[play.gamepad]: no gamepad button {name:?} (south, east, north, west, l1, r1, l2, r2, l3, r3, select, start, up, down, left, right)"
                ));
            }
        }
        DEFAULT_GAMEPAD
            .iter()
            .map(|&(name, button, default)| {
                let target = overrides.get(name).map_or(default, String::as_str);
                match part(target) {
                    Some(Part::Button(bit)) => Ok((button, bit)),
                    _ => Err(anyhow!(
                        "[play.gamepad]: {name} = {target:?} is not a DualShock button (cross, square, triangle, circle, l1, ...)"
                    )),
                }
            })
            .collect::<Result<_>>()
            .map(Self)
    }

    /// The DualShock buttons `pressed` gives.
    fn buttons(&self, pressed: impl Fn(gilrs::Button) -> bool) -> u16 {
        self.0
            .iter()
            .filter(|&&(b, _)| pressed(b))
            .fold(0, |m, &(_, bit)| m | bit)
    }
}

/// The first connected gamepad's state, in the DualShock's terms.
fn gamepad(gilrs: &mut gilrs::Gilrs, map: &GamepadMap) -> PadInput {
    use gilrs::Axis;
    while let Some(event) = gilrs.next_event() {
        if matches!(event.event, gilrs::EventType::Connected) {
            report(&gilrs.gamepad(event.id));
        }
    }
    let Some((_, pad)) = gilrs.gamepads().find(|(_, p)| p.is_connected()) else {
        return PadInput::default();
    };
    let buttons = map.buttons(|b| pad.is_pressed(b));
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
    screen: Option<Screen>,
    /// The latest fields, newest last (at most three).
    fields: Vec<Field>,
    /// The picture they make.
    frame: Option<Frame>,
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
        if self.screen.is_some() {
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
            .and_then(|window| Screen::new(event_loop, window));
        match made {
            Ok(screen) => self.screen = Some(screen),
            Err(e) => {
                self.error = Some(format!("opening the window: {e}"));
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Event) {
        match event {
            Event::Frame(field) => {
                if self.fields.len() == 3 {
                    self.fields.remove(0);
                }
                self.fields.push(field);
                self.frame = Some(deinterlace(&self.fields));
                if let Some(screen) = &self.screen {
                    screen.window().request_redraw();
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

    /// Shows the latest picture at 4:3 in the window, black around it.
    fn draw(&mut self) -> Result<(), String> {
        match &mut self.screen {
            Some(screen) => screen.draw(self.frame.as_ref()),
            None => Ok(()),
        }
    }
}

/// A field as the display showed it.
struct Field {
    frame: Frame,
    /// The odd field: its lines sit one line lower in the picture.
    odd: bool,
}

/// Row `index` of a picture.
fn row(frame: &Frame, index: usize) -> &[u8] {
    let stride = frame.width as usize * 3;
    &frame.rgb[index * stride..(index + 1) * stride]
}

/// The difference (summed over red, green and blue, above and below)
/// under which a pixel counts as unchanged between two fields of the same
/// parity.
const STILL: u32 = 24;

/// The picture the latest of `fields` (newest last) makes, deinterlaced
/// adaptively, roughly as PCSX2's default does: the latest field's lines
/// in their rows (the odd field's one row lower); each row between them
/// woven in from the field before where the picture is still, and
/// interpolated from the latest field's rows above and below where it
/// moves. Still means the latest field's rows above and below match the
/// field two before (the same parity). Still 2D then does not bob, still
/// 3D shows all 448 lines, and moving edges do not comb. A frame-mode
/// picture is already whole.
fn deinterlace(fields: &[Field]) -> Frame {
    let Some(latest) = fields.last() else {
        return Frame {
            width: 0,
            height: 0,
            rgb: Vec::new(),
        };
    };
    let (width, height) = (latest.frame.width as usize, latest.frame.height as usize);
    if height > 256 || width == 0 || height == 0 {
        return latest.frame.clone();
    }
    let same =
        |f: &Field| f.frame.width == latest.frame.width && f.frame.height == latest.frame.height;
    let back = |k: usize| {
        fields
            .len()
            .checked_sub(k)
            .map(|k| &fields[k])
            .filter(|f| same(f))
    };
    // The field before (the other parity) and the one before that (this
    // parity again).
    let before = back(2).filter(|f| f.odd != latest.odd);
    let earlier = back(3).filter(|f| before.is_some() && f.odd == latest.odd);
    let own = usize::from(latest.odd);
    let mut rgb = Vec::with_capacity(width * height * 2 * 3);
    for line in 0..height * 2 {
        let index = line / 2;
        if line % 2 == own {
            rgb.extend_from_slice(row(&latest.frame, index));
            continue;
        }
        // The latest field's rows above and below this line.
        let (up, down) = if latest.odd {
            (index.saturating_sub(1), index)
        } else {
            (index, (index + 1).min(height - 1))
        };
        let (above, below) = (row(&latest.frame, up), row(&latest.frame, down));
        let was = earlier.map(|f| (row(&f.frame, up), row(&f.frame, down)));
        let woven = before.map(|f| row(&f.frame, index));
        for x in 0..width {
            let px = 3 * x..3 * x + 3;
            let differ = |a: &[u8], b: &[u8]| -> u32 {
                a[px.clone()]
                    .iter()
                    .zip(&b[px.clone()])
                    .map(|(&p, &q)| u32::from(p.abs_diff(q)))
                    .sum()
            };
            let still = was.is_some_and(|(a, b)| differ(above, a) + differ(below, b) <= STILL);
            match woven {
                Some(n) if still => rgb.extend_from_slice(&n[px]),
                _ => rgb.extend(
                    above[px.clone()]
                        .iter()
                        .zip(&below[px])
                        .map(|(&a, &b)| a.midpoint(b)),
                ),
            }
        }
    }
    Frame {
        width: latest.frame.width,
        height: latest.frame.height * 2,
        rgb,
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
    fn gamepad_buttons_go_by_position_and_can_be_remapped() {
        use gilrs::Button;
        let map = GamepadMap::new(&std::collections::BTreeMap::new()).unwrap();
        assert_eq!(
            map.buttons(|b| b == Button::South),
            1 << 14,
            "bottom: Cross"
        );
        assert_eq!(map.buttons(|b| b == Button::West), 1 << 15, "left: Square");
        // A pad whose driver reports its left and top buttons swapped.
        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert("north".to_owned(), "square".to_owned());
        overrides.insert("west".to_owned(), "triangle".to_owned());
        let map = GamepadMap::new(&overrides).unwrap();
        assert_eq!(map.buttons(|b| b == Button::North), 1 << 15);
        assert_eq!(map.buttons(|b| b == Button::West), 1 << 12);
        overrides.insert("paddle".to_owned(), "cross".to_owned());
        assert!(GamepadMap::new(&overrides).is_err());
        overrides.remove("paddle");
        overrides.insert("south".to_owned(), "left-up".to_owned());
        assert!(GamepadMap::new(&overrides).is_err());
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

    fn field(rows: &[u8], odd: bool) -> Field {
        Field {
            frame: Frame {
                width: 1,
                height: u32::try_from(rows.len()).unwrap_or(0),
                rgb: rows.iter().flat_map(|&v| [v, v, v]).collect(),
            },
            odd,
        }
    }

    fn rows(frame: &Frame) -> Vec<u8> {
        frame.rgb.iter().step_by(3).copied().collect()
    }

    #[test]
    fn still_fields_weave_and_do_not_bob() {
        // The same picture in every field: the odd field's rows land one
        // line lower, and the rows between come from the other field, so
        // the picture is identical whichever field is the latest.
        let still = [10, 200, 30];
        let even = deinterlace(&[
            field(&still, false),
            field(&still, true),
            field(&still, false),
        ]);
        let odd = deinterlace(&[
            field(&still, true),
            field(&still, false),
            field(&still, true),
        ]);
        assert_eq!(rows(&even), [10, 10, 200, 200, 30, 30]);
        assert_eq!(rows(&odd), rows(&even));
    }

    #[test]
    fn moving_fields_interpolate_instead_of_combing() {
        // The latest field differs from the one two before: the field
        // before is not woven in; the latest field's neighbours are
        // averaged.
        let latest = field(&[0, 100, 200], false);
        let picture = deinterlace(&[field(&[9, 9, 9], false), field(&[50, 50, 50], true), latest]);
        assert_eq!(rows(&picture), [0, 50, 100, 150, 200, 200]);
        // With no history, the same.
        let alone = deinterlace(&[field(&[0, 100, 200], true)]);
        assert_eq!(rows(&alone), [0, 0, 50, 100, 150, 200]);
    }
}
