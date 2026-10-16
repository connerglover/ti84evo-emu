//! Settings › Instances: start another instance, or give this one a new ID.
//!
//! Both go through [`Platform::spawn_instance`](crate::platform::Platform::spawn_instance).
//! "New instance" starts `--instance <next free id>`. "Change ID" starts
//! `--instance <new> --rename-from <old>` and closes this window's app: the new process waits
//! for this one's lock to go away, then renames the directory (see [`crate::instance`]).

use std::path::{Path, PathBuf};

use egui::{Align2, Button, RichText, TextEdit};

use crate::feature::{AppRequest, FeatureCtx};
use crate::instance::{instance_dir, next_free_id, validate_id};
use crate::toast::ToastKind;

/// The window's title.
pub const INSTANCES_TITLE: &str = "Instances";
/// The tooltip of the disabled "Change ID" button of the default instance.
pub const DEFAULT_NOT_RENAMED: &str = "The default instance cannot be renamed";

/// Width of the window, in points.
const WINDOW_W: f32 = 260.0;

/// The data base all instances share: `data_dir` itself for the default instance, else
/// `data_dir/../..` (instance `id` lives in `base/instances/<id>`).
pub fn base_dir(data_dir: &Path, instance: Option<&str>) -> PathBuf {
    match instance {
        None => data_dir.to_path_buf(),
        Some(_) => data_dir
            .parent()
            .and_then(Path::parent)
            .unwrap_or(data_dir)
            .to_path_buf(),
    }
}

/// The Instances window.
#[derive(Debug, Default)]
pub struct InstancesWindow {
    open: bool,
    /// The text of the "New ID" field.
    new_id: String,
    /// The inline error of "Change ID".
    error: Option<String>,
}

impl InstancesWindow {
    /// Opens the window with an empty "New ID" field.
    pub fn open(&mut self) {
        *self = Self {
            open: true,
            ..Self::default()
        };
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// "New instance": starts the next free instance id.
    fn new_instance(cx: &mut FeatureCtx<'_>) {
        let base = base_dir(cx.data_dir, cx.instance);
        let id = next_free_id(&base);
        match cx
            .platform
            .spawn_instance(&["--instance".to_owned(), id.clone()])
        {
            Ok(()) => cx
                .toasts
                .push(format!("Started instance {id}"), ToastKind::Info),
            Err(e) => cx.toasts.push(
                format!("Could not start an instance: {e}"),
                ToastKind::Error,
            ),
        }
    }

    /// "Change ID": checks the new id, saves the flash, starts the renaming process and closes
    /// this one. Does nothing for the default instance.
    fn change_id(&mut self, cx: &mut FeatureCtx<'_>) {
        let Some(old) = cx.instance else {
            return;
        };
        let new = self.new_id.trim().to_owned();
        if let Err(e) = validate_id(&new) {
            self.error = Some(e.to_string());
            return;
        }
        if new == old {
            self.error = Some(format!("This instance is already called {new}"));
            return;
        }
        let base = base_dir(cx.data_dir, cx.instance);
        if instance_dir(&base, Some(&new)).exists() {
            self.error = Some(format!("Instance {new} already exists"));
            return;
        }
        self.error = None;
        cx.requests.push(AppRequest::Persist);
        let args = [
            "--instance".to_owned(),
            new,
            "--rename-from".to_owned(),
            old.to_owned(),
        ];
        match cx.platform.spawn_instance(&args) {
            Ok(()) => cx.requests.push(AppRequest::Close),
            Err(e) => cx.toasts.push(
                format!("Could not start an instance: {e}"),
                ToastKind::Error,
            ),
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, cx: &mut FeatureCtx<'_>) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(INSTANCES_TITLE)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .default_width(WINDOW_W)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(format!("Instance: {}", cx.instance.unwrap_or("default")))
                        .strong(),
                );
                ui.add_space(4.0);
                if ui.button("New instance").clicked() {
                    Self::new_instance(cx);
                }
                ui.separator();
                let named = cx.instance.is_some();
                ui.horizontal(|ui| {
                    ui.label("New ID");
                    ui.add(
                        TextEdit::singleline(&mut self.new_id)
                            .desired_width(140.0)
                            .hint_text(RichText::new("lab-2").weak()),
                    );
                });
                let change = ui
                    .add_enabled(named, Button::new("Change ID"))
                    .on_disabled_hover_text(DEFAULT_NOT_RENAMED);
                if change.clicked() {
                    self.change_id(cx);
                }
                if let Some(e) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            });
        if !open {
            self.open = false;
        }
    }
}
