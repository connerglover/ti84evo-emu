//! The application shell: header, calculator view and (later) toolbar and
//! debug panels.

use std::path::{Path, PathBuf};

use egui::{Align, Frame, Layout, Margin, RichText};

use crate::backend::{EmuBackend, RunStatus};
use crate::input::KeyLatch;
use crate::keypad::KeyId;
use crate::keypad::geometry::{CalcTransform, LCD_RECT};
use crate::keypad::paint::{body_rect_to_screen, paint_body, paint_key};
use crate::lcd::{LcdView, ScaleFilter};
use crate::pacer::SpeedMeter;
use crate::theme::{APP_BG, HEADER_BG, LABEL_DARK, install_fonts};

/// The window and header title.
pub const TITLE: &str = "TI-84 Evo Emulator";
/// Height of the header bar, in points.
const HEADER_H: f32 = 44.0;
/// Size of the header title, in points.
const TITLE_SIZE: f32 = 18.0;
/// Space around the calculator inside the central panel, in points.
const CENTRAL_MARGIN: i8 = 8;

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

/// The emulator application.
pub struct EvoApp {
    backend: Box<dyn EmuBackend>,
    data_dir: PathBuf,
    paused: bool,
    debug_open: bool,
    status: RunStatus,
    speed: SpeedMeter,
    latch: KeyLatch,
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
            latch: KeyLatch::new(),
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

    fn calculator(&mut self, ui: &mut egui::Ui) {
        let t = CalcTransform::fit(ui.available_rect_before_wrap());
        self.transform = t;
        let painter = ui.painter().clone();
        paint_body(&painter, &t);
        self.lcd.paint(ui, body_rect_to_screen(&t, LCD_RECT));
        for def in crate::keypad::KEYS.iter() {
            let press = if self.latch.is_down(def.id) { 1.0 } else { 0.0 };
            paint_key(&painter, &t, def, press, false);
        }
    }
}

impl eframe::App for EvoApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let backlight = self.backend.backlight();
        self.lcd
            .update(ctx, self.backend.framebuffer(), backlight, self.filter);
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
