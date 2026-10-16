//! The application shell: header, sidebar and its popovers, calculator view, run loop and
//! keypad input, and debug panels.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui::{
    Align, Frame, Id, Label, Layout, Margin, Pos2, Rect, Response, RichText, Sense, Vec2,
    ViewportCommand, WidgetInfo, WidgetType, pos2, vec2,
};

use crate::backend::placeholder::Placeholder;
use crate::backend::{BackendError, CLOCK_HZ, EmuBackend, KeyEvent, RunStatus, UartId};
use crate::debug::{DEBUG_MIN_W, DEBUG_W, DebugPanel};
use crate::feature::{AppRequest, Feature, FeatureCtx};
use crate::features::Features;
use crate::input::{KeyAction, KeyLatch, KeyboardTracker, Keymap, Source, pc_key_label};
use crate::keymap_window::KeymapWindow;
use crate::keypad::geometry::{
    BODY_W, CalcTransform, KEYPAD_ORIGIN, LCD_RECT, bounds, dpad_bounds, hit_test,
};
use crate::keypad::paint::{body_rect_to_screen, keypad_rect_to_screen, paint_body, paint_key};
use crate::keypad::{KEYS, KeyId, KeyShape, key_def};
use crate::lcd::{LcdView, ScaleFilter};
use crate::pacer::{FrameRunner, Pacer, Speed, SpeedMeter, WALL_BUDGET, capped_budget};
use crate::platform::{DesktopPlatform, PlatformRef};
use crate::prefs::{PREFS_KEY, Prefs};
use crate::store::{Store, StoreError};
use crate::theme::{APP_BG, HEADER_BG, LABEL_DARK, install_fonts};
use crate::toast::{ToastKind, Toasts};
use crate::toolbar::{
    Action, Group, PickerMode, TOOLBAR_W, about_window, drawable_icons, group_popover,
    not_available, sidebar_buttons, slot_picker, toolbar_frame,
};

/// The window and header title.
pub const TITLE: &str = "TI-84 Evo Emulator";
/// Height of the header bar, in points.
const HEADER_H: f32 = 44.0;
/// Size of the header title, in points.
const TITLE_SIZE: f32 = 18.0;
/// Header room kept for the status text before the backend name gets any, in points.
const STATUS_ROOM: f32 = 110.0;
/// Widest slot for the backend name in the header, in points; a longer name is elided.
const NAME_MAX_W: f32 = 240.0;
/// Below this slot width the header leaves the backend name out (its tooltip on the status
/// still shows it), in points.
const NAME_MIN_W: f32 = 80.0;
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
/// The window size Reset layout asks for (the default window).
const DEFAULT_WINDOW: Vec2 = vec2(520.0, 960.0);
/// The toast after Reset layout.
pub const LAYOUT_RESET: &str = "Layout reset";
/// The compact-mode context menu entry that brings the header and sidebar back.
pub const EXIT_COMPACT: &str = "Exit compact mode";

/// The widest the debug panel may be in a window `window_w` points wide:
/// what is left after the toolbar, the central margins and a calculator at
/// [`MIN_CALC_SCALE_WITH_DEBUG`]. Never negative.
pub fn debug_max_width(window_w: f32) -> f32 {
    let reserved = TOOLBAR_W + 2.0 * f32::from(CENTRAL_MARGIN) + BODY_W * MIN_CALC_SCALE_WITH_DEBUG;
    (window_w - reserved).max(0.0)
}

/// The progress message (and header status) while the machine is provisioned.
pub const PROVISION_NOTICE: &str = "Preparing calculator\u{2026}";
/// The toast after provisioning succeeded.
pub const PROVISION_DONE: &str = "Calculator ready";
/// The toast for a machine tool used while provisioning runs.
pub const PROVISION_BUSY: &str = "Wait until the calculator is ready";
/// How often the app checks on a running provisioning.
const PROVISION_POLL: Duration = Duration::from_millis(50);

/// What the provisioning thread hands back: the backend, and how provisioning went.
type ProvisionResult = (Box<dyn EmuBackend>, Result<(), BackendError>);

/// Which backend `main` starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// The fake backend (`--demo`).
    Demo,
    /// The emulated machine, booted from this bundle (argument or `EVO_OS_IMAGE`).
    Bundle(PathBuf),
    /// The bundle linked in the preferences, else no ROM and the ROM Setup Wizard
    /// (see [`start_backend`]).
    FromPrefs,
}

/// Picks the backend: `--demo`, else the bundle argument, else `EVO_OS_IMAGE` (`env`; empty
/// counts as unset), else [`Choice::FromPrefs`].
pub fn choose_backend(bundle_arg: Option<PathBuf>, demo: bool, env: Option<OsString>) -> Choice {
    if demo {
        return Choice::Demo;
    }
    match bundle_arg.or_else(|| env.filter(|e| !e.is_empty()).map(PathBuf::from)) {
        Some(path) => Choice::Bundle(path),
        None => Choice::FromPrefs,
    }
}

/// What [`start_backend`] decided.
pub struct StartUp {
    pub backend: Box<dyn EmuBackend>,
    /// Shown as an Info toast on the first frame.
    pub notice: Option<String>,
    /// Open the ROM Setup Wizard.
    pub open_wizard: bool,
}

/// The [`Choice::FromPrefs`] decision: the bundle linked in `prefs`, opened with its flash in
/// `dir/flash`. Without a linked bundle, or when it does not open (with a notice saying why),
/// the backend is [`Placeholder::no_rom`] and the ROM Setup Wizard opens.
pub fn start_backend(prefs: &Prefs, platform: &PlatformRef, dir: &Path) -> StartUp {
    let Some(path) = &prefs.bundle else {
        return StartUp {
            backend: Box::new(Placeholder::no_rom()),
            notice: None,
            open_wizard: true,
        };
    };
    match platform.open_backend(path, &dir.join("flash")) {
        Ok(backend) => StartUp {
            backend,
            notice: None,
            open_wizard: false,
        },
        Err(e) => StartUp {
            backend: Box::new(Placeholder::no_rom()),
            notice: Some(format!("Cannot open {}: {e}", path.display())),
            open_wizard: true,
        },
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
    /// File dialogs, clipboard, browser, network and process spawning.
    pub platform: PlatformRef,
    /// Open the ROM Setup Wizard on the first frame.
    pub open_wizard: bool,
    /// The OS bundle the backend was opened from; `None` without a ROM or for the demo.
    pub rom: Option<PathBuf>,
    /// The instance id; `None` for the default instance.
    pub instance: Option<String>,
}

impl AppOptions {
    /// Defaults: not paused, default preferences, no notice, the desktop platform, no wizard,
    /// no ROM, the default instance.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            start_paused: false,
            data_dir,
            prefs: Prefs::default(),
            notice: None,
            platform: DesktopPlatform::shared(),
            open_wizard: false,
            rom: None,
            instance: None,
        }
    }
}

/// The header status line.
///
/// A stop status (breakpoint, lockup, `BKPT`, watchpoint, goal) is shown even while
/// paused, since it is the reason for the pause; any other status shows `Paused`.
pub fn status_text(status: RunStatus, paused: bool, speed: Option<u32>) -> String {
    match status {
        RunStatus::Breakpoint(a) => format!("Breakpoint at 0x{a:08x}"),
        RunStatus::Lockup { pc } => format!("Lockup at 0x{pc:08x}"),
        RunStatus::Bkpt { pc, imm } => format!("BKPT #{imm} at 0x{pc:08x}"),
        RunStatus::Watchpoint { addr, dir, pc, .. } => {
            format!(
                "Watchpoint ({}) at 0x{addr:08x}, pc 0x{pc:08x}",
                dir.as_str()
            )
        }
        RunStatus::Goal { pc } => format!("Stopped at 0x{pc:08x}"),
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
    /// The thread provisioning the real backend; `backend` is a [`Placeholder`] meanwhile.
    provisioning: Option<JoinHandle<ProvisionResult>>,
    usb_worker: Option<crate::connect::worker::UsbWorker>,
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
    /// The linked OS bundle, kept so saving preferences does not drop it.
    bundle: Option<PathBuf>,
    /// Compact sidebar mode, kept so saving preferences does not drop it.
    compact: bool,
    /// The selected speed; `pacer.speed` is its factor.
    speed_setting: Speed,
    transform: CalcTransform,
    store: Store,
    toasts: Toasts,
    /// Per group, whether its icon is drawable; filled on the first frame,
    /// once the fonts are loaded.
    icons: Option<[bool; 7]>,
    /// The open group popover and where it sits.
    popover: Option<(Group, Pos2)>,
    /// Where the last popover sat: the slot picker opens there.
    popover_pos: Pos2,
    /// The open slot picker and where it sits.
    picker: Option<(PickerMode, Pos2)>,
    /// Reset layout was asked for; it runs at the start of the next `ui` pass, which has
    /// the context it needs.
    pending_reset_layout: bool,
    about_open: bool,
    keymap_window: KeymapWindow,
    /// The sidebar panel as laid out in the last frame.
    toolbar_rect: Rect,
    /// The platform services; features reach them through the app.
    platform: PlatformRef,
    /// The tools, driven through the [`Feature`] seam.
    features: Features,
    /// The OS bundle the backend was opened from.
    rom: Option<PathBuf>,
    /// The instance id; `None` for the default instance.
    instance: Option<String>,
    /// The egui context, for requests that need it (`Close`).
    ctx: egui::Context,
    /// Set once a `Close` request was applied.
    close_requested: bool,
}

impl EvoApp {
    /// Builds the app and installs its fonts and light theme into `cc_ctx`. A backend that
    /// [`needs_provision`](EmuBackend::needs_provision) is provisioned at once, on a worker
    /// thread.
    pub fn new(cc_ctx: &egui::Context, backend: Box<dyn EmuBackend>, opts: AppOptions) -> Self {
        install_fonts(cc_ctx);
        cc_ctx.set_theme(egui::ThemePreference::Light);
        let features = Features::new(&opts);
        let mut toasts = Toasts::new();
        if let Some(notice) = opts.notice {
            toasts.push(notice, ToastKind::Info);
        }
        let needs_provision = backend.needs_provision();
        let mut app = Self {
            backend,
            provisioning: None,
            usb_worker: None,
            store: Store::new(opts.data_dir.clone()),
            toasts,
            icons: None,
            popover: None,
            popover_pos: pos2(TOOLBAR_W, HEADER_H),
            picker: None,
            pending_reset_layout: false,
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
                speed: opts.prefs.speed.factor(),
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
            bundle: opts.prefs.bundle,
            compact: opts.prefs.compact,
            speed_setting: opts.prefs.speed,
            transform: CalcTransform::fit(egui::Rect::ZERO),
            platform: opts.platform,
            features,
            rom: opts.rom,
            instance: opts.instance,
            ctx: cc_ctx.clone(),
            close_requested: false,
        };
        if needs_provision {
            app.start_provision();
        }
        app
    }

    /// True while the machine is being provisioned (first run or Re-provision).
    pub fn provisioning(&self) -> bool {
        self.provisioning.is_some()
    }

    /// Moves the backend to a worker thread that provisions it; a [`Placeholder`] stands in.
    fn start_provision(&mut self) {
        if self.provisioning.is_some() || self.usb_worker.is_some() {
            return;
        }
        self.resync();
        let away = Box::new(Placeholder::new(
            self.backend.name(),
            &self.backend.state_tag(),
        ));
        let mut backend = std::mem::replace(&mut self.backend, away);
        let spawned = std::thread::Builder::new()
            .name("evo-provision".to_owned())
            .spawn(move || {
                // A panic must not take the backend with it.
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| backend.provision()))
                    .unwrap_or_else(|_| {
                        Err(BackendError::Provision(
                            "the provisioning thread panicked".into(),
                        ))
                    });
                (backend, result)
            });
        match spawned {
            Ok(handle) => {
                self.provisioning = Some(handle);
                self.toasts.set_progress(Some(PROVISION_NOTICE.to_owned()));
            }
            // The backend is lost with the closure only if the OS refuses a thread.
            Err(e) => self
                .toasts
                .push(format!("Cannot start provisioning: {e}"), ToastKind::Error),
        }
    }

    /// Takes the backend back once the provisioning thread is done: saves the provisioned
    /// flash and lets the machine run, or reports the failure.
    fn poll_provision(&mut self, ctx: &egui::Context) {
        let Some(handle) = &self.provisioning else {
            return;
        };
        if !handle.is_finished() {
            ctx.request_repaint_after(PROVISION_POLL);
            return;
        }
        let Some(handle) = self.provisioning.take() else {
            return;
        };
        self.toasts.set_progress(None);
        match handle.join() {
            Ok((backend, result)) => {
                self.backend = backend;
                self.resync();
                match result {
                    Ok(()) => {
                        self.persist();
                        self.toasts.push(PROVISION_DONE, ToastKind::Info);
                    }
                    Err(e) => self.toasts.push(e.to_string(), ToastKind::Error),
                }
            }
            // `catch_unwind` in the thread makes this unreachable in practice.
            Err(_) => self
                .toasts
                .push("The provisioning thread was lost", ToastKind::Error),
        }
    }

    pub fn backend(&self) -> &dyn EmuBackend {
        self.backend.as_ref()
    }

    /// The backend, for tests that read its frame. While provisioning runs this is a
    /// stand-in with no display.
    pub fn backend_mut(&mut self) -> &mut dyn EmuBackend {
        self.backend.as_mut()
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    /// Pauses or resumes. Pausing cancels any debugger goal (also when already paused).
    /// Resuming from a breakpoint first steps off it, or the run would stop on it at once.
    pub fn set_paused(&mut self, p: bool) {
        if p {
            self.backend.cancel_goal();
            if !self.paused {
                self.paused = true;
                self.speed.reset();
            }
            return;
        }
        if !self.paused {
            return;
        }
        if matches!(self.status, RunStatus::Breakpoint(_)) {
            self.status = self.backend.step();
            if self.status.is_stop() {
                self.speed.reset();
                return;
            }
        }
        self.paused = false;
        if !self.status.is_stop() {
            self.status = RunStatus::Running;
        }
        self.speed.reset();
    }

    /// The OS bundle the backend was opened from; `None` without a ROM or for the demo.
    pub fn rom(&self) -> Option<&Path> {
        self.rom.as_deref()
    }

    pub fn features(&self) -> &Features {
        &self.features
    }

    pub fn features_mut(&mut self) -> &mut Features {
        &mut self.features
    }

    /// Adds a feature after the built-in ones (tests only).
    #[doc(hidden)]
    pub fn push_feature(&mut self, f: Box<dyn Feature>) {
        self.features.push(f);
    }

    /// Adds a feature ahead of the built-in ones, so it is offered actions they own
    /// (tests only).
    #[doc(hidden)]
    pub fn push_feature_first(&mut self, f: Box<dyn Feature>) {
        self.features.push_first(f);
    }

    /// The debug panels' state: consoles, log and tabs.
    pub fn debug(&self) -> &DebugPanel {
        &self.debug
    }

    /// True once a `Close` request was applied.
    pub fn close_requested(&self) -> bool {
        self.close_requested
    }

    /// Applies one feature request (see [`crate::feature`] for when the app does this).
    pub fn apply_request(&mut self, r: AppRequest) {
        if self.usb_worker.is_some()
            && matches!(
                &r,
                AppRequest::Reset | AppRequest::Provision | AppRequest::ReplaceBackend { .. }
            )
        {
            self.toasts.push(
                "Wait for the USB operation to finish, or cancel it in USB tools.",
                ToastKind::Info,
            );
            return;
        }
        match r {
            AppRequest::Usb(task) => {
                if self.provisioning()
                    || self.usb_worker.is_some()
                    || self.features.rom.blocks_input()
                    || self.picker.is_some()
                {
                    let _ = task
                        .events
                        .send(crate::connect::types::UsbEvent::Finished(Err(
                            "The calculator is busy. Try again when it is ready.".into(),
                        )));
                } else {
                    self.resync();
                    self.backend.cancel_goal();
                    match crate::connect::worker::UsbWorker::start(&mut self.backend, task) {
                        Ok(worker) => self.usb_worker = Some(worker),
                        Err(e) => self.toasts.push(e, ToastKind::Error),
                    }
                }
            }
            AppRequest::Resync => self.resync(),
            AppRequest::Persist => self.persist(),
            AppRequest::Reset => self.reset_machine(),
            AppRequest::Provision => self.start_provision(),
            AppRequest::ReplaceBackend {
                backend,
                rom,
                persist_old,
            } => self.replace_backend(backend, rom, persist_old),
            AppRequest::ApplyPrefs(p) => self.apply_prefs(p),
            AppRequest::SetPaused(p) => self.set_paused(p),
            AppRequest::ResetLayout => self.pending_reset_layout = true,
            AppRequest::Close => {
                self.close_requested = true;
                self.ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    fn apply_requests(&mut self, requests: Vec<AppRequest>) {
        for r in requests {
            self.apply_request(r);
        }
    }

    /// Swaps in `backend`. A provisioning run in progress is abandoned: its thread is detached
    /// and the backend it holds is dropped when it ends, unsaved.
    fn replace_backend(
        &mut self,
        backend: Box<dyn EmuBackend>,
        rom: Option<PathBuf>,
        persist_old: bool,
    ) {
        if self.provisioning.take().is_some() {
            self.toasts.set_progress(None);
        } else if persist_old {
            self.persist();
        }
        self.backend = backend;
        self.rom = rom;
        self.features.connect.calculator_changed();
        self.resync();
        if self.backend.needs_provision() {
            self.start_provision();
        }
    }

    fn apply_prefs(&mut self, p: Prefs) {
        self.keymap = p.keymap;
        self.filter = p.filter;
        self.debug_open = p.debug_open;
        self.compact = p.compact;
        self.bundle = p.bundle;
        self.set_speed(p.speed);
    }

    /// Runs `f` on each feature in turn, with one [`FeatureCtx`], until `f` returns `true`;
    /// then applies the requests the pass collected. Returns whether `f` returned `true`.
    fn feature_pass(
        &mut self,
        key_events: &[KeyEvent],
        mut f: impl FnMut(&mut dyn Feature, &mut FeatureCtx<'_>) -> bool,
    ) -> bool {
        let prefs = self.prefs();
        let provisioning = self.provisioning() || self.usb_worker.is_some();
        let mut requests = Vec::new();
        let mut handled = false;
        {
            let mut cx = FeatureCtx {
                backend: self.backend.as_mut(),
                toasts: &mut self.toasts,
                platform: &self.platform,
                data_dir: &self.data_dir,
                prefs: &prefs,
                rom: self.rom.as_deref(),
                instance: self.instance.as_deref(),
                paused: self.paused,
                provisioning,
                key_events,
                requests: &mut requests,
            };
            for feature in self.features.each() {
                if f(feature, &mut cx) {
                    handled = true;
                    break;
                }
            }
        }
        self.apply_requests(requests);
        handled
    }

    pub fn debug_open(&self) -> bool {
        self.debug_open
    }

    /// True in compact mode: no header and no sidebar.
    pub fn compact(&self) -> bool {
        self.compact
    }

    /// The selected emulation speed.
    pub fn speed(&self) -> Speed {
        self.speed_setting
    }

    /// The platform services the app was built with.
    pub fn platform(&self) -> &PlatformRef {
        &self.platform
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

    /// The preferences to save: current keymap, filter, debug-open state,
    /// linked bundle, compact mode and speed.
    pub fn prefs(&self) -> Prefs {
        Prefs {
            keymap: self.keymap.clone(),
            filter: self.filter,
            debug_open: self.debug_open,
            bundle: self.bundle.clone(),
            compact: self.compact,
            speed: self.speed_setting,
        }
    }

    /// True while the keyboard-mapping window waits for a key to bind.
    pub fn keymap_capturing(&self) -> bool {
        self.keymap_window.capturing()
    }

    /// Writes flash to disk; a failure becomes a toast. Skipped while the unit still needs
    /// provisioning: after a failed first run the flash is wherever provisioning stopped, and
    /// a flash file would stop the next launch from provisioning again.
    fn persist(&mut self) {
        if self.backend.needs_provision() {
            return;
        }
        if let Err(e) = self.backend.persist() {
            self.toasts
                .push(format!("Flash not saved: {e}"), ToastKind::Error);
        }
    }

    /// Brings the GUI back in line with a machine that was reset or loaded:
    /// drops keys restored as held and releases pending on the old timeline, and cancels any
    /// debugger goal.
    fn resync(&mut self) {
        self.backend.cancel_goal();
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
        self.features.connect.calculator_changed();
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

    /// Pauses, or resumes (see [`set_paused`](Self::set_paused)).
    fn toggle_pause(&mut self) {
        self.set_paused(!self.paused);
    }

    fn set_speed(&mut self, s: Speed) {
        self.speed_setting = s;
        self.pacer.speed = s.factor();
        self.speed.reset();
    }

    /// Closes the debug dock and leaves compact mode, forgets window and panel positions
    /// and sizes, and asks for the default window size.
    fn reset_layout(&mut self, ctx: &egui::Context) {
        self.debug_open = false;
        self.debug_shown = false;
        self.compact = false;
        self.grown = false;
        self.grow_target = None;
        self.popover = None;
        self.picker = None;
        ctx.data_mut(|d| d.remove::<egui::PanelState>(Id::new(DEBUG_PANEL_ID)));
        ctx.memory_mut(|m| m.reset_areas());
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(DEFAULT_WINDOW));
        self.toasts.push(LAYOUT_RESET, ToastKind::Info);
    }

    /// Whether `a` is drawn selected in its popover.
    /// The app answers for its own actions, then the features.
    fn action_active(&self, a: Action) -> bool {
        let app = match a {
            Action::TogglePause => self.paused,
            Action::Speed(s) => s == self.speed_setting,
            Action::ToggleDebug => self.debug_open,
            Action::Keymap => self.keymap_window.is_open(),
            Action::Smooth => self.filter == ScaleFilter::Smooth,
            Action::Compact => self.compact,
            Action::About => self.about_open,
            _ => false,
        };
        app || self.features.active(a)
    }

    /// Runs one sidebar action. The slot picker of Save state and Load state opens where
    /// the last popover sat. An action the app does not handle goes to each feature in
    /// turn; one no feature takes shows [`not_available`].
    pub fn run_action(&mut self, a: Action) {
        let machine = matches!(
            a,
            Action::Reset
                | Action::Reprovision
                | Action::SaveState
                | Action::LoadState
                | Action::Screenshot
                | Action::ReloadRom
                | Action::ChangeRom
                | Action::ImportRam
                | Action::ExportRam
                | Action::ImportState
                | Action::ExportState
                | Action::ImportDebug
                | Action::ExportDebug
                | Action::ImportSettings
                | Action::ExportSettings
                | Action::ImportFlash
                | Action::ExportFlash
                | Action::ResetEverything
        );
        if machine && self.provisioning() {
            self.toasts.push(PROVISION_BUSY, ToastKind::Info);
            return;
        }
        if machine && self.usb_worker.is_some() {
            self.toasts.push(
                "Wait for the USB operation to finish, or cancel it in USB tools.",
                ToastKind::Info,
            );
            return;
        }
        match a {
            Action::Reset => self.reset_machine(),
            Action::Reprovision => {
                self.picker = None;
                self.start_provision();
            }
            Action::SaveState => self.picker = Some((PickerMode::Save, self.popover_pos)),
            Action::LoadState => self.picker = Some((PickerMode::Load, self.popover_pos)),
            Action::TogglePause => self.toggle_pause(),
            Action::Speed(s) => self.set_speed(s),
            Action::Screenshot => self.screenshot(),
            Action::ToggleDebug => self.debug_open = !self.debug_open,
            Action::Keymap => self.keymap_window.toggle(),
            Action::Smooth => {
                self.filter = match self.filter {
                    ScaleFilter::Nearest => ScaleFilter::Smooth,
                    ScaleFilter::Smooth => ScaleFilter::Nearest,
                };
            }
            Action::Compact => {
                self.compact = !self.compact;
                self.popover = None;
                self.picker = None;
            }
            Action::ResetLayout => self.pending_reset_layout = true,
            Action::About => self.about_open = !self.about_open,
            other => {
                if !self.feature_pass(&[], |f, cx| f.on_action(other, cx)) {
                    self.toasts.push(not_available(other), ToastKind::Info);
                }
            }
        }
    }

    /// Opens `group`'s popover beside `button`, or closes it when it is already open.
    /// Debug has no popover: it toggles the dock.
    fn click_group(&mut self, group: Group, button: Rect) {
        self.picker = None;
        if group == Group::Debug {
            self.popover = None;
            self.run_action(Action::ToggleDebug);
            return;
        }
        self.popover = match self.popover {
            Some((open, _)) if open == group => None,
            _ => Some((group, pos2(self.toolbar_rect.right(), button.top()))),
        };
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        self.toolbar_rect = ui.max_rect();
        let icons = *self.icons.get_or_insert_with(|| drawable_icons(ui.ctx()));
        let debug_open = self.debug_open;
        let open = self.popover.map(|(g, _)| g);
        let out = sidebar_buttons(ui, &icons, open, |g| g == Group::Debug && debug_open);
        if let (Some(group), Some(rect)) = (out.clicked, out.clicked_rect) {
            self.click_group(group, rect);
        }
    }

    /// The open group popover: runs the action picked and closes, or closes on a click
    /// outside it and the sidebar.
    fn popover(&mut self, ctx: &egui::Context) {
        let Some((group, pos)) = self.popover else {
            return;
        };
        let out = group_popover(ctx, pos, group, |a| self.action_active(a));
        if let Some(a) = out.picked {
            self.popover = None;
            self.popover_pos = pos;
            self.run_action(a);
        } else if out.clicked_elsewhere && !self.on_sidebar(ctx) {
            // A click on the sidebar is handled by its button.
            self.popover = None;
        }
    }

    /// True when this frame's click is on the sidebar.
    fn on_sidebar(&self, ctx: &egui::Context) -> bool {
        !self.compact
            && ctx
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|p| self.toolbar_rect.contains(p))
    }

    /// The slot picker, About and keyboard-mapping windows and toasts, over
    /// everything else.
    fn overlays(&mut self, ctx: &egui::Context) {
        let had_picker = self.picker.is_some();
        self.popover(ctx);
        if let Some((mode, pos)) = self.picker {
            let store = &self.store;
            let out = slot_picker(ctx, pos, mode, |n| store.has_slot(n));
            if let Some(n) = out.picked {
                match mode {
                    PickerMode::Save => self.save_slot(n),
                    PickerMode::Load => self.load_slot(n),
                }
            } else if out.clicked_elsewhere && had_picker && !self.on_sidebar(ctx) {
                // A click on the sidebar is handled by its button, and the click that
                // picked Save state or Load state in a popover this frame opened it.
                self.picker = None;
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
        // A feature's modal takes the keyboard too.
        let typing = ctx.egui_wants_keyboard_input()
            || self.keymap_window.capturing()
            || self.features.blocks_input();
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

    /// Step 2 of `logic`: emulate this frame's share of time. The run stops at the earliest
    /// feature deadline and at the latch's next key release, so scripted key events and
    /// clicked-key releases land at their exact cycles at any speed.
    fn emulate(&mut self, ctx: &egui::Context) {
        if self.paused || self.provisioning() || self.usb_worker.is_some() {
            self.speed.reset();
            return;
        }
        let (stable_dt, unstable_dt) = ctx.input(|i| (i.stable_dt, i.unstable_dt));
        let deadline = [self.features.next_deadline(), self.latch.next_release()]
            .into_iter()
            .flatten()
            .min();
        let budget = capped_budget(
            self.pacer.budget(f64::from(stable_dt)),
            self.backend.cycles(),
            deadline,
        );
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

    /// Step 4 of `logic`: move the UART output and new log lines into the consoles.
    fn drain_uart(&mut self) {
        let bytes = self.backend.take_uart();
        self.debug.push_uart(UartId::Uart0, &bytes);
        let bytes = self.backend.take_uart2();
        self.debug.push_uart(UartId::Uart2, &bytes);
        let lines = crate::logbuf::drain();
        if !lines.is_empty() {
            self.debug.push_log(lines);
        }
    }

    /// The debug panel's contents; applies its run controls.
    fn debug_panel(&mut self, ui: &mut egui::Ui) {
        let out = self.debug.show(ui, self.backend.as_mut(), self.paused);
        if out.step && self.paused {
            self.status = self.backend.step();
        }
        if let Some(goal) = out.goal
            && self.paused
        {
            match self.backend.begin_goal(goal) {
                Ok(None) => {
                    self.paused = false;
                    self.status = RunStatus::Running;
                    self.speed.reset();
                }
                Ok(Some(status)) => self.status = status,
                Err(e) => self.toasts.push(e.to_string(), ToastKind::Error),
            }
        }
        for note in out.notes {
            self.toasts.push(note, ToastKind::Error);
        }
        if out.toggle_pause {
            self.toggle_pause();
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

    /// The title on the left; the status and the backend name on the right. Nothing
    /// overlaps at any window width: the name is elided to a slot of at most
    /// [`NAME_MAX_W`] and left out when the slot would be narrower than [`NAME_MIN_W`], and
    /// the status and title are elided to the room that is left.
    fn header(&self, ui: &mut egui::Ui) {
        let status = if self.usb_worker.is_some() {
            "USB transfer…".to_owned()
        } else if self.provisioning() {
            PROVISION_NOTICE.to_owned()
        } else {
            status_text(self.status, self.paused, self.speed.percent())
        };
        let name = self.backend.name();
        ui.horizontal_centered(|ui| {
            ui.add(Label::new(RichText::new(TITLE).size(TITLE_SIZE).color(LABEL_DARK)).truncate());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let slot = (ui.available_width() - STATUS_ROOM).min(NAME_MAX_W);
                if slot >= NAME_MIN_W {
                    let size = vec2(slot, ui.available_height());
                    ui.allocate_ui_with_layout(size, Layout::right_to_left(Align::Center), |ui| {
                        ui.add(Label::new(RichText::new(name).color(LABEL_DARK)).truncate())
                            .on_hover_text(name);
                    });
                    ui.separator();
                }
                ui.add(Label::new(RichText::new(status).color(LABEL_DARK)).truncate())
                    .on_hover_text(name);
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

    /// In compact mode, the right-click menu on the calculator view's background. It is
    /// added before the keys, so the keys stay on top of it.
    fn compact_menu(&mut self, ui: &mut egui::Ui) {
        let bg = ui.interact(ui.max_rect(), Id::new("evo-compact-bg"), Sense::click());
        bg.context_menu(|ui| {
            let exit = ui.add(egui::Button::new(EXIT_COMPACT).sense(Sense::CLICK));
            if exit.clicked() {
                self.compact = false;
                ui.close();
            }
        });
    }

    fn calculator(&mut self, ui: &mut egui::Ui) {
        if self.compact {
            self.compact_menu(ui);
        }
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
        if self.usb_worker.as_ref().is_some_and(|w| w.is_finished()) {
            match self.usb_worker.take().unwrap().finish() {
                Ok(backend) => {
                    self.backend = backend;
                    self.resync();
                }
                Err(e) => self.toasts.push(e, ToastKind::Error),
            }
        }
        if self.usb_worker.is_some() {
            ctx.request_repaint_after(PROVISION_POLL);
        }
        self.poll_provision(ctx);
        self.keyboard(ctx);
        self.emulate(ctx);
        self.latch.tick(self.backend.cycles());
        self.flush_edges();
        let events = self.backend.take_key_events();
        self.drain_uart();
        self.feature_pass(&events, |f, cx| {
            f.frame(cx);
            false
        });
        if self.usb_worker.is_none() {
            let backlight = self.backend.backlight();
            self.lcd
                .update(ctx, self.backend.framebuffer(), backlight, self.filter);
        }
        if !self.paused {
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if std::mem::take(&mut self.pending_reset_layout) {
            let ctx = ui.ctx().clone();
            self.reset_layout(&ctx);
        }
        if !self.compact {
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
                .show(ui, |ui| self.sidebar(ui));
        }
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
                .show(ui, |ui| {
                    ui.add_enabled_ui(self.usb_worker.is_none(), |ui| self.debug_panel(ui));
                });
        }
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(APP_BG).inner_margin(CENTRAL_MARGIN))
            .show(ui, |ui| self.calculator(ui));
        let ctx = ui.ctx().clone();
        self.overlays(&ctx);
        self.feature_pass(&[], |f, cx| {
            f.ui(&ctx, cx);
            false
        });
        self.sync_window(&ctx);
    }

    /// Called on shutdown and periodically: writes flash to disk, then the
    /// preferences.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.persist();
        eframe::set_value(storage, PREFS_KEY, &self.prefs());
    }

    /// Called once on shutdown, after [`save`](Self::save): the features finish (a recording
    /// is written out), their requests apply, then flash is written a last time.
    fn on_exit(&mut self) {
        if let Some(worker) = self.usb_worker.take() {
            worker.cancel();
            match worker.finish() {
                Ok(backend) => self.backend = backend,
                Err(e) => log::error!("{e}"),
            }
        }
        self.feature_pass(&[], |f, cx| {
            f.on_exit(cx);
            false
        });
        self.persist();
    }
}
