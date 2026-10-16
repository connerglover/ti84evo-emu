//! The ROM Setup Wizard, Reload ROM and Change ROM.
//!
//! The wizard is a modal with four pages: Welcome, Choose (pick or drop an OS bundle, which
//! [`validate::check_bundle`] checks), Prepare (the new backend is provisioned on the app's
//! worker thread) and Done. It opens on the first frame when [`AppOptions::open_wizard`] is set
//! (no ROM linked yet) and at Choose for Calculator › Change ROM.
//!
//! This feature also owns Settings › Instances ([`instances`]) and Settings › Reset everything
//! ([`reset`]), which ends by opening the wizard at Welcome.

pub mod instances;
pub mod reset;
pub mod validate;

use std::path::{Path, PathBuf};

use egui::{Align, Button, Id, Layout, RichText};

use crate::app::{AppOptions, PROVISION_NOTICE};
use crate::backend::EmuBackend;
use crate::backend::placeholder::Placeholder;
use crate::feature::{AppRequest, Feature, FeatureCtx};
use crate::platform::FileFilter;
use crate::prefs::Prefs;
use crate::toast::ToastKind;
use crate::toolbar::Action;

use instances::InstancesWindow;
use reset::{ResetChoice, ResetModal, clear_data_dir};
use validate::{Checked, check_bundle, display_name};

/// The modal's title.
pub const WIZARD_TITLE: &str = "ROM Setup Wizard";
/// The Welcome page's text.
pub const WELCOME_TEXT: &str =
    "This emulator needs a TI-84 Evo OS bundle (.84b2). It is not included: use your own copy.";
/// The Choose page's heading and the open dialog's title.
pub const CHOOSE_TITLE: &str = "Choose an OS bundle";
/// The open dialog's filter on the Choose page.
pub const BUNDLE_FILTER: FileFilter = FileFilter {
    name: "TI-84 Evo OS bundle",
    extensions: &["84b2"],
};
/// Shown when more than one file is dropped on the Choose page.
pub const DROP_ONE: &str = "Drop one file at a time";
/// Shown on the Prepare page when provisioning ended and the unit still needs it.
pub const PREPARE_FAILED: &str = "Preparing failed.";

/// Width of the wizard's content, in points.
const WIZARD_W: f32 = 380.0;

/// A page of the ROM Setup Wizard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Welcome,
    Choose,
    Prepare,
    Done,
}

/// The ROM setup feature: the wizard, Change ROM, Reload ROM, Instances and Reset everything.
pub struct RomSetup {
    /// The open page; `None` while the wizard is closed.
    page: Option<Page>,
    /// The bundle that passed the check on the Choose page.
    chosen: Option<(PathBuf, Checked)>,
    /// The inline error of the Choose page (a failed check, a refused drop, a failed open).
    error: Option<String>,
    /// `ctx` time of the first frame the current provisioning run was shown on.
    prepare_started: Option<f64>,
    /// Settings › Instances.
    instances: InstancesWindow,
    /// Settings › Reset everything.
    reset: ResetModal,
}

impl RomSetup {
    pub fn new(opts: &AppOptions) -> Self {
        let mut s = Self {
            page: None,
            chosen: None,
            error: None,
            prepare_started: None,
            instances: InstancesWindow::default(),
            reset: ResetModal::default(),
        };
        if opts.open_wizard {
            s.open_at(Page::Welcome);
        }
        s
    }

    /// The open wizard page; `None` when the wizard is closed.
    pub fn page(&self) -> Option<Page> {
        self.page
    }

    /// Opens the wizard at `page` with nothing chosen.
    fn open_at(&mut self, page: Page) {
        self.close();
        self.page = Some(page);
    }

    fn close(&mut self) {
        self.page = None;
        self.chosen = None;
        self.error = None;
        self.prepare_started = None;
    }

    /// Checks `path` and keeps it, or the error, for the Choose page.
    fn choose(&mut self, path: PathBuf) {
        match check_bundle(&path) {
            Ok(c) => {
                self.chosen = Some((path, c));
                self.error = None;
            }
            Err(e) => {
                self.chosen = None;
                self.error = Some(e);
            }
        }
    }

    /// Next on the Choose page: opens the chosen bundle and asks the app to swap it in.
    fn open_chosen(&mut self, cx: &mut FeatureCtx<'_>) {
        let Some((path, _)) = &self.chosen else {
            return;
        };
        let path = path.clone();
        match open_bundle(&path, cx) {
            Ok(backend) => {
                cx.requests.push(AppRequest::ReplaceBackend {
                    backend,
                    rom: Some(path.clone()),
                    persist_old: true,
                });
                cx.requests.push(AppRequest::ApplyPrefs(Prefs {
                    bundle: Some(path),
                    ..cx.prefs.clone()
                }));
                self.page = Some(Page::Prepare);
                self.error = None;
                self.prepare_started = None;
            }
            Err(e) => {
                self.error = Some(format!("Cannot open {}: {e}", display_name(&path)));
            }
        }
    }

    /// Calculator › Reload ROM: opens the running bundle again, with its flash.
    fn reload(&mut self, cx: &mut FeatureCtx<'_>) {
        let Some(rom) = cx.rom.map(Path::to_path_buf) else {
            cx.toasts.push("No ROM to reload", ToastKind::Info);
            return;
        };
        match open_bundle(&rom, cx) {
            Ok(backend) => {
                cx.requests.push(AppRequest::ReplaceBackend {
                    backend,
                    rom: Some(rom),
                    persist_old: true,
                });
                cx.toasts.push("ROM reloaded", ToastKind::Info);
            }
            Err(e) => cx
                .toasts
                .push(format!("Reload failed: {e}"), ToastKind::Error),
        }
    }

    /// Reset everything, confirmed: deletes this instance's data and starts over without a ROM.
    /// The old machine is dropped unsaved (its flash was just deleted).
    fn reset_everything(&mut self, cx: &mut FeatureCtx<'_>) {
        for e in clear_data_dir(cx.data_dir) {
            cx.toasts.push(e, ToastKind::Error);
        }
        cx.requests.push(AppRequest::ReplaceBackend {
            backend: Box::new(Placeholder::no_rom()),
            rom: None,
            persist_old: false,
        });
        cx.requests.push(AppRequest::ApplyPrefs(Prefs::default()));
        cx.requests.push(AppRequest::ResetLayout);
        self.instances = InstancesWindow::default();
        self.open_at(Page::Welcome);
        cx.toasts.push("Everything was reset", ToastKind::Info);
    }

    /// Draws the content of `page`; `now` is the `ctx` time.
    fn page_ui(&mut self, ui: &mut egui::Ui, page: Page, now: f64, cx: &mut FeatureCtx<'_>) {
        ui.label(RichText::new(WIZARD_TITLE).strong());
        ui.separator();
        match page {
            Page::Welcome => {
                ui.heading("Welcome");
                ui.add_space(4.0);
                ui.label(WELCOME_TEXT);
            }
            Page::Choose => self.choose_ui(ui, cx),
            Page::Prepare => {
                ui.heading("Prepare calculator");
                ui.add_space(4.0);
                if cx.provisioning {
                    let started = *self.prepare_started.get_or_insert(now);
                    ui.label(PROVISION_NOTICE);
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        let secs = (now - started).max(0.0).floor() as u64;
                        ui.label(format!("{secs} s"));
                    });
                } else {
                    ui.colored_label(ui.visuals().error_fg_color, PREPARE_FAILED);
                }
            }
            Page::Done => {
                ui.heading("Done");
                ui.add_space(4.0);
                ui.label("The calculator is ready.");
            }
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                self.close();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| match page {
                Page::Welcome => {
                    if ui.button("Next").clicked() {
                        self.page = Some(Page::Choose);
                    }
                }
                Page::Choose => {
                    if ui
                        .add_enabled(self.chosen.is_some(), Button::new("Next"))
                        .clicked()
                    {
                        self.open_chosen(cx);
                    }
                    if ui.button("Back").clicked() {
                        self.page = Some(Page::Welcome);
                    }
                }
                Page::Prepare => {
                    if !cx.provisioning && ui.button("Retry").clicked() {
                        self.prepare_started = None;
                        cx.requests.push(AppRequest::Provision);
                    }
                }
                Page::Done => {
                    if ui.button("Finish").clicked() {
                        self.close();
                    }
                }
            });
        });
    }

    fn choose_ui(&mut self, ui: &mut egui::Ui, cx: &mut FeatureCtx<'_>) {
        ui.heading(CHOOSE_TITLE);
        ui.add_space(4.0);
        let dropped: Vec<PathBuf> = ui.ctx().input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        match <[PathBuf; 1]>::try_from(dropped) {
            Ok([one]) => self.choose(one),
            Err(more) if more.is_empty() => {}
            Err(_) => {
                self.chosen = None;
                self.error = Some(DROP_ONE.to_owned());
            }
        }
        ui.horizontal(|ui| {
            if ui.button("Browse\u{2026}").clicked()
                && let Some(path) = cx.platform.pick_open(CHOOSE_TITLE, &[BUNDLE_FILTER])
            {
                self.choose(path);
            }
            ui.label(RichText::new("or drop the file here").weak());
        });
        ui.add_space(4.0);
        if let Some((path, c)) = &self.chosen {
            ui.label(RichText::new(display_name(path)).strong());
            ui.label(format!(
                "OS {} \u{b7} {}",
                c.info.build_version, c.info.target_device
            ));
            if let Some(w) = &c.warning {
                ui.colored_label(ui.visuals().warn_fg_color, w);
            }
        }
        if let Some(e) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
    }
}

/// Opens `bundle` with this instance's flash. The running backend's flash is written first,
/// so the new backend boots from the latest flash and not from the last periodic save.
fn open_bundle(bundle: &Path, cx: &mut FeatureCtx<'_>) -> Result<Box<dyn EmuBackend>, String> {
    if !cx.backend.needs_provision()
        && let Err(e) = cx.backend.persist()
    {
        cx.toasts
            .push(format!("Flash not saved: {e}"), ToastKind::Error);
    }
    cx.platform.open_backend(bundle, &cx.data_dir.join("flash"))
}

impl Feature for RomSetup {
    fn on_action(&mut self, action: Action, cx: &mut FeatureCtx<'_>) -> bool {
        match action {
            Action::ChangeRom => {
                self.open_at(Page::Choose);
                true
            }
            Action::ReloadRom => {
                self.reload(cx);
                true
            }
            Action::Instances => {
                self.instances.open();
                true
            }
            Action::ResetEverything => {
                self.reset.open();
                true
            }
            _ => false,
        }
    }

    /// Moves Prepare on to Done once provisioning ended with a provisioned unit.
    fn frame(&mut self, cx: &mut FeatureCtx<'_>) {
        if self.page == Some(Page::Prepare) && !cx.provisioning && !cx.backend.needs_provision() {
            self.page = Some(Page::Done);
        }
    }

    fn ui(&mut self, ctx: &egui::Context, cx: &mut FeatureCtx<'_>) {
        self.instances.ui(ctx, cx);
        if self.reset.ui(ctx, cx) == ResetChoice::Confirm {
            self.reset_everything(cx);
        }
        let Some(page) = self.page else {
            return;
        };
        let now = ctx.input(|i| i.time);
        egui::Modal::new(Id::new("rom-setup-wizard")).show(ctx, |ui| {
            ui.set_width(WIZARD_W);
            self.page_ui(ui, page, now, cx);
        });
    }

    fn active(&self, action: Action) -> bool {
        match action {
            Action::ChangeRom => self.page.is_some(),
            Action::Instances => self.instances.is_open(),
            Action::ResetEverything => self.reset.is_open(),
            _ => false,
        }
    }

    fn next_deadline(&self) -> Option<u64> {
        None
    }

    fn blocks_input(&self) -> bool {
        self.page.is_some() || self.reset.is_open()
    }

    fn on_exit(&mut self, _cx: &mut FeatureCtx<'_>) {}
}
