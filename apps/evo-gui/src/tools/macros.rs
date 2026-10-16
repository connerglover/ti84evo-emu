//! Macros / AutoTester: record, play, save and load key scripts, and optionally check the
//! screen's SHA-256 once a played script has settled.
//!
//! Playback is scheduled in machine cycles: event `i` lands at `start + at_ms * CLOCK_HZ /
//! 1000`, where `start` is the cycle count when "Play" was clicked. [`Macros::next_deadline`]
//! reports the next scheduled cycle, so the app's run stops there and every event lands at its
//! exact cycle at any speed. Cycles do not advance while the app is paused, so a pause (from
//! any source) freezes the schedule with its held keys still held; resuming continues it.
//! Playback ends at its end cycle, on "Stop" (which releases the keys it holds), or when the
//! cycle counter goes below the last cycle playback saw (Reset, a state load, a new backend).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use egui::{Align2, Button, Color32, RichText, TextEdit, Window};
use sha2::{Digest, Sha256};

use crate::backend::{CLOCK_HZ, EmuBackend, Frame, KeyEvent};
use crate::feature::{AppRequest, FeatureCtx};
use crate::keypad::KeyId;
use crate::toast::ToastKind;
use crate::tools::script::{ScriptEvent, ms_between, parse_script, to_script};

/// How long a played script settles before the screen check, in milliseconds.
pub const DEFAULT_SETTLE_MS: u64 = 500;

/// The window title.
pub const MACROS_TITLE: &str = "Macros / AutoTester";

/// The largest macro file "Load" reads; bigger files are refused unread.
pub const MAX_MACRO_BYTES: u64 = 1024 * 1024;

const MAX_NAME_LEN: usize = 64;
const EXPECT_HEADER: &str = "# expect-sha256:";
const SETTLE_HEADER: &str = "# settle-ms:";
const CYCLES_PER_MS: u64 = CLOCK_HZ / 1000;
const WINDOW_W: f32 = 380.0;
const NAME_ERROR: &str = "Macro name must be 1\u{2013}64 letters, digits, spaces, '_' or '-', not starting or \
     ending with a space, and not a reserved device name";
const HASH_ERROR: &str = "Expected SHA-256 must be 64 hex digits";
const SAVE_HASH_ERROR: &str = "Saving with the screen check needs an Expected SHA-256";
const SETTLE_ERROR: &str = "Settle (ms) must be a whole number of milliseconds";
const NO_FRAME: &str = "No display frame";

/// The lowercase hex SHA-256 of `frame.rgba` (the pixels only; the size is not hashed).
pub fn screen_sha256(frame: &Frame) -> String {
    hex(&Sha256::digest(&frame.rgba))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// `s` trimmed and lower-cased when it is 64 hex digits.
fn normalize_hash(s: &str) -> Option<String> {
    let s = s.trim();
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

/// A whole number of milliseconds (ASCII digits only).
fn parse_ms(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// A saved macro: a key script, the screen hash it should end on and the settle time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacroFile {
    pub name: String,
    pub script: String,
    /// Lowercase hex; `None` when the screen is not checked.
    pub expect_sha256: Option<String>,
    pub settle_ms: u64,
}

impl MacroFile {
    /// The file text: the headers, then the script and a final newline.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        if let Some(h) = &self.expect_sha256 {
            out.push_str(&format!("{EXPECT_HEADER} {h}\n"));
        }
        out.push_str(&format!("{SETTLE_HEADER} {}\n", self.settle_ms));
        out.push_str(self.script.trim_end());
        out.push('\n');
        out
    }

    /// Reads a macro file. The headers `# expect-sha256: <64 hex>` and `# settle-ms: <n>` may
    /// be on any line; other lines starting with `#` are ignored; every other line is the
    /// script. A missing settle header means [`DEFAULT_SETTLE_MS`].
    pub fn parse(name: &str, text: &str) -> Result<Self, String> {
        let mut expect = None;
        let mut settle = DEFAULT_SETTLE_MS;
        let mut script = Vec::new();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if let Some(v) = line.strip_prefix(EXPECT_HEADER) {
                expect = Some(
                    normalize_hash(v)
                        .ok_or_else(|| format!("`{}` is not a 64-digit hex SHA-256", v.trim()))?,
                );
            } else if let Some(v) = line.strip_prefix(SETTLE_HEADER) {
                settle = parse_ms(v)
                    .ok_or_else(|| format!("`{}` is not a whole number of ms", v.trim()))?;
            } else if !line.trim_start().starts_with('#') {
                script.push(line);
            }
        }
        Ok(Self {
            name: name.to_owned(),
            script: script.join("\n").trim().to_owned(),
            expect_sha256: expect,
            settle_ms: settle,
        })
    }
}

/// Device names Windows reserves in every directory (with any extension).
fn reserved_name(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    matches!(n.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((n.starts_with("COM") || n.starts_with("LPT"))
            && n.len() == 4
            && n.as_bytes()[3].is_ascii_digit())
}

/// `{data_dir}/macros/{name}.keys`. `name` is 1–64 of `[A-Za-z0-9 _-]`, does not start or end
/// with a space, and is not a Windows device name, so it cannot leave the macros directory.
pub fn macro_path(data_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let ok = (1..=MAX_NAME_LEN).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'_' | b'-'))
        && !name.starts_with(' ')
        && !name.ends_with(' ')
        && !reserved_name(name);
    if !ok {
        return Err(NAME_ERROR.to_owned());
    }
    Ok(data_dir.join("macros").join(format!("{name}.keys")))
}

/// The names of the `.keys` files in `{data_dir}/macros` that [`macro_path`] accepts, sorted.
fn saved_macros(data_dir: &Path) -> Vec<String> {
    let Ok(dir) = fs::read_dir(data_dir.join("macros")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = dir
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let file = e.file_name().into_string().ok()?;
            let name = file.strip_suffix(".keys")?.to_owned();
            macro_path(data_dir, &name).is_ok().then_some(name)
        })
        .collect();
    names.sort();
    names
}

/// Reads and parses the macro `name`, checking its size before reading.
fn load_macro(data_dir: &Path, name: &str) -> Result<MacroFile, String> {
    let path = macro_path(data_dir, name)?;
    let meta = fs::metadata(&path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a file".to_owned());
    }
    if meta.len() > MAX_MACRO_BYTES {
        return Err(format!(
            "the file is too large ({} bytes, at most {MAX_MACRO_BYTES})",
            meta.len()
        ));
    }
    let bytes = fs::read(&path).map_err(|e| e.to_string())?;
    let text = String::from_utf8(bytes).map_err(|_| "the file is not UTF-8 text".to_owned())?;
    MacroFile::parse(name, &text)
}

/// How a played macro ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MacroResult {
    /// The screen matched the expected hash.
    Pass,
    /// The screen hashed to `got` instead.
    Fail { got: String },
    /// The screen was checked but the display was off.
    NoFrame,
    /// The script ran to the end (no screen check).
    Done,
}

impl MacroResult {
    /// The result label.
    pub fn label(&self) -> String {
        match self {
            MacroResult::Pass => "Result: PASS".to_owned(),
            MacroResult::Fail { got } => format!("Result: FAIL (screen sha256 {got})"),
            MacroResult::NoFrame => "Result: no display frame".to_owned(),
            MacroResult::Done => "Result: done".to_owned(),
        }
    }
}

/// A running playback.
#[derive(Debug)]
struct Playback {
    /// `(cycle, key, pressed)`, in script order (cycles never decrease).
    events: Vec<(u64, KeyId, bool)>,
    next: usize,
    /// The cycle playback ends at: the last event, plus the settle time when checking.
    end: u64,
    /// The expected hash when the screen is checked.
    expect: Option<String>,
    /// The latest cycle count playback saw; a lower one means the machine restarted.
    seen: u64,
    /// The keys playback holds down.
    held: BTreeSet<KeyId>,
}

impl Playback {
    fn next_cycle(&self) -> u64 {
        self.events.get(self.next).map_or(self.end, |e| e.0)
    }
}

/// The Macros / AutoTester window and its recorder and player.
#[derive(Debug)]
pub struct Macros {
    open: bool,
    name: String,
    script: String,
    check: bool,
    settle: String,
    expected: String,
    /// The key events recorded so far, while recording.
    recording: Option<Vec<KeyEvent>>,
    playback: Option<Playback>,
    result: Option<MacroResult>,
    error: Option<String>,
    /// The saved macro names, while the Load list is shown.
    load_list: Option<Vec<String>>,
}

impl Default for Macros {
    fn default() -> Self {
        Self::new()
    }
}

impl Macros {
    pub fn new() -> Self {
        Self {
            open: false,
            name: String::new(),
            script: String::new(),
            check: false,
            settle: DEFAULT_SETTLE_MS.to_string(),
            expected: String::new(),
            recording: None,
            playback: None,
            result: None,
            error: None,
            load_list: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the window when closed, closes it when open (recording and playback go on).
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: &str) {
        self.name = name.to_owned();
    }

    pub fn script(&self) -> &str {
        &self.script
    }

    pub fn set_script(&mut self, script: &str) {
        self.script = script.to_owned();
    }

    /// Whether "Check screen at end" is ticked.
    pub fn check(&self) -> bool {
        self.check
    }

    /// The "Expected SHA-256" field.
    pub fn expected(&self) -> &str {
        &self.expected
    }

    /// Sets "Check screen at end" and the "Expected SHA-256" field.
    pub fn set_check(&mut self, check: bool, expected: &str) {
        self.check = check;
        self.expected = expected.to_owned();
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    pub fn is_playing(&self) -> bool {
        self.playback.is_some()
    }

    /// How the last playback ended; `None` before any or after "Stop".
    pub fn result(&self) -> Option<&MacroResult> {
        self.result.as_ref()
    }

    /// The inline error, if any.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The cycle the next run must stop at: the next scripted event, then the end cycle.
    pub fn next_deadline(&self) -> Option<u64> {
        self.playback.as_ref().map(Playback::next_cycle)
    }

    fn start_recording(&mut self) {
        self.recording = Some(Vec::new());
        self.error = None;
    }

    /// Writes the recording into the script box. Times count from the first recorded key, so
    /// the time before it is not part of the macro.
    fn stop_recording(&mut self) {
        let Some(events) = self.recording.take() else {
            return;
        };
        let Some(first) = events.first().map(|e| e.cycle) else {
            self.script.clear();
            return;
        };
        let events: Vec<ScriptEvent> = events
            .iter()
            .map(|e| ScriptEvent {
                at_ms: ms_between(first, e.cycle),
                key: e.key,
                pressed: e.pressed,
            })
            .collect();
        self.script = to_script(&events);
    }

    fn record(&mut self, events: &[KeyEvent]) {
        let Some(rec) = &mut self.recording else {
            return;
        };
        for e in events {
            // The cycle counter went back (Reset, a state load): start over from here.
            if rec.last().is_some_and(|l| e.cycle < l.cycle) {
                rec.clear();
            }
            rec.push(*e);
        }
    }

    /// Starts playback of the script box; problems are reported inline.
    fn play(&mut self, cx: &mut FeatureCtx<'_>) {
        self.error = None;
        self.result = None;
        let events = match parse_script(&self.script) {
            Ok(e) => e,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        let Some(settle_ms) = parse_ms(&self.settle) else {
            self.error = Some(SETTLE_ERROR.to_owned());
            return;
        };
        let expect = if !self.check {
            None
        } else if self.expected.trim().is_empty() {
            // No hash yet: the run still settles and checks, and the FAIL result shows the
            // screen hash to use as the expectation (an empty string matches no hash).
            Some(String::new())
        } else {
            match normalize_hash(&self.expected) {
                Some(h) => Some(h),
                None => {
                    self.error = Some(HASH_ERROR.to_owned());
                    return;
                }
            }
        };
        let start = cx.backend.cycles();
        let at = |ms: u64| start.saturating_add(ms.saturating_mul(CYCLES_PER_MS));
        let events: Vec<(u64, KeyId, bool)> = events
            .iter()
            .map(|e| (at(e.at_ms), e.key, e.pressed))
            .collect();
        let last = events.last().map_or(start, |e| e.0);
        let end = if expect.is_some() {
            last.saturating_add(settle_ms.saturating_mul(CYCLES_PER_MS))
        } else {
            last
        };
        self.playback = Some(Playback {
            events,
            next: 0,
            end,
            expect,
            seen: start,
            held: BTreeSet::new(),
        });
        if cx.paused {
            cx.requests.push(AppRequest::SetPaused(false));
        }
    }

    /// Ends playback early, releasing the keys it holds.
    fn stop_playback(&mut self, backend: &mut dyn EmuBackend) {
        if let Some(pb) = self.playback.take() {
            for key in pb.held {
                backend.set_key(key, false);
            }
        }
    }

    /// Applies the events that are due and, at the end cycle, the result.
    fn advance(&mut self, cx: &mut FeatureCtx<'_>) {
        let Some(pb) = &mut self.playback else {
            return;
        };
        let now = cx.backend.cycles();
        if now < pb.seen {
            // Reset, a state load or a new backend: the app released every key already.
            self.playback = None;
            cx.toasts
                .push("Macro stopped: the machine restarted", ToastKind::Info);
            return;
        }
        pb.seen = now;
        while let Some(&(cycle, key, pressed)) = pb.events.get(pb.next) {
            if cycle > now {
                return;
            }
            cx.backend.set_key(key, pressed);
            if pressed {
                pb.held.insert(key);
            } else {
                pb.held.remove(&key);
            }
            pb.next += 1;
        }
        if now < pb.end {
            return;
        }
        let result = match &pb.expect {
            None => MacroResult::Done,
            Some(want) => match cx.backend.framebuffer() {
                None => MacroResult::NoFrame,
                Some(f) => {
                    let got = screen_sha256(f);
                    if &got == want {
                        MacroResult::Pass
                    } else {
                        MacroResult::Fail { got }
                    }
                }
            },
        };
        self.result = Some(result);
        self.playback = None;
    }

    /// Called every frame, also while the window is closed.
    pub fn frame(&mut self, cx: &mut FeatureCtx<'_>) {
        self.record(cx.key_events);
        self.advance(cx);
    }

    /// The app is closing: playback stops and its keys are released.
    pub fn on_exit(&mut self, cx: &mut FeatureCtx<'_>) {
        self.stop_playback(cx.backend);
        self.recording = None;
    }

    fn use_current_screen(&mut self, backend: &mut dyn EmuBackend) {
        match backend.framebuffer() {
            Some(f) => {
                self.expected = screen_sha256(f);
                self.check = true;
                self.error = None;
            }
            None => self.error = Some(NO_FRAME.to_owned()),
        }
    }

    fn save(&mut self, cx: &mut FeatureCtx<'_>) {
        self.error = None;
        let path = match macro_path(cx.data_dir, &self.name) {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        let Some(settle_ms) = parse_ms(&self.settle) else {
            self.error = Some(SETTLE_ERROR.to_owned());
            return;
        };
        let expect_sha256 = if !self.check {
            None
        } else if self.expected.trim().is_empty() {
            // A file cannot say "check, hash unknown": Load would turn the check off.
            self.error = Some(SAVE_HASH_ERROR.to_owned());
            return;
        } else {
            match normalize_hash(&self.expected) {
                Some(h) => Some(h),
                None => {
                    self.error = Some(HASH_ERROR.to_owned());
                    return;
                }
            }
        };
        let file = MacroFile {
            name: self.name.clone(),
            script: self.script.clone(),
            expect_sha256,
            settle_ms,
        };
        let written = path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(&path, file.to_text()));
        match written {
            Ok(()) => {
                cx.toasts
                    .push(format!("Saved macro {}", self.name), ToastKind::Info);
                if self.load_list.is_some() {
                    self.load_list = Some(saved_macros(cx.data_dir));
                }
            }
            Err(e) => cx
                .toasts
                .push(format!("Save failed: {e}"), ToastKind::Error),
        }
    }

    fn load(&mut self, name: &str, cx: &mut FeatureCtx<'_>) {
        match load_macro(cx.data_dir, name) {
            Ok(m) => {
                self.name = m.name;
                self.script = m.script;
                self.check = m.expect_sha256.is_some();
                self.expected = m.expect_sha256.unwrap_or_default();
                self.settle = m.settle_ms.to_string();
                self.error = None;
                self.load_list = None;
            }
            Err(e) => {
                let text = format!("Load failed: {e}");
                cx.toasts.push(text.clone(), ToastKind::Error);
                self.error = Some(text);
            }
        }
    }

    /// Draws the window while open.
    pub fn ui(&mut self, ctx: &egui::Context, cx: &mut FeatureCtx<'_>) {
        if !self.open {
            return;
        }
        let mut open = true;
        Window::new(MACROS_TITLE)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .vscroll(false)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .default_width(WINDOW_W)
            .show(ctx, |ui| self.contents(ui, cx));
        if !open {
            self.open = false;
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui, cx: &mut FeatureCtx<'_>) {
        let idle = !self.is_recording() && !self.is_playing();
        ui.horizontal(|ui| {
            let label = ui.label("Name");
            ui.add_enabled(
                idle,
                TextEdit::singleline(&mut self.name).desired_width(200.0),
            )
            .labelled_by(label.id);
        });
        let label = ui.label("Script");
        ui.add_enabled(
            idle,
            TextEdit::multiline(&mut self.script)
                .code_editor()
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        )
        .labelled_by(label.id);
        ui.horizontal(|ui| {
            if self.is_recording() {
                if ui.button("Stop recording").clicked() {
                    self.stop_recording();
                }
            } else if ui
                .add_enabled(!self.is_playing(), Button::new("Record"))
                .clicked()
            {
                self.start_recording();
            }
            let can_play = idle && !cx.provisioning;
            if ui.add_enabled(can_play, Button::new("Play")).clicked() {
                self.play(cx);
            }
            if ui
                .add_enabled(self.is_playing(), Button::new("Stop"))
                .clicked()
            {
                self.stop_playback(cx.backend);
                self.result = None;
            }
        });
        ui.separator();
        ui.add_enabled_ui(!self.is_playing(), |ui| {
            ui.checkbox(&mut self.check, "Check screen at end");
            ui.horizontal(|ui| {
                let label = ui.label("Settle (ms)");
                ui.add(TextEdit::singleline(&mut self.settle).desired_width(70.0))
                    .labelled_by(label.id);
            });
            let label = ui.label("Expected SHA-256");
            ui.add(
                TextEdit::singleline(&mut self.expected)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY),
            )
            .labelled_by(label.id);
            if ui.button("Use current screen").clicked() {
                self.use_current_screen(cx.backend);
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.add_enabled(idle, Button::new("Save")).clicked() {
                self.save(cx);
            }
            if ui.add_enabled(idle, Button::new("Load")).clicked() {
                self.load_list = match self.load_list {
                    Some(_) => None,
                    None => Some(saved_macros(cx.data_dir)),
                };
            }
        });
        if let Some(names) = self.load_list.clone() {
            if names.is_empty() {
                ui.label(RichText::new("No saved macros").weak());
            }
            for name in names {
                if ui.add_enabled(idle, Button::new(&name)).clicked() {
                    self.load(&name, cx);
                }
            }
        }
        if let Some(e) = &self.error {
            ui.label(RichText::new(e).color(ui.visuals().error_fg_color));
        }
        if self.is_playing() {
            ui.label("Playing\u{2026}");
        } else if let Some(r) = &self.result {
            let color = match r {
                MacroResult::Pass => Color32::from_rgb(0x2E, 0x7D, 0x32),
                MacroResult::Fail { .. } | MacroResult::NoFrame => ui.visuals().error_fg_color,
                MacroResult::Done => ui.visuals().text_color(),
            };
            ui.label(RichText::new(r.label()).color(color));
        }
    }
}
