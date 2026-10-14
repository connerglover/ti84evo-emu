//! The application shell: header, toolbar, calculator view, run loop and
//! keypad input, and debug panels.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use egui::{
    Align, Frame, Id, Layout, Margin, Pos2, Rect, Response, RichText, Sense, Vec2, ViewportCommand,
    WidgetInfo, WidgetType, pos2, vec2,
};

use crate::backend::{CLOCK_HZ, EmuBackend, RunStatus};
use crate::debug::{DEBUG_MIN_W, DEBUG_W, DebugPanel};
use crate::input::{KeyAction, KeyLatch, KeyboardTracker, Keymap, Source, pc_key_label};
use crate::keymap_window::KeymapWindow;
use crate::keypad::geometry::{
    BODY_W, CalcTransform, KEYPAD_ORIGIN, LCD_RECT, bounds, dpad_bounds, hit_test,
};
use crate::keypad::paint::{body_rect_to_screen, keypad_rect_to_screen, paint_body, paint_key};
use crate::keypad::{KEYS, KeyId, KeyShape, key_def};
use crate::lcd::{LcdView, ScaleFilter};
use crate::pacer::{FrameRunner, Pacer, SpeedMeter, WALL_BUDGET};
use crate::prefs::{PREFS_KEY, Prefs};
use crate::store::{Store, StoreError};
use crate::theme::{APP_BG, HEADER_BG, LABEL_DARK, install_fonts};
use crate::toast::{ToastKind, Toasts};
use crate::toolbar::{
    PickerMode, TOOLBAR_W, Tool, about_window, drawable_icons, slot_picker, toolbar_buttons,
    toolbar_frame,
};

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
/// The debug panel's egui id.
const DEBUG_PANEL_ID: &str = "debug";
/// How many frames to wait for the window to reach the size asked for.
const GROW_WAIT_FRAMES: u32 = 30;
/// The smallest calculator scale the debug panel may squeeze the view to.
const MIN_CALC_SCALE_WITH_DEBUG: f32 = 0.6;

/// The widest the debug panel may be in a window `window_w` points wide:
/// what is left after the toolbar, the central margins and a calculator at
/// [`MIN_CALC_SCALE_WITH_DEBUG`]. Never negative.
pub fn debug_max_width(window_w: f32) -> f32 {
    let reserved = TOOLBAR_W + 2.0 * f32::from(CENTRAL_MARGIN) + BODY_W * MIN_CALC_SCALE_WITH_DEBUG;
    (window_w - reserved).max(0.0)
}

/// The toast shown when no bundle was given and the demo backend runs.
pub const DEMO_NOTICE: &str =
    "No OS bundle given: running the demo backend. Usage: evo-gui <bundle.84b2>";

/// Which backend `main` starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// The fake backend; `notice` shows [`DEMO_NOTICE`] because no bundle was found.
    Demo { notice: bool },
    /// The emulated machine, booted from this bundle.
    Bundle(PathBuf),
}

/// Picks the backend: `--demo`, else the bundle argument, else `EVO_OS_IMAGE` (`env`; empty
/// counts as unset), else the demo with a notice.
pub fn choose_backend(bundle_arg: Option<PathBuf>, demo: bool, env: Option<OsString>) -> Choice {
    if demo {
        return Choice::Demo { notice: false };
    }
    match bundle_arg.or_else(|| env.filter(|e| !e.is_empty()).map(PathBuf::from)) {
        Some(path) => Choice::Bundle(path),
        None => Choice::Demo { notice: true },
    }
}

/// Start-up options of [`EvoApp`].
#[derive(Clone, Debug)]
pub struct AppOptions {
    /// Start with emulation paused.
    pub start_paused: bool,
    /// Where state slots, screenshots and preferences live.
    pub data_dir: PathBuf,
    /// Preferences loaded from the last run.
    pub prefs: Prefs,
    /// Shown as an Info toast on the first frame.
    pub notice: Option<String>,
}

impl AppOptions {
    /// Defaults: not paused, default preferences, no notice.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            start_paused: false,
            data_dir,
            prefs: Prefs::default(),
            notice: None,
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
    /// Debug panel state, including the UART console.
    debug: DebugPanel,
    /// Whether the debug panel was open when the window size was last synced.
    debug_shown: bool,
    /// True when opening the debug panel grew the window by `DEBUG_W`.
    grown: bool,
    /// While the window grows: the width asked for and how many more frames
    /// to wait for it. The panel forgets its width meanwhile, so it opens at
    /// `DEBUG_W` once there is room instead of keeping the narrow first frame's.
    grow_target: Option<(f32, u32)>,
    lcd: LcdView,
    filter: ScaleFilter,
    transform: CalcTransform,
    store: Store,
    toasts: Toasts,
    /// Per tool, whether its icon is drawable; filled on the first frame,
    /// once the fonts are loaded.
    icons: Option<[bool; 8]>,
    /// The open slot picker and where it sits.
    picker: Option<(PickerMode, Pos2)>,
    about_open: bool,
    keymap_window: KeymapWindow,
    /// The toolbar panel as laid out in the last frame.
    toolbar_rect: Rect,
}

impl EvoApp {
    /// Builds the app and installs its fonts and light theme into `cc_ctx`.
    pub fn new(cc_ctx: &egui::Context, backend: Box<dyn EmuBackend>, opts: AppOptions) -> Self {
        install_fonts(cc_ctx);
        cc_ctx.set_theme(egui::ThemePreference::Light);
        let mut toasts = Toasts::new();
        if let Some(notice) = opts.notice {
            toasts.push(notice, ToastKind::Info);
        }
        Self {
            backend,
            store: Store::new(opts.data_dir.clone()),
            toasts,
            icons: None,
            picker: None,
            about_open: false,
            keymap_window: KeymapWindow::new(),
            toolbar_rect: Rect::NOTHING,
            data_dir: opts.data_dir,
            paused: opts.start_paused,
            debug_open: opts.prefs.debug_open,
            status: RunStatus::Running,
            speed: SpeedMeter::default(),
            pacer: Pacer {
                clock_hz: CLOCK_HZ,
                speed: 1.0,
            },
            runner: FrameRunner::new(),
            latch: KeyLatch::new(),
            keymap: opts.prefs.keymap,
            tracker: KeyboardTracker::default(),
            pointer_keys: BTreeSet::new(),
            dpad_down: false,
            dpad_pressed: None,
            debug: DebugPanel::new(),
            // eframe restores the saved window, which already includes the
            // panel when it was open at quit: do not grow it again, but do
            // give its width back when the panel closes.
            debug_shown: opts.prefs.debug_open,
            grown: opts.prefs.debug_open,
            grow_target: None,
            lcd: LcdView::new(),
            filter: opts.prefs.filter,
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

    /// How the LCD frame is scaled.
    pub fn filter(&self) -> ScaleFilter {
        self.filter
    }

    /// The preferences to save: current keymap, filter and debug-open state.
    pub fn prefs(&self) -> Prefs {
        Prefs {
            keymap: self.keymap.clone(),
            filter: self.filter,
            debug_open: self.debug_open,
        }
    }

    /// True while the keyboard-mapping window waits for a key to bind.
    pub fn keymap_capturing(&self) -> bool {
        self.keymap_window.capturing()
    }

    /// Writes flash to disk; a failure becomes a toast.
    fn persist(&mut self) {
        if let Err(e) = self.backend.persist() {
            self.toasts
                .push(format!("Flash not saved: {e}"), ToastKind::Error);
        }
    }

    /// Brings the GUI back in line with a machine that was reset or loaded:
    /// drops keys restored as held and releases pending on the old timeline.
    fn resync(&mut self) {
        self.latch.clear();
        for id in KeyId::ALL {
            self.backend.set_key(id, false);
        }
        self.status = RunStatus::Running;
        self.speed.reset();
    }

    fn reset_machine(&mut self) {
        self.persist();
        self.backend.reset();
        self.resync();
        self.toasts.push("Reset", ToastKind::Info);
    }

    fn save_slot(&mut self, n: u8) {
        match self.store.save_slot(n, self.backend.as_ref()) {
            Ok(_) => self.toasts.push(format!("Saved slot {n}"), ToastKind::Info),
            Err(e) => self
                .toasts
                .push(format!("Save failed: {e}"), ToastKind::Error),
        }
    }

    fn load_slot(&mut self, n: u8) {
        self.persist();
        match self.store.load_slot(n, self.backend.as_mut()) {
            Ok(()) => {
                self.resync();
                self.toasts
                    .push(format!("Loaded slot {n}"), ToastKind::Info);
            }
            Err(e) => self
                .toasts
                .push(format!("Load failed: {e}"), ToastKind::Error),
        }
    }

    fn screenshot(&mut self) {
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let result = match self.backend.framebuffer() {
            Some(frame) => self.store.screenshot(frame, unix_ms),
            None => Err(StoreError::NoFrame),
        };
        match result {
            Ok(path) => self.toasts.push(
                format!("Screenshot saved to {}", path.display()),
                ToastKind::Info,
            ),
            Err(e) => self
                .toasts
                .push(format!("Screenshot failed: {e}"), ToastKind::Error),
        }
    }

    /// Opens the slot picker in `mode` beside `button`, or closes it when it
    /// is already open in that mode.
    fn toggle_picker(&mut self, mode: PickerMode, button: Rect) {
        self.picker = match self.picker {
            Some((open, _)) if open == mode => None,
            _ => Some((mode, pos2(self.toolbar_rect.right(), button.top()))),
        };
    }

    fn apply_tool(&mut self, tool: Tool, button: Rect) {
        match tool {
            Tool::Screenshot => self.screenshot(),
            Tool::SaveState => self.toggle_picker(PickerMode::Save, button),
            Tool::LoadState => self.toggle_picker(PickerMode::Load, button),
            Tool::Reset => self.reset_machine(),
            Tool::Smooth => {
                self.filter = match self.filter {
                    ScaleFilter::Nearest => ScaleFilter::Smooth,
                    ScaleFilter::Smooth => ScaleFilter::Nearest,
                };
            }
            Tool::Debug => self.debug_open = !self.debug_open,
            Tool::Keymap => self.keymap_window.toggle(),
            Tool::About => self.about_open = !self.about_open,
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        self.toolbar_rect = ui.max_rect();
        let icons = *self.icons.get_or_insert_with(|| drawable_icons(ui.ctx()));
        let (filter, debug_open) = (self.filter, self.debug_open);
        let keymap_open = self.keymap_window.is_open();
        let out = toolbar_buttons(ui, &icons, |t| match t {
            Tool::Smooth => filter == ScaleFilter::Smooth,
            Tool::Debug => debug_open,
            Tool::Keymap => keymap_open,
            _ => false,
        });
        if let (Some(tool), Some(rect)) = (out.clicked, out.clicked_rect) {
            self.apply_tool(tool, rect);
        }
    }

    /// The slot picker, About and keyboard-mapping windows and toasts, over
    /// everything else.
    fn overlays(&mut self, ctx: &egui::Context) {
        if let Some((mode, pos)) = self.picker {
            let store = &self.store;
            let out = slot_picker(ctx, pos, mode, |n| store.has_slot(n));
            if let Some(n) = out.picked {
                match mode {
                    PickerMode::Save => self.save_slot(n),
                    PickerMode::Load => self.load_slot(n),
                }
            } else if out.clicked_elsewhere {
                // A click on the toolbar is handled by the tool itself.
                let on_toolbar = ctx
                    .input(|i| i.pointer.interact_pos())
                    .is_some_and(|p| self.toolbar_rect.contains(p));
                if !on_toolbar {
                    self.picker = None;
                }
            }
        }
        about_window(ctx, &mut self.about_open);
        self.keymap_window.show(ctx, &mut self.keymap);
        self.toasts.show(ctx);
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
        // `logic` runs before `ui`, so a press the mapping window captures in
        // this frame's `ui` is already dropped here.
        let typing = ctx.egui_wants_keyboard_input() || self.keymap_window.capturing();
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

    /// Step 4 of `logic`: move the UART output into the console.
    fn drain_uart(&mut self) {
        let bytes = self.backend.take_uart();
        self.debug.uart_mut().push(&bytes);
    }

    /// The debug panel's contents; applies its run controls.
    fn debug_panel(&mut self, ui: &mut egui::Ui) {
        let out = self.debug.show(ui, self.backend.as_mut(), self.paused);
        if out.step && self.paused {
            self.status = self.backend.step();
        }
        if out.toggle_pause {
            if self.paused && matches!(self.status, RunStatus::Breakpoint(_)) {
                // Move off the breakpoint, or the run would stop on it at once.
                self.status = self.backend.step();
            }
            self.paused = !self.paused;
            if !self.paused && !self.status.is_stop() {
                self.status = RunStatus::Running;
            }
            self.speed.reset();
        }
    }

    /// Grows the window by `DEBUG_W` when the debug panel opens and shrinks it
    /// back when it closes (only if it grew). A host that ignores viewport
    /// commands (kittest) keeps the size; the panel's width cap covers that.
    fn sync_window(&mut self, ctx: &egui::Context) {
        if self.debug_open == self.debug_shown {
            return;
        }
        self.debug_shown = self.debug_open;
        self.grow_target = None;
        let inner = ctx.input(|i| i.viewport().inner_rect);
        if self.debug_open {
            if let Some(r) = inner {
                let size = r.size() + vec2(DEBUG_W, 0.0);
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
                self.grown = true;
                self.grow_target = Some((size.x, GROW_WAIT_FRAMES));
            }
        } else if std::mem::take(&mut self.grown)
            && let Some(r) = inner
        {
            let size = (r.size() - vec2(DEBUG_W, 0.0)).max(Vec2::splat(1.0));
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
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
        egui::Panel::left("toolbar")
            .exact_size(TOOLBAR_W)
            .resizable(false)
            .frame(toolbar_frame())
            .show(ui, |ui| self.toolbar(ui));
        if self.debug_open {
            let window_w = ui.max_rect().width();
            if let Some((target, left)) = self.grow_target {
                ui.ctx()
                    .data_mut(|d| d.remove::<egui::PanelState>(Id::new(DEBUG_PANEL_ID)));
                self.grow_target =
                    (window_w + 0.5 < target && left > 0).then(|| (target, left - 1));
            }
            let max_w = debug_max_width(window_w);
            // `max_size` after `min_size` lowers the minimum to the cap when
            // the window is too narrow for both.
            egui::Panel::right(DEBUG_PANEL_ID)
                .resizable(true)
                .default_size(DEBUG_W)
                .min_size(DEBUG_MIN_W)
                .max_size(max_w)
                .show(ui, |ui| self.debug_panel(ui));
        }
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(APP_BG).inner_margin(CENTRAL_MARGIN))
            .show(ui, |ui| self.calculator(ui));
        let ctx = ui.ctx().clone();
        self.overlays(&ctx);
        self.sync_window(&ctx);
    }

    /// Called on shutdown and periodically: writes flash to disk, then the
    /// preferences.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.persist();
        eframe::set_value(storage, PREFS_KEY, &self.prefs());
    }
}
