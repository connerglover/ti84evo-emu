//! Help: third-party licenses, the update check and bug reports.

mod update;

use std::time::Duration;

use egui::{Align2, RichText, ScrollArea, TextStyle, Vec2};

pub use update::{
    ISSUES_URL, Release, UPDATE_REPO, UpdateCheck, UpdateOutcome, latest_release_url, outcome,
    parse_release, parse_version,
};

use crate::app::AppOptions;
use crate::feature::{Feature, FeatureCtx};
use crate::toast::ToastKind;
use crate::toolbar::Action;

/// How often the UI wakes up to poll a running update check.
const UPDATE_POLL: Duration = Duration::from_millis(100);

/// The text of the "Third-party licenses" window: the generated crate licenses, then the
/// license of the bundled DejaVu fonts.
pub fn licenses_text() -> String {
    let mut text = String::from(include_str!("../../assets/third-party-licenses.txt"));
    text.push_str("\n\nDejaVu fonts\n");
    text.push_str(include_str!("../../assets/LICENSE"));
    text
}

/// The help feature: owns Help › Third-party licenses, Check for updates and Report a bug.
pub struct Help {
    /// The running update check, if any.
    check: Option<UpdateCheck>,
    /// The newer release the last check found; the "Update available" window shows while set.
    found: Option<Release>,
    licenses_open: bool,
    /// [`licenses_text`] split into lines, built the first time the window opens.
    license_lines: Vec<String>,
}

impl Help {
    pub fn new(_opts: &AppOptions) -> Self {
        Self {
            check: None,
            found: None,
            licenses_open: false,
            license_lines: Vec::new(),
        }
    }

    fn start_check(&mut self, cx: &mut FeatureCtx<'_>) {
        if self.check.is_some() {
            cx.toasts
                .push("Already checking for updates", ToastKind::Info);
            return;
        }
        match UpdateCheck::spawn(cx.platform.clone()) {
            Ok(c) => self.check = Some(c),
            Err(e) => cx
                .toasts
                .push(format!("Update check failed: {e}"), ToastKind::Error),
        }
    }

    fn finish_check(&mut self, reply: Result<String, String>, cx: &mut FeatureCtx<'_>) {
        match outcome(reply, env!("CARGO_PKG_VERSION")) {
            UpdateOutcome::Newer(r) => {
                cx.toasts
                    .push(format!("Version {} is available", r.tag), ToastKind::Info);
                self.found = Some(r);
            }
            UpdateOutcome::Latest => cx.toasts.push(
                format!(
                    "You have the latest version ({})",
                    env!("CARGO_PKG_VERSION")
                ),
                ToastKind::Info,
            ),
            UpdateOutcome::NoReleases => {
                cx.toasts.push("No releases published yet", ToastKind::Info)
            }
            UpdateOutcome::Failed(e) => cx
                .toasts
                .push(format!("Update check failed: {e}"), ToastKind::Error),
        }
    }

    fn licenses_window(&mut self, ctx: &egui::Context) {
        if self.license_lines.is_empty() {
            self.license_lines = licenses_text().lines().map(str::to_owned).collect();
        }
        let lines = &self.license_lines;
        egui::Window::new("Third-party licenses")
            .open(&mut self.licenses_open)
            .collapsible(false)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .default_size([440.0, 560.0])
            .vscroll(false)
            .show(ctx, |ui| {
                let row = ui.text_style_height(&TextStyle::Monospace);
                // Only the visible lines are laid out: the text is several hundred KiB.
                ScrollArea::both().auto_shrink([false, false]).show_rows(
                    ui,
                    row,
                    lines.len(),
                    |ui, range| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for line in &lines[range] {
                            ui.add(egui::Label::new(RichText::new(line).monospace()).extend());
                        }
                    },
                );
            });
    }

    fn update_window(&mut self, ctx: &egui::Context, cx: &mut FeatureCtx<'_>) {
        let Some(release) = &self.found else {
            return;
        };
        let mut open = true;
        let mut clicked = false;
        egui::Window::new("Update available")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .default_width(300.0)
            .show(ctx, |ui| {
                ui.label(format!(
                    "A new version, {}, is available. You have {}.",
                    release.tag,
                    env!("CARGO_PKG_VERSION")
                ));
                ui.add_space(6.0);
                clicked = ui.button("Open download page").clicked();
            });
        if clicked {
            match cx.platform.open_url(&release.url) {
                Ok(()) => open = false,
                Err(e) => cx
                    .toasts
                    .push(format!("Could not open the browser: {e}"), ToastKind::Error),
            }
        }
        if !open {
            self.found = None;
        }
    }
}

impl Feature for Help {
    fn on_action(&mut self, action: Action, cx: &mut FeatureCtx<'_>) -> bool {
        match action {
            Action::Licenses => self.licenses_open = true,
            Action::CheckUpdates => self.start_check(cx),
            Action::ReportBug => {
                if let Err(e) = cx.platform.open_url(ISSUES_URL) {
                    cx.toasts
                        .push(format!("Could not open the browser: {e}"), ToastKind::Error);
                }
            }
            _ => return false,
        }
        true
    }

    fn frame(&mut self, cx: &mut FeatureCtx<'_>) {
        let Some(reply) = self.check.as_ref().and_then(UpdateCheck::poll) else {
            return;
        };
        self.check = None;
        self.finish_check(reply, cx);
    }

    fn ui(&mut self, ctx: &egui::Context, cx: &mut FeatureCtx<'_>) {
        if self.check.is_some() {
            // `frame` polls the worker; keep frames coming while paused.
            ctx.request_repaint_after(UPDATE_POLL);
        }
        if self.licenses_open {
            self.licenses_window(ctx);
        }
        self.update_window(ctx, cx);
    }

    fn active(&self, action: Action) -> bool {
        match action {
            Action::Licenses => self.licenses_open,
            Action::CheckUpdates => self.check.is_some(),
            _ => false,
        }
    }

    fn next_deadline(&self) -> Option<u64> {
        None
    }

    fn blocks_input(&self) -> bool {
        false
    }

    fn on_exit(&mut self, _cx: &mut FeatureCtx<'_>) {
        // A running check is abandoned: its thread ends on its own and its reply is dropped.
        self.check = None;
    }
}
