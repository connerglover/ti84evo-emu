//! Files: import and export of RAM, calculator state, debug state, settings and flash.
//!
//! Every action asks for a path through the platform's dialogs (cancel does nothing), then
//! reports one toast: `Exported {what} to {path}`, `Imported {what}`, `Export failed: …` or
//! `Import failed: …`. An import checks the file's size from its metadata before reading it
//! ([`IMPORT_MAX_BYTES`]), validates format and version, and changes nothing when any check
//! fails. RAM, state and flash imports persist flash first, as slot loads do, and resync the
//! app afterwards.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::app::AppOptions;
use crate::backend::Watchpoint;
use crate::feature::{AppRequest, Feature, FeatureCtx};
use crate::platform::FileFilter;
use crate::prefs::Prefs;
use crate::store::{self, read_state_file, write_state_file};
use crate::toast::ToastKind;
use crate::toolbar::Action;

/// Largest file any import reads.
pub const IMPORT_MAX_BYTES: u64 = 64 << 20;

/// [`DebugStateFile::format`] of a debug state file.
pub const DEBUG_FORMAT: &str = "evo-gui-debug";
/// [`SettingsFile::format`] of a settings file.
pub const SETTINGS_FORMAT: &str = "evo-gui-settings";
/// The only version of either JSON file this build writes and reads.
pub const FILE_VERSION: u32 = 1;

/// Refuses `path` unless it is a file of at most `max` bytes, reading nothing: a directory is
/// `"is a directory"`, a larger file `"file is too large"`.
pub fn check_size(path: &Path, max: u64) -> Result<(), String> {
    store::check_bounded(path, max).map_err(|e| e.to_string())
}

/// Reads `path` after [`check_size`]; the read is bounded by `max` too.
pub fn read_limited(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    store::read_bounded(path, max).map_err(|e| e.to_string())
}

/// The breakpoints and watchpoints, as Files › Export debug state writes them (JSON).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DebugStateFile {
    /// Always [`DEBUG_FORMAT`].
    pub format: String,
    /// Always [`FILE_VERSION`].
    pub version: u32,
    pub breakpoints: Vec<u32>,
    pub watchpoints: Vec<Watchpoint>,
}

/// The preferences, as Files › Export settings writes them (JSON).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingsFile {
    /// Always [`SETTINGS_FORMAT`].
    pub format: String,
    /// Always [`FILE_VERSION`].
    pub version: u32,
    pub prefs: Prefs,
}

/// One kind of file the Files group moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Ram,
    State,
    Debug,
    Settings,
    Flash,
}

impl Kind {
    /// The kind and direction (`true` = export) of `a`, if it is a Files action.
    fn of(a: Action) -> Option<(Kind, bool)> {
        Some(match a {
            Action::ExportRam => (Kind::Ram, true),
            Action::ImportRam => (Kind::Ram, false),
            Action::ExportState => (Kind::State, true),
            Action::ImportState => (Kind::State, false),
            Action::ExportDebug => (Kind::Debug, true),
            Action::ImportDebug => (Kind::Debug, false),
            Action::ExportSettings => (Kind::Settings, true),
            Action::ImportSettings => (Kind::Settings, false),
            Action::ExportFlash => (Kind::Flash, true),
            Action::ImportFlash => (Kind::Flash, false),
            _ => return None,
        })
    }

    /// The `{what}` of the toasts.
    fn what(self) -> &'static str {
        match self {
            Kind::Ram => "RAM image",
            Kind::State => "calculator state",
            Kind::Debug => "debug state",
            Kind::Settings => "settings",
            Kind::Flash => "flash image",
        }
    }

    fn export_title(self) -> &'static str {
        match self {
            Kind::Ram => "Export RAM image",
            Kind::State => "Export calculator state",
            Kind::Debug => "Export debug state",
            Kind::Settings => "Export settings",
            Kind::Flash => "Export flash image",
        }
    }

    fn import_title(self) -> &'static str {
        match self {
            Kind::Ram => "Import RAM image",
            Kind::State => "Import calculator state",
            Kind::Debug => "Import debug state",
            Kind::Settings => "Import settings",
            Kind::Flash => "Import flash image",
        }
    }

    fn default_name(self) -> &'static str {
        match self {
            Kind::Ram => "evo.evoram",
            Kind::State => "evo.evostate",
            Kind::Debug => "evo.evodebug",
            Kind::Settings => "evo-settings.json",
            Kind::Flash => "evo.evoflash",
        }
    }

    fn filter(self) -> FileFilter {
        match self {
            Kind::Ram => FileFilter {
                name: "RAM image",
                extensions: &["evoram"],
            },
            Kind::State => FileFilter {
                name: "Calculator state",
                extensions: &["evostate"],
            },
            Kind::Debug => FileFilter {
                name: "Debug state",
                extensions: &["evodebug"],
            },
            Kind::Settings => FileFilter {
                name: "Settings",
                extensions: &["json"],
            },
            Kind::Flash => FileFilter {
                name: "Flash image",
                extensions: &["evoflash"],
            },
        }
    }
}

/// The files feature: owns the ten Files › Import/Export actions. It keeps no state between
/// actions; each one runs to completion inside `on_action`.
pub struct Files {
    _priv: (),
}

impl Files {
    pub fn new(_opts: &AppOptions) -> Self {
        Self { _priv: () }
    }
}

/// Writes flash before an import replaces the machine's contents, as slot loads do. Skipped
/// while the unit still needs provisioning (see the app's `persist`). A failure is a toast and
/// the import goes on.
fn persist_first(cx: &mut FeatureCtx<'_>) {
    if cx.backend.needs_provision() {
        return;
    }
    if let Err(e) = cx.backend.persist() {
        cx.toasts
            .push(format!("Flash not saved: {e}"), ToastKind::Error);
    }
}

fn to_json<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    serde_json::to_vec_pretty(value).map_err(|e| e.to_string())
}

fn export(kind: Kind, path: &Path, cx: &mut FeatureCtx<'_>) -> Result<(), String> {
    let write = |bytes: Vec<u8>| store::write_atomic(path, &bytes).map_err(|e| e.to_string());
    match kind {
        Kind::Ram => write(cx.backend.export_ram().map_err(|e| e.to_string())?),
        Kind::State => write_state_file(path, &*cx.backend).map_err(|e| e.to_string()),
        Kind::Debug => write(to_json(&DebugStateFile {
            format: DEBUG_FORMAT.to_owned(),
            version: FILE_VERSION,
            breakpoints: cx.backend.breakpoints(),
            watchpoints: cx.backend.watchpoints(),
        })?),
        Kind::Settings => write(to_json(&SettingsFile {
            format: SETTINGS_FORMAT.to_owned(),
            version: FILE_VERSION,
            prefs: cx.prefs.clone(),
        })?),
        Kind::Flash => cx.backend.export_flash(path).map_err(|e| e.to_string()),
    }
}

/// Parses a debug state file; anything but a well-formed version-1 file is refused.
fn parse_debug(bytes: &[u8]) -> Result<DebugStateFile, String> {
    let bad = || "not a debug state file".to_owned();
    let f: DebugStateFile = serde_json::from_slice(bytes).map_err(|_| bad())?;
    if f.format != DEBUG_FORMAT || f.version != FILE_VERSION {
        return Err(bad());
    }
    Ok(f)
}

/// Parses a settings file; anything but a well-formed version-1 file is refused.
fn parse_settings(bytes: &[u8]) -> Result<SettingsFile, String> {
    let bad = || "not a settings file".to_owned();
    let f: SettingsFile = serde_json::from_slice(bytes).map_err(|_| bad())?;
    if f.format != SETTINGS_FORMAT || f.version != FILE_VERSION {
        return Err(bad());
    }
    Ok(f)
}

fn import(kind: Kind, path: &Path, cx: &mut FeatureCtx<'_>) -> Result<(), String> {
    match kind {
        Kind::Ram => {
            let bytes = read_limited(path, IMPORT_MAX_BYTES)?;
            persist_first(cx);
            cx.backend.import_ram(&bytes).map_err(|e| e.to_string())?;
            cx.requests.push(AppRequest::Resync);
        }
        Kind::State => {
            // Refused before the flash is persisted; `read_state_file` bounds its read again.
            check_size(path, IMPORT_MAX_BYTES)?;
            persist_first(cx);
            read_state_file(path, &mut *cx.backend).map_err(|e| e.to_string())?;
            cx.requests.push(AppRequest::Resync);
        }
        Kind::Debug => {
            let f = parse_debug(&read_limited(path, IMPORT_MAX_BYTES)?)?;
            // Watchpoints first: an invalid one is refused and the breakpoints stay as they are.
            cx.backend
                .set_watchpoints(f.watchpoints)
                .map_err(|e| e.to_string())?;
            cx.backend.set_breakpoints(f.breakpoints);
        }
        Kind::Settings => {
            let f = parse_settings(&read_limited(path, IMPORT_MAX_BYTES)?)?;
            // The linked bundle belongs to this installation, not to the settings file.
            let prefs = Prefs {
                bundle: cx.prefs.bundle.clone(),
                ..f.prefs
            };
            cx.requests.push(AppRequest::ApplyPrefs(prefs));
        }
        Kind::Flash => {
            // A directory or an oversized file never reaches the backend.
            check_size(path, IMPORT_MAX_BYTES)?;
            persist_first(cx);
            cx.backend.import_flash(path).map_err(|e| e.to_string())?;
            cx.requests.push(AppRequest::Resync);
        }
    }
    Ok(())
}

fn pick(kind: Kind, export: bool, cx: &FeatureCtx<'_>) -> Option<PathBuf> {
    let filters = [kind.filter()];
    if export {
        cx.platform
            .pick_save(kind.export_title(), kind.default_name(), &filters)
    } else {
        cx.platform.pick_open(kind.import_title(), &filters)
    }
}

impl Feature for Files {
    fn on_action(&mut self, action: Action, cx: &mut FeatureCtx<'_>) -> bool {
        let Some((kind, is_export)) = Kind::of(action) else {
            return false;
        };
        let Some(path) = pick(kind, is_export, cx) else {
            return true;
        };
        let what = kind.what();
        if is_export {
            match export(kind, &path, cx) {
                Ok(()) => cx.toasts.push(
                    format!("Exported {what} to {}", path.display()),
                    ToastKind::Info,
                ),
                Err(e) => cx
                    .toasts
                    .push(format!("Export failed: {e}"), ToastKind::Error),
            }
        } else {
            match import(kind, &path, cx) {
                Ok(()) => cx.toasts.push(format!("Imported {what}"), ToastKind::Info),
                Err(e) => cx
                    .toasts
                    .push(format!("Import failed: {e}"), ToastKind::Error),
            }
        }
        true
    }
    fn frame(&mut self, _cx: &mut FeatureCtx<'_>) {}
    fn ui(&mut self, _ctx: &egui::Context, _cx: &mut FeatureCtx<'_>) {}
    fn active(&self, _action: Action) -> bool {
        false
    }
    fn next_deadline(&self) -> Option<u64> {
        None
    }
    fn blocks_input(&self) -> bool {
        false
    }
    fn on_exit(&mut self, _cx: &mut FeatureCtx<'_>) {}
}
