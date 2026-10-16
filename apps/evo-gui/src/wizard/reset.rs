//! Settings › Reset everything: a confirmation modal, then the instance's data is deleted and
//! the emulator starts over without a ROM.

use std::path::Path;

use egui::{Align, Id, Layout, RichText};

use crate::feature::FeatureCtx;
use crate::instance::LOCK_FILE;
use crate::platform::FileFilter;
use crate::toast::ToastKind;

/// The modal's heading.
pub const RESET_TITLE: &str = "Reset everything?";
/// The modal's text.
pub const RESET_TEXT: &str = "This deletes the flash, state slots, screenshots, recordings, \
macros and settings of this instance. The emulator then starts over with the ROM Setup Wizard.";
/// The save dialog's filter for "Export flash first…".
pub const FLASH_FILTER: FileFilter = FileFilter {
    name: "Flash image",
    extensions: &["evoflash"],
};

/// Entries of the data dir that Reset everything keeps: the lock this process holds, eframe's
/// storage file of a named instance (rewritten with default settings on exit), and the other
/// instances (the default instance's data dir is their base; some may be running).
pub const KEPT: [&str; 3] = [LOCK_FILE, "app.ron", "instances"];

/// Width of the modal's content, in points.
const RESET_W: f32 = 360.0;

/// What the user did in the modal this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetChoice {
    /// Nothing yet, or the modal is closed.
    None,
    /// "Reset everything": the caller deletes and restarts.
    Confirm,
}

/// The Reset everything modal.
#[derive(Debug, Default)]
pub struct ResetModal {
    open: bool,
}

impl ResetModal {
    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// "Export flash first…": saves the flash where the user picks; the modal stays open.
    fn export_flash(cx: &mut FeatureCtx<'_>) {
        let Some(path) =
            cx.platform
                .pick_save("Export flash image", "evo.evoflash", &[FLASH_FILTER])
        else {
            return;
        };
        match cx.backend.export_flash(&path) {
            Ok(()) => cx.toasts.push("Flash exported", ToastKind::Info),
            Err(e) => cx
                .toasts
                .push(format!("Export failed: {e}"), ToastKind::Error),
        }
    }

    /// Draws the modal; returns [`ResetChoice::Confirm`] (and closes) when confirmed.
    pub fn ui(&mut self, ctx: &egui::Context, cx: &mut FeatureCtx<'_>) -> ResetChoice {
        if !self.open {
            return ResetChoice::None;
        }
        let mut choice = ResetChoice::None;
        egui::Modal::new(Id::new("reset-everything")).show(ctx, |ui| {
            ui.set_width(RESET_W);
            ui.heading(RESET_TITLE);
            ui.add_space(4.0);
            ui.label(RESET_TEXT);
            ui.add_space(8.0);
            if ui.button("Export flash first\u{2026}").clicked() {
                Self::export_flash(cx);
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    self.open = false;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let confirm = egui::Button::new(
                        RichText::new("Reset everything").color(ui.visuals().error_fg_color),
                    );
                    if ui.add(confirm).clicked() {
                        self.open = false;
                        choice = ResetChoice::Confirm;
                    }
                });
            });
        });
        choice
    }
}

/// Deletes every entry of `data_dir` except [`KEPT`]. Each entry that cannot be deleted is
/// reported as `"Could not delete {name}: {e}"` and the rest are still deleted. A missing
/// `data_dir` has nothing to delete.
pub fn clear_data_dir(data_dir: &Path) -> Vec<String> {
    let entries = match std::fs::read_dir(data_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => return vec![format!("Could not read {}: {e}", data_dir.display())],
    };
    let mut errors = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                errors.push(format!("Could not read {}: {e}", data_dir.display()));
                continue;
            }
        };
        let name = entry.file_name();
        if KEPT.iter().any(|k| name == *k) {
            continue;
        }
        let path = entry.path();
        // `file_type` does not follow links: a link is removed, never what it points to.
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        let result = if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            // On Windows a link to a directory is removed with `remove_dir`.
            std::fs::remove_file(&path).or_else(|e| std::fs::remove_dir(&path).map_err(|_| e))
        };
        if let Err(e) = result {
            errors.push(format!("Could not delete {}: {e}", name.to_string_lossy()));
        }
    }
    errors
}
