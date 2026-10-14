//! The application shell: header, calculator view, run loop and keypad input,
//! and (later) toolbar and debug panels.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use egui::{
    Align, Frame, Id, Layout, Margin, Pos2, Response, RichText, Sense, WidgetInfo, WidgetType,
};

use crate::backend::{CLOCK_HZ, EmuBackend, RunStatus};
use crate::input::{KeyAction, KeyLatch, KeyboardTracker, Keymap, Source, pc_key_label};
use crate::keypad::geometry::{
    CalcTransform, KEYPAD_ORIGIN, LCD_RECT, bounds, dpad_bounds, hit_test,
};
use crate::keypad::paint::{body_rect_to_screen, keypad_rect_to_screen, paint_body, paint_key};
use crate::keypad::{KEYS, KeyId, KeyShape, key_def};
use crate::lcd::{LcdView, ScaleFilter};
use crate::pacer::{FrameRunner, Pacer, SpeedMeter, WALL_BUDGET};
use crate::theme::{APP_BG, HEADER_BG, LABEL_DARK, install_fonts};

/// The window and header title.
pub const TITLE: &str = "TI-84 Evo Emulator";
/// Height of the header bar, in points.
const HEADER_H: f32 = 44.0;
/// Size of the header title, in points.
const TITLE_SIZE: f32 = 18.0;
/// Space around the calculator inside the central panel, in points.
const CENTRAL_MARGIN: i8 = 8;
/// Duration of the key press animation, in seconds.
const PRESS_ANIM_SECS: f32 = 0.06;
/// The most UART bytes kept until the console (Task 8) takes them over.
const UART_KEEP: usize = 64 * 1024;

/// Start-up options of [`EvoApp`].
#[derive(Clone, Debug)]
pub struct AppOptions {
    /// Start with emulation paused.
    pub start_paused: bool,
    /// Where state slots, screenshots and preferences live.
    pub data_dir: PathBuf,
}

impl AppOptions {
    /// Defaults: not paused.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            start_paused: false,
            data_dir,
        }
    }
}

/// The header status line.
///
/// A stop status (breakpoint, lockup, `BKPT`) is shown even while paused,
/// since it is the reason for the pause; any other status shows `Paused`.
pub fn status_text(status: RunStatus, paused: bool, speed: Option<u32>) -> String {
    match status {
        RunStatus::Breakpoint(a) => format!("Breakpoint at 0x{a:08x}"),
        RunStatus::Lockup { pc } => format!("Lockup at 0x{pc:08x}"),
        RunStatus::Bkpt { pc, imm } => format!("BKPT #{imm} at 0x{pc:08x}"),
        _ if paused => "Paused".to_owned(),
        RunStatus::Running => match speed {
            Some(p) => format!("Running \u{b7} {p}%"),
            None => "Running".to_owned(),
        },
        RunStatus::Idle => "Idle (CPU asleep)".to_owned(),
    }
}

/// What the pointer did to one key (or the D-pad) this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointerEdge {
    None,
    Press,
    Release,
    /// Press and release in the same frame.
    Tap,
}

/// Classifies a widget's pointer state against whether it was held last frame.
fn pointer_edge(resp: &Response, was_down: bool) -> PointerEdge {
    let down = resp.is_pointer_button_down_on();
    match (was_down, down) {
        (false, true) => PointerEdge::Press,
        (true, false) => PointerEdge::Release,
        (false, false) if resp.clicked() => PointerEdge::Tap,
        _ => PointerEdge::None,
    }
}

/// The emulator application.
pub struct EvoApp {
    backend: Box<dyn EmuBackend>,
    data_dir: PathBuf,
    paused: bool,
    debug_open: bool,
    status: RunStatus,
    speed: SpeedMeter,
    pacer: Pacer,
    runner: FrameRunner,
    latch: KeyLatch,
    keymap: Keymap,
    tracker: KeyboardTracker,
    /// Rect keys the pointer holds down.
    pointer_keys: BTreeSet<KeyId>,
    /// True while the pointer button is down on the D-pad widget.
    dpad_down: bool,
    /// The wedge the current D-pad press pressed (none for a press in a gap).
    dpad_pressed: Option<KeyId>,
    /// UART output not yet shown (the console arrives in Task 8).
    uart: Vec<u8>,
    lcd: LcdView,
    filter: ScaleFilter,
    transform: CalcTransform,
}

impl EvoApp {
    /// Builds the app and installs its fonts and light theme into `cc_ctx`.
    pub fn new(cc_ctx: &egui::Context, backend: Box<dyn EmuBackend>, opts: AppOptions) -> Self {
        install_fonts(cc_ctx);
        cc_ctx.set_theme(egui::ThemePreference::Light);
        Self {
            backend,
            data_dir: opts.data_dir,
            paused: opts.start_paused,
            debug_open: false,
            status: RunStatus::Running,
            speed: SpeedMeter::default(),
            pacer: Pacer {
                clock_hz: CLOCK_HZ,
                speed: 1.0,
            },
            runner: FrameRunner::new(),
            latch: KeyLatch::new(),
            keymap: Keymap::default_map(),
            tracker: KeyboardTracker::default(),
            pointer_keys: BTreeSet::new(),
            dpad_down: false,
            dpad_pressed: None,
            uart: Vec::new(),
            lcd: LcdView::new(),
            filter: ScaleFilter::Nearest,
            transform: CalcTransform::fit(egui::Rect::ZERO),
        }
    }

    pub fn backend(&self) -> &dyn EmuBackend {
        self.backend.as_ref()
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    pub fn set_paused(&mut self, p: bool) {
        self.paused = p;
    }

    pub fn debug_open(&self) -> bool {
        self.debug_open
    }

    /// Why the last run returned.
    pub fn status(&self) -> RunStatus {
        self.status
    }

    /// Where state slots, screenshots and preferences live.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// True while `id` is held in the key latch, i.e. drawn pressed.
    pub fn key_down(&self, id: KeyId) -> bool {
        self.latch.is_down(id)
    }

    /// The calculator transform as laid out in the last frame.
    pub fn calc_transform(&self) -> CalcTransform {
        self.transform
    }

    /// Sends the latch's pending edges to the backend.
    fn flush_edges(&mut self) {
        for (key, pressed) in self.latch.take_edges() {
            self.backend.set_key(key, pressed);
        }
    }

    /// Applies one pointer edge on `key` to the latch, at the current cycle.
    fn apply_pointer(&mut self, key: KeyId, edge: PointerEdge) {
        let now = self.backend.cycles();
        match edge {
            PointerEdge::None => {}
            PointerEdge::Press => self.latch.press(key, Source::Pointer, now),
            PointerEdge::Release => self.latch.release(key, Source::Pointer, now),
            PointerEdge::Tap => {
                self.latch.press(key, Source::Pointer, now);
                self.latch.release(key, Source::Pointer, now);
            }
        }
    }

    /// Step 1 of `logic`: PC-keyboard input.
    fn keyboard(&mut self, ctx: &egui::Context) {
        let events = ctx.input(|i| i.events.clone());
        let typing = ctx.egui_wants_keyboard_input();
        let actions = self.tracker.actions(&events, &self.keymap, typing);
        let now = self.backend.cycles();
        for action in actions {
            match action {
                KeyAction::Press(id) => self.latch.press(id, Source::Keyboard, now),
                KeyAction::Release(id) => self.latch.release(id, Source::Keyboard, now),
                KeyAction::ReleaseAll => self.release_keyboard_keys(now),
            }
        }
        self.flush_edges();
    }

    /// Releases every key the keyboard holds, leaving pointer holds alone.
    fn release_keyboard_keys(&mut self, now: u64) {
        for id in KeyId::ALL {
            self.latch.release(id, Source::Keyboard, now);
        }
    }

    /// Step 2 of `logic`: emulate this frame's share of time.
    fn emulate(&mut self, ctx: &egui::Context) {
        if self.paused {
            self.speed.reset();
            return;
        }
        let (stable_dt, unstable_dt) = ctx.input(|i| (i.stable_dt, i.unstable_dt));
        let budget = self.pacer.budget(f64::from(stable_dt));
        let start = Instant::now();
        let run = self
            .runner
            .run(self.backend.as_mut(), budget, WALL_BUDGET, &mut || {
                start.elapsed()
            });
        self.status = run.status;
        if run.status.is_stop() {
            self.paused = true;
            self.debug_open = true;
            self.speed.reset();
        } else {
            self.speed.push(run.cycles, f64::from(unstable_dt));
        }
    }

    /// Step 4 of `logic`: keep the UART output (bounded).
    fn drain_uart(&mut self) {
        let bytes = self.backend.take_uart();
        self.uart.extend_from_slice(&bytes);
        if self.uart.len() > UART_KEEP {
            let excess = self.uart.len() - UART_KEEP;
            self.uart.drain(..excess);
        }
    }

    fn header(&self, ui: &mut egui::Ui) {
        let status = status_text(self.status, self.paused, self.speed.percent());
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new(TITLE).size(TITLE_SIZE).color(LABEL_DARK));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(self.backend.name()).color(LABEL_DARK));
                ui.separator();
                ui.label(RichText::new(status).color(LABEL_DARK));
            });
        });
    }

    /// The tooltip of a key: its name and PC bindings; `None` when unbound.
    fn tooltip(&self, id: KeyId) -> Option<String> {
        let keys = self.keymap.keys_for(id);
        if keys.is_empty() {
            return None;
        }
        let bindings: Vec<String> = keys.into_iter().map(pc_key_label).collect();
        Some(format!("{} \u{2014} {}", id.name(), bindings.join(", ")))
    }

    /// Accessibility info and tooltip shared by rect keys and wedges.
    fn describe(&self, resp: Response, id: KeyId) -> Response {
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, id.name()));
        match self.tooltip(id) {
            Some(text) => resp.on_hover_text(text),
            None => resp,
        }
    }

    /// The press animation value of `id` in `0.0..=1.0`.
    fn press_anim(&self, ctx: &egui::Context, id: KeyId) -> f32 {
        ctx.animate_bool_with_time(
            Id::new(("evo-key-anim", id)),
            self.key_down(id),
            PRESS_ANIM_SECS,
        )
    }

    fn calculator(&mut self, ui: &mut egui::Ui) {
        let t = CalcTransform::fit(ui.available_rect_before_wrap());
        self.transform = t;
        let painter = ui.painter().clone();
        paint_body(&painter, &t);
        self.lcd.paint(ui, body_rect_to_screen(&t, LCD_RECT));

        // Rect keys: one click-and-drag widget each, never focusable.
        let mut hovered: Option<KeyId> = None;
        for def in KEYS.iter() {
            let KeyShape::Rect(r) = def.shape else {
                continue;
            };
            let rect = keypad_rect_to_screen(&t, r);
            let resp = ui.interact(
                rect,
                Id::new(("evo-key", def.id)),
                Sense::CLICK | Sense::DRAG,
            );
            let resp = self.describe(resp, def.id);
            let was_down = self.pointer_keys.contains(&def.id);
            let edge = pointer_edge(&resp, was_down);
            if resp.is_pointer_button_down_on() {
                self.pointer_keys.insert(def.id);
            } else {
                self.pointer_keys.remove(&def.id);
            }
            self.apply_pointer(def.id, edge);
            if resp.hovered() {
                hovered = Some(def.id);
            }
        }

        // D-pad: hover-only widgets per wedge (accessibility, tooltips), then
        // one widget over the whole pad that hit-tests the wedges.
        for def in KEYS
            .iter()
            .filter(|d| matches!(d.shape, KeyShape::Wedge(_)))
        {
            let bounds = keypad_rect_to_screen(&t, bounds(def.id));
            let resp = ui.interact(bounds, Id::new(("evo-wedge", def.id)), Sense::hover());
            self.describe(resp, def.id);
        }
        let dpad = ui.interact(
            keypad_rect_to_screen(&t, dpad_bounds()),
            Id::new("evo-dpad"),
            Sense::CLICK | Sense::DRAG,
        );
        let wedge_at = |p: Option<Pos2>| {
            p.map(|p| t.to_body(p) - KEYPAD_ORIGIN.to_vec2())
                .and_then(hit_test)
                .filter(|id| matches!(key_def(*id).shape, KeyShape::Wedge(_)))
        };
        let press_pos = dpad.interact_pointer_pos().or_else(|| dpad.hover_pos());
        match pointer_edge(&dpad, self.dpad_down) {
            PointerEdge::None => {}
            PointerEdge::Press => {
                self.dpad_pressed = wedge_at(press_pos);
                if let Some(id) = self.dpad_pressed {
                    self.apply_pointer(id, PointerEdge::Press);
                }
            }
            PointerEdge::Release => {
                if let Some(id) = self.dpad_pressed.take() {
                    self.apply_pointer(id, PointerEdge::Release);
                }
            }
            PointerEdge::Tap => {
                if let Some(id) = wedge_at(press_pos) {
                    self.apply_pointer(id, PointerEdge::Tap);
                }
            }
        }
        self.dpad_down = dpad.is_pointer_button_down_on();
        if dpad.hovered() {
            hovered = hovered.or_else(|| wedge_at(dpad.hover_pos()));
        }
        self.flush_edges();

        let ctx = ui.ctx().clone();
        for def in KEYS.iter() {
            let press = self.press_anim(&ctx, def.id);
            paint_key(&painter, &t, def, press, hovered == Some(def.id));
        }
    }
}

impl eframe::App for EvoApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.keyboard(ctx);
        self.emulate(ctx);
        self.latch.tick(self.backend.cycles());
        self.flush_edges();
        self.drain_uart();
        let backlight = self.backend.backlight();
        self.lcd
            .update(ctx, self.backend.framebuffer(), backlight, self.filter);
        if !self.paused {
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("header")
            .exact_size(HEADER_H)
            .frame(
                Frame::NONE
                    .fill(HEADER_BG)
                    .inner_margin(Margin::symmetric(12, 0)),
            )
            .show(ui, |ui| self.header(ui));
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(APP_BG).inner_margin(CENTRAL_MARGIN))
            .show(ui, |ui| self.calculator(ui));
    }
}
