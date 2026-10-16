//! Native USB workspace for the calculator already open in the emulator.

pub mod types;
pub(crate) mod updates;
pub(crate) mod worker;

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver},
};
use std::time::{Duration, Instant};

use crate::backend::Frame;
use crate::feature::{AppRequest, Feature, FeatureCtx};
use crate::platform::FileFilter;
use crate::theme::{BODY_OUTLINE, HEADER_BG, LABEL_DARK};
use crate::toolbar::Action;
use egui::{Button, ColorImage, RichText, ScrollArea, TextureHandle, TextureOptions};
use types::*;

const INPUT_LIMIT: u64 = 32 * 1024 * 1024;
const FILE_FILTER: &[FileFilter] = &[FileFilter {
    name: "Calculator files, Python, images and CSV",
    extensions: &[
        "8xp2", "8xv2", "8xl2", "8xm2", "8xn2", "8xs2", "8ca2", "8ci2", "8mp2", "8xd2", "8xg2",
        "8xt2", "8xw2", "8xy2", "8xz2", "py", "png", "csv",
    ],
}];
const OS_FILTER: &[FileFilter] = &[FileFilter {
    name: "TI-84 Evo OS bundle",
    extensions: &["84b2"],
}];
const PNG_FILTER: &[FileFilter] = &[FileFilter {
    name: "PNG image",
    extensions: &["png"],
}];

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Files,
    Screen,
    Calculator,
    Developer,
}

struct PendingFile {
    name: String,
    bytes: Vec<u8>,
    rename: String,
    image_slot: u8,
}

enum UpdateReply {
    Latest(String),
    Bundle(String, Vec<u8>),
}

pub struct ConnectTools {
    open: bool,
    tab: Tab,
    files: Vec<CalculatorFile>,
    selected: BTreeSet<(String, u32)>,
    filter: String,
    reverse_sort: bool,
    loaded: bool,
    info: String,
    status: String,
    error: bool,
    progress: Option<UsbProgress>,
    receiver: Option<Receiver<UsbEvent>>,
    cancelled: Arc<AtomicBool>,
    refresh_after: bool,
    pending: Vec<PendingFile>,
    report: Option<(String, bool)>,
    options: SendOptions,
    delete_confirm: bool,
    pending_os: Option<PendingFile>,
    receive_folder: Option<PathBuf>,
    screen: Option<Frame>,
    texture: Option<TextureHandle>,
    screen_changed: bool,
    auto_screen: bool,
    last_screen: Instant,
    uri: String,
    response: String,
    raw_response: Vec<u8>,
    debug_message: String,
    allow_write: bool,
    history: VecDeque<String>,
    latest_version: Option<String>,
    update_job: Option<std::thread::JoinHandle<Result<UpdateReply, String>>>,
    update_status: String,
}

impl Default for ConnectTools {
    fn default() -> Self {
        Self {
            open: false,
            tab: Tab::Files,
            files: Vec::new(),
            selected: BTreeSet::new(),
            filter: String::new(),
            reverse_sort: false,
            loaded: false,
            info: String::new(),
            status: "Ready to read the calculator.".into(),
            error: false,
            progress: None,
            receiver: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            refresh_after: false,
            pending: Vec::new(),
            report: None,
            options: SendOptions {
                archived: true,
                ..SendOptions::default()
            },
            delete_confirm: false,
            pending_os: None,
            receive_folder: None,
            screen: None,
            texture: None,
            screen_changed: false,
            auto_screen: false,
            last_screen: Instant::now(),
            uri: "hh01/sys/attributes".into(),
            response: String::new(),
            raw_response: Vec::new(),
            debug_message: String::new(),
            allow_write: false,
            history: VecDeque::new(),
            latest_version: None,
            update_job: None,
            update_status: String::new(),
        }
    }
}

impl ConnectTools {
    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn busy(&self) -> bool {
        self.receiver.is_some()
    }
    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn calculator_changed(&mut self) {
        self.files.clear();
        self.selected.clear();
        self.loaded = false;
        self.info.clear();
        self.screen = None;
        self.texture = None;
        self.auto_screen = false;
        self.status = "Calculator changed. Refresh to read its files.".into();
        self.error = false;
    }

    fn record(&mut self, text: impl Into<String>) {
        if self.history.len() == 200 {
            self.history.pop_front();
        }
        self.history.push_back(text.into());
    }

    fn fail(&mut self, error: impl ToString) {
        self.status = error.to_string();
        self.error = true;
        self.record(format!("Error: {}", self.status));
    }

    fn start(&mut self, operation: UsbOperation, cx: &mut FeatureCtx<'_>) {
        if self.busy() || cx.provisioning {
            return;
        }
        self.status = format!("{}…", operation.label());
        self.record(self.status.clone());
        self.error = false;
        self.progress = None;
        self.cancelled = Arc::new(AtomicBool::new(false));
        self.refresh_after = matches!(
            &operation,
            UsbOperation::Send { .. } | UsbOperation::SendMany(_) | UsbOperation::Delete(_)
        );
        let (events, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        cx.requests.push(AppRequest::Usb(UsbTask {
            operation,
            events,
            cancelled: self.cancelled.clone(),
        }));
    }

    fn selection(&self) -> Vec<CalculatorFile> {
        self.files
            .iter()
            .filter(|f| self.selected.contains(&(f.token_name.clone(), f.kind)))
            .cloned()
            .collect()
    }

    fn pick_file(
        cx: &FeatureCtx<'_>,
        title: &str,
        filters: &[FileFilter],
    ) -> Result<Option<PendingFile>, String> {
        let Some(path) = cx.platform.pick_open(title, filters) else {
            return Ok(None);
        };
        Self::read_file(&path).map(Some)
    }

    fn read_file(path: &Path) -> Result<PendingFile, String> {
        // A bounded read also handles files which grow after the metadata check.
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(INPUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > INPUT_LIMIT {
            return Err(
                "This file is larger than 32 MiB. Choose a calculator file or a smaller image."
                    .into(),
            );
        }
        Ok(PendingFile {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            bytes,
            rename: String::new(),
            image_slot: 1,
        })
    }

    fn receive(&mut self, cx: &mut FeatureCtx<'_>) {
        let files = self.selection();
        if files.is_empty() {
            return;
        }
        if let Some(folder) = cx
            .platform
            .pick_folder("Save received calculator files in…")
        {
            self.receive_folder = Some(folder);
            self.start(UsbOperation::Receive(files), cx);
        }
    }

    fn completed(&mut self, result: Result<UsbResult, String>, cx: &mut FeatureCtx<'_>) {
        self.receiver = None;
        self.progress = None;
        self.last_screen = Instant::now();
        match result {
            Err(e) => {
                self.refresh_after = false;
                self.receive_folder = None;
                self.auto_screen = false;
                self.fail(e);
            }
            Ok(result) => {
                self.error = false;
                self.status = "Ready.".into();
                match result {
                    UsbResult::Batch {
                        files,
                        errors,
                        message,
                    } => {
                        self.error = !errors.is_empty();
                        let mut report = message;
                        if let Some(folder) = self.receive_folder.take() {
                            match save_received(&folder, &files) {
                                Ok(()) => report.push_str(&format!(
                                    "\nSaved {} files to {}.",
                                    files.len(),
                                    folder.display()
                                )),
                                Err(e) => {
                                    self.error = true;
                                    report.push_str(&format!("\n{e}"));
                                }
                            }
                        }
                        for error in errors {
                            report.push_str(&format!("\n{error}"));
                        }
                        self.status.clone_from(&report);
                        self.report = Some((report, self.error));
                    }
                    UsbResult::Directory { mut files, info } => {
                        files.sort_by_key(|f| f.name.to_lowercase());
                        self.selected.retain(|id| {
                            files.iter().any(|f| id == &(f.token_name.clone(), f.kind))
                        });
                        self.files = files;
                        self.info = info;
                        self.loaded = true;
                        self.status = format!("{} files on this calculator.", self.files.len());
                    }
                    UsbResult::Files(files) => {
                        if let Some(folder) = self.receive_folder.take() {
                            match save_received(&folder, &files) {
                                Ok(()) => {
                                    self.status = format!(
                                        "Saved {} files to {}.",
                                        files.len(),
                                        folder.display()
                                    )
                                }
                                Err(e) => self.fail(e),
                            }
                        }
                    }
                    UsbResult::Screen(frame) => {
                        self.screen = Some(frame);
                        self.screen_changed = true;
                        self.status = "Screen captured over USB.".into();
                    }
                    UsbResult::Text(text) => {
                        self.info.clone_from(&text);
                        self.raw_response = text.as_bytes().to_vec();
                        self.response = text;
                    }
                    UsbResult::Bytes(bytes) => {
                        self.response = preview_bytes(&bytes);
                        self.raw_response = bytes;
                    }
                    UsbResult::Done(text) => self.status = text,
                }
                self.record(self.status.clone());
            }
        }
        if self.refresh_after {
            self.refresh_after = false;
            self.start(UsbOperation::Refresh, cx);
        }
    }

    fn files_ui(&mut self, ui: &mut egui::Ui, cx: &mut FeatureCtx<'_>, enabled: bool) {
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(enabled, Button::new("Send files…"))
                .clicked()
            {
                let paths = cx
                    .platform
                    .pick_open_many("Send to calculator", FILE_FILTER);
                if paths.len() > 100 {
                    self.fail("Choose at most 100 files per transfer.");
                } else if !paths.is_empty() {
                    let mut files = Vec::new();
                    let mut total = 0;
                    for path in paths {
                        match Self::read_file(&path) {
                            Ok(mut file) => {
                                total += file.bytes.len();
                                if total > 64 * 1024 * 1024 {
                                    self.fail(
                                        "The selected files exceed 64 MiB. Send a smaller group.",
                                    );
                                    files.clear();
                                    break;
                                }
                                file.image_slot = (files.len() % 10 + 1) as u8;
                                files.push(file);
                            }
                            Err(e) => {
                                self.fail(e);
                                files.clear();
                                break;
                            }
                        }
                    }
                    self.pending = files;
                    self.options = SendOptions {
                        archived: true,
                        ..SendOptions::default()
                    };
                }
            }
            if ui.add_enabled(enabled, Button::new("Refresh")).clicked() {
                self.start(UsbOperation::Refresh, cx);
            }
            if ui
                .add_enabled(
                    enabled && !self.selected.is_empty(),
                    Button::new("Save selected…"),
                )
                .clicked()
            {
                self.receive(cx);
            }
            if ui
                .add_enabled(enabled && !self.selected.is_empty(), Button::new("Delete…"))
                .clicked()
            {
                self.delete_confirm = true;
            }
        });
        if !self.pending.is_empty() {
            self.send_ui(ui, cx, enabled);
        }
        if let Some((report, error)) = self.report.clone() {
            ui.group(|ui| {
                ui.strong("Last transfer");
                ScrollArea::vertical()
                    .id_salt("usb-transfer-report")
                    .max_height(120.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new(report).color(if error {
                            crate::theme::TOAST_ERROR_BG
                        } else {
                            LABEL_DARK
                        }));
                    });
                if ui.small_button("Dismiss report").clicked() {
                    self.report = None;
                }
            });
        }
        if self.delete_confirm {
            ui.group(|ui| {
                ui.label(format!(
                    "Delete {} selected files from this calculator?",
                    self.selected.len()
                ));
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(enabled, Button::new("Delete from calculator"))
                        .clicked()
                    {
                        self.delete_confirm = false;
                        self.start(UsbOperation::Delete(self.selection()), cx);
                    }
                    if ui.button("Keep files").clicked() {
                        self.delete_confirm = false;
                    }
                });
            });
        }
        ui.add_space(8.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("Find a file by name or type")
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            if ui.small_button("Select visible").clicked() {
                for f in self.visible_files() {
                    self.selected.insert((f.token_name, f.kind));
                }
            }
            if ui.small_button("Clear selection").clicked() {
                self.selected.clear();
            }
            ui.label(format!("{} selected", self.selected.len()));
        });
        ui.separator();
        if !self.loaded {
            ui.add_space(12.0);
            ui.label("Your calculator files will appear here.");
            ui.label("Send a Python program, image, CSV or calculator variable. Select files to save them to your computer.");
        } else if self.files.is_empty() {
            ui.label("No files on this calculator. Use Send file to add one.");
        } else if self.visible_files().is_empty() {
            ui.label("No files match your search.");
            if ui.button("Clear filter").clicked() {
                self.filter.clear();
            }
        } else {
            ScrollArea::vertical()
                .id_salt("usb-directory")
                .max_height(310.0)
                .show(ui, |ui| {
                    egui::Grid::new("usb-files-grid")
                        .num_columns(4)
                        .spacing([12.0, 7.0])
                        .striped(true)
                        .show(ui, |ui| {
                            if ui
                                .small_button(if self.reverse_sort {
                                    "Name ↓"
                                } else {
                                    "Name ↑"
                                })
                                .clicked()
                            {
                                self.reverse_sort = !self.reverse_sort;
                            }
                            ui.strong("Type");
                            ui.strong("Size");
                            ui.strong("Location");
                            ui.end_row();
                            for f in self.visible_files() {
                                let id = (f.token_name.clone(), f.kind);
                                let mut selected = self.selected.contains(&id);
                                if ui.checkbox(&mut selected, &f.name).changed() {
                                    if selected {
                                        self.selected.insert(id);
                                    } else {
                                        self.selected.remove(&id);
                                    }
                                }
                                ui.label(file_type(&f.extension))
                                    .on_hover_text(&f.extension);
                                ui.label(byte_size(f.size));
                                ui.label(if f.archived { "Archive" } else { "RAM" });
                                ui.end_row();
                            }
                        });
                });
        }
    }

    fn visible_files(&self) -> Vec<CalculatorFile> {
        let q = self.filter.to_lowercase();
        let mut files: Vec<_> = self
            .files
            .iter()
            .filter(|f| {
                f.name.to_lowercase().contains(&q)
                    || f.extension.to_lowercase().contains(&q)
                    || file_type(&f.extension).to_lowercase().contains(&q)
            })
            .cloned()
            .collect();
        if self.reverse_sort {
            files.reverse();
        }
        files
    }

    fn send_ui(&mut self, ui: &mut egui::Ui, cx: &mut FeatureCtx<'_>, enabled: bool) {
        let name = self.pending[0].name.clone();
        let multiple = self.pending.len() > 1;
        let lower = name.to_lowercase();
        ui.add_space(8.0);
        ui.group(|ui| {
            ui.label(RichText::new(if multiple { format!("Send {} files", self.pending.len()) } else { format!("Send {name}") }).strong());
            ui.add_enabled_ui(enabled, |ui| {
                if multiple {
                    let mut remove = None;
                    ScrollArea::vertical().id_salt("usb-pending-files").max_height(150.0).show(ui, |ui| {
                        for (index, file) in self.pending.iter_mut().enumerate() {
                            ui.push_id(index, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(&file.name);
                                    if let Some(archive) = fixed_location(&file.name) { ui.label(if archive { "Archive" } else { "RAM" }); }
                                    if file.name.to_ascii_lowercase().ends_with(".png") {
                                        ui.label("Image slot"); ui.add(egui::DragValue::new(&mut file.image_slot).range(1..=10));
                                    } else {
                                        ui.add(egui::TextEdit::singleline(&mut file.rename).hint_text("Keep name").desired_width(110.0));
                                    }
                                    if ui.small_button("Remove").clicked() { remove = Some(index); }
                                });
                            });
                        }
                    });
                    if let Some(index) = remove { self.pending.remove(index); }
                } else if lower.ends_with(".png") {
                    ui.horizontal(|ui| {
                        ui.label("Save as");
                        egui::ComboBox::from_id_salt("usb-image-slot").selected_text(format!("Image{}", self.options.image_slot)).show_ui(ui, |ui| {
                            for slot in 1..=10 { ui.selectable_value(&mut self.options.image_slot, slot, format!("Image{slot}")); }
                        });
                    });
                    ui.label("The picture is resized to fit a calculator image.");
                } else {
                    ui.horizontal(|ui| {
                        ui.label(if lower.ends_with(".csv") && self.options.csv_target == CsvTarget::Lists { "List names" } else { "Calculator name" });
                        let hint = if lower.ends_with(".csv") {
                            if self.options.csv_target == CsvTarget::Lists { "L1, L2, … (default)" } else { "A (default)" }
                        } else { "Keep original name" };
                        ui.add(egui::TextEdit::singleline(&mut self.options.name).hint_text(hint).desired_width(160.0));
                    });
                }
                if self.pending.iter().any(|f| f.name.to_ascii_lowercase().ends_with(".csv")) {
                    ui.horizontal(|ui| {
                        ui.label("Import as");
                        ui.selectable_value(&mut self.options.csv_target, CsvTarget::Lists, "Lists");
                        ui.selectable_value(&mut self.options.csv_target, CsvTarget::Matrix, "Matrix");
                    });
                    ui.label("Lists: up to 6 columns and 999 rows. Matrix: up to 400 cells.");
                }
                ui.horizontal(|ui| {
                    if !multiple && let Some(archive) = fixed_location(&name) {
                        ui.label(if archive { "This file type is stored in Archive." } else { "This file type is stored in RAM." });
                    } else {
                        ui.checkbox(&mut self.options.archived, "Store in Archive").on_hover_text("Archive keeps files across emulator restarts. Some calculator file types require Archive or RAM and always use that location.");
                    }
                    ui.checkbox(&mut self.options.overwrite, "Replace existing files");
                });
                ui.label("Archive keeps files when the emulator closes. RAM does not.");
                ui.horizontal(|ui| {
                    if ui.button("Send to calculator").clicked() {
                        let files = std::mem::take(&mut self.pending).into_iter().map(|file| {
                            let mut options = self.options.clone();
                            if multiple { options.name = file.rename; options.image_slot = file.image_slot; }
                            UploadFile { filename: file.name, bytes: file.bytes, options }
                        }).collect();
                        self.start(UsbOperation::SendMany(files), cx);
                    }
                    if ui.button("Cancel files").clicked() { self.pending.clear(); }
                });
            });
        });
    }

    fn screen_ui(&mut self, ui: &mut egui::Ui, cx: &mut FeatureCtx<'_>, enabled: bool) {
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(enabled, Button::new("Capture screen"))
                .clicked()
            {
                self.start(UsbOperation::Screen, cx);
            }
            ui.add_enabled_ui(cx.backend.usb_available() || self.busy(), |ui| {
                ui.checkbox(&mut self.auto_screen, "Auto-refresh");
            });
            if ui
                .add_enabled(self.screen.is_some(), Button::new("Copy"))
                .clicked()
                && let Some(frame) = &self.screen
            {
                match cx.platform.copy_image(frame) {
                    Ok(()) => self.status = "Screen copied to clipboard.".into(),
                    Err(e) => self.fail(e),
                }
            }
            if ui
                .add_enabled(self.screen.is_some(), Button::new("Save PNG…"))
                .clicked()
            {
                self.save_screen(cx);
            }
        });
        ui.add_space(12.0);
        if self.screen_changed {
            if let Some(frame) = &self.screen {
                let image = ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgba,
                );
                self.texture = Some(ui.ctx().load_texture(
                    "usb-screen",
                    image,
                    TextureOptions::NEAREST,
                ));
            }
            self.screen_changed = false;
        }
        if let Some(texture) = &self.texture {
            let w = ui.available_width().min(480.0);
            ui.image((texture.id(), egui::vec2(w, w * 0.75)));
        } else {
            ui.label("Capture the calculator display to share a graph, worked example or result.");
            ui.add_space(180.0);
        }
        ui.label(
            "Captures use the calculator’s USB screen function. Exam mode may restrict access.",
        );
    }

    fn save_screen(&mut self, cx: &FeatureCtx<'_>) {
        if let Some(path) =
            cx.platform
                .pick_save("Save calculator screen", "calculator.png", PNG_FILTER)
            && let Some(frame) = &self.screen
        {
            let result = crate::capture::encode_still(frame)
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    crate::store::write_atomic(&path, &bytes).map_err(|e| e.to_string())
                });
            match result {
                Ok(()) => self.status = format!("Saved {}.", path.display()),
                Err(e) => self.fail(e),
            }
        }
    }

    fn calculator_ui(&mut self, ui: &mut egui::Ui, cx: &mut FeatureCtx<'_>, enabled: bool) {
        ui.strong("Calculator information");
        if ui
            .add_enabled(enabled, Button::new("Read information"))
            .clicked()
        {
            self.start(UsbOperation::DeviceInfo, cx);
        }
        if !self.info.is_empty() {
            ScrollArea::vertical()
                .id_salt("usb-info")
                .max_height(170.0)
                .show(ui, |ui| {
                    ui.label(&self.info);
                });
        } else {
            ui.label("Read the OS version, device details and memory information.");
        }
        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.strong("Exam mode");
        ui.label("Exit Press-to-Test on this emulated calculator.");
        if ui
            .add_enabled(enabled, Button::new("Exit exam mode"))
            .clicked()
        {
            self.start(UsbOperation::ExitExam, cx);
        }
        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.strong("Operating system");
        ui.label("Choose a TI-84 Evo .84b2 bundle to send through USB.");
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    enabled && self.update_job.is_none(),
                    Button::new("Check latest OS"),
                )
                .clicked()
            {
                let platform = cx.platform.clone();
                self.update_status = "Checking TI’s OS catalog…".into();
                match std::thread::Builder::new()
                    .name("evo-os-check".into())
                    .spawn(move || {
                        updates::check_latest(platform.as_ref()).map(UpdateReply::Latest)
                    }) {
                    Ok(job) => self.update_job = Some(job),
                    Err(e) => self.update_status = format!("Could not start OS check: {e}"),
                }
            }
            if let Some(version) = self.latest_version.clone()
                && ui
                    .add_enabled(
                        enabled && self.update_job.is_none(),
                        Button::new(format!("Download {version}")),
                    )
                    .clicked()
            {
                let platform = cx.platform.clone();
                self.update_status = format!("Downloading OS {version} from TI…");
                match std::thread::Builder::new()
                    .name("evo-os-download".into())
                    .spawn(move || {
                        updates::download(platform.as_ref(), &version)
                            .map(|(name, bytes)| UpdateReply::Bundle(name, bytes))
                    }) {
                    Ok(job) => self.update_job = Some(job),
                    Err(e) => self.update_status = format!("Could not start download: {e}"),
                }
            }
        });
        if !self.update_status.is_empty() {
            ui.label(&self.update_status);
        }
        if self.update_job.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        if ui
            .add_enabled(enabled, Button::new("Choose OS bundle…"))
            .clicked()
        {
            match Self::pick_file(cx, "Install calculator OS", OS_FILTER) {
                Ok(file) => self.pending_os = file,
                Err(e) => self.fail(e),
            }
        }
        if let Some(file) = &self.pending_os {
            ui.label(format!("{} · {}", file.name, byte_size(file.bytes.len())));
            ui.label("This updates the calculator OS. Save a flash backup from Files before installing a different version.");
            ui.horizontal(|ui| {
                if ui.add_enabled(enabled, Button::new("Install OS")).clicked() {
                    let bytes = self.pending_os.take().unwrap().bytes;
                    self.start(UsbOperation::InstallOs(bytes), cx);
                }
                if ui.button("Cancel install").clicked() {
                    self.pending_os = None;
                }
            });
        }
    }

    fn developer_ui(&mut self, ui: &mut egui::Ui, cx: &mut FeatureCtx<'_>, enabled: bool) {
        ui.label("Inspect the same USB resources used by calculator transfers.");
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(enabled, Button::new("Save diagnostics…"))
                .clicked()
                && let Some(folder) = cx.platform.pick_folder("Save calculator diagnostics in…")
            {
                self.receive_folder = Some(folder);
                self.start(UsbOperation::Logs, cx);
            }
            if ui
                .add_enabled(!self.raw_response.is_empty(), Button::new("Save response…"))
                .clicked()
                && let Some(path) =
                    cx.platform
                        .pick_save("Save USB response", "usb-response.bin", &[])
                && let Err(e) = crate::store::write_atomic(&path, &self.raw_response)
            {
                self.fail(e);
            }
            if ui.button("Export session log…").clicked()
                && let Some(path) =
                    cx.platform
                        .pick_save("Export USB session log", "usb-session.txt", &[])
            {
                let text = self.history.iter().cloned().collect::<Vec<_>>().join("\n");
                if let Err(e) = crate::store::write_atomic(&path, text.as_bytes()) {
                    self.fail(e);
                }
            }
        });
        ui.add_space(8.0);
        ui.label("Resource URI");
        ui.add(
            egui::TextEdit::singleline(&mut self.uri)
                .font(egui::TextStyle::Monospace)
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    enabled && !self.uri.trim().is_empty(),
                    Button::new("Read resource"),
                )
                .clicked()
            {
                self.start(UsbOperation::GetResource(self.uri.trim().into()), cx);
            }
            ui.checkbox(&mut self.allow_write, "Enable resource writes");
        });
        if self.allow_write {
            ui.label("Writes can change or remove calculator data. Check the URI before sending.");
            if ui
                .add_enabled(enabled, Button::new("Send bytes from file…"))
                .clicked()
            {
                match Self::pick_file(cx, "Send raw resource bytes", &[]) {
                    Ok(Some(file)) => self.start(
                        UsbOperation::PutResource {
                            uri: self.uri.trim().into(),
                            bytes: file.bytes,
                        },
                        cx,
                    ),
                    Ok(None) => {}
                    Err(e) => self.fail(e),
                }
            }
        }
        ui.collapsing("Debug message", |ui| {
            ui.text_edit_singleline(&mut self.debug_message);
            if ui
                .add_enabled(
                    enabled && !self.debug_message.is_empty(),
                    Button::new("Send debug message"),
                )
                .clicked()
            {
                self.start(UsbOperation::DebugMessage(self.debug_message.clone()), cx);
            }
        });
        ui.add_space(8.0);
        ui.strong("Response");
        ScrollArea::both()
            .id_salt("usb-response")
            .max_height(180.0)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.response)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .interactive(false),
                );
            });
        ui.collapsing("Session activity", |ui| {
            ui.label("Recent progress and packet summaries, sampled during transfer. Up to 200 entries are kept.");
            ScrollArea::vertical().id_salt("usb-history").max_height(160.0).stick_to_bottom(true).show(ui, |ui| {
                for line in &self.history { ui.monospace(line); }
            });
        });
    }
}

impl Feature for ConnectTools {
    fn on_action(&mut self, action: Action, cx: &mut FeatureCtx<'_>) -> bool {
        if action != Action::UsbTools {
            return false;
        }
        self.open = !self.open;
        if self.open && !self.loaded && cx.backend.usb_available() {
            self.start(UsbOperation::Refresh, cx);
        }
        true
    }

    fn frame(&mut self, cx: &mut FeatureCtx<'_>) {
        if self.update_job.as_ref().is_some_and(|j| j.is_finished()) {
            match self
                .update_job
                .take()
                .unwrap()
                .join()
                .unwrap_or_else(|_| Err("OS download worker stopped unexpectedly".into()))
            {
                Ok(UpdateReply::Latest(version)) => {
                    self.update_status = format!("Latest official OS: {version}.");
                    self.latest_version = Some(version);
                }
                Ok(UpdateReply::Bundle(name, bytes)) => {
                    self.update_status =
                        "Download complete. Review the bundle below, then choose Install OS."
                            .into();
                    self.pending_os = Some(PendingFile {
                        name,
                        bytes,
                        rename: String::new(),
                        image_slot: 1,
                    });
                }
                Err(e) => {
                    self.update_status = format!(
                        "Could not fetch the OS: {e}. You can choose a local bundle instead."
                    )
                }
            }
        }
        let events: Vec<_> = self
            .receiver
            .as_ref()
            .map(|r| r.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            match event {
                UsbEvent::Progress(p) => {
                    if self
                        .progress
                        .as_ref()
                        .is_none_or(|old| old.phase != p.phase)
                    {
                        self.record(p.phase.clone());
                    }
                    if let Some(detail) = &p.detail {
                        self.record(detail.clone());
                    }
                    self.progress = Some(p);
                }
                UsbEvent::Finished(result) => self.completed(result, cx),
            }
        }
        if self.open
            && self.tab == Tab::Screen
            && self.auto_screen
            && !self.busy()
            && !cx.provisioning
            && cx.backend.usb_available()
            && self.last_screen.elapsed() >= Duration::from_millis(1500)
        {
            self.start(UsbOperation::Screen, cx);
        }
    }

    fn ui(&mut self, ctx: &egui::Context, cx: &mut FeatureCtx<'_>) {
        if !self.open {
            return;
        }
        if self.tab == Tab::Screen && self.auto_screen {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        let mut open = self.open;
        let enabled = !self.busy() && !cx.provisioning && cx.backend.usb_available();
        let max_width = (ctx.content_rect().width() - 24.0).max(280.0);
        egui::Window::new("USB tools").id(egui::Id::new("evo-usb-tools"))
            .open(&mut open).default_width(480.0_f32.min(max_width)).min_width(280.0).max_width(max_width)
            .default_height(520.0_f32.min(ctx.content_rect().height() - 140.0)).min_height(240.0)
            .default_pos(egui::pos2(60.0, 65.0)).resizable(true)
            .frame(egui::Frame::window(&ctx.global_style()).fill(HEADER_BG))
            .show(ctx, |ui| {
                ui.visuals_mut().override_text_color = Some(LABEL_DARK);
                ui.horizontal_wrapped(|ui| {
                    for (tab, label) in [(Tab::Files,"Files"), (Tab::Screen,"Screen"), (Tab::Calculator,"Calculator"), (Tab::Developer,"Developer")] {
                        ui.selectable_value(&mut self.tab, tab, label);
                    }
                });
                ui.separator();
                if !cx.backend.usb_available() && !self.busy() && !cx.provisioning {
                    ui.label("Open a calculator OS bundle to use USB transfers. The demonstration calculator has no USB device.");
                    ui.separator();
                }
                // Reserve feedback space: otherwise a growing preview consumes it and
                // clips the status/cancel controls outside the resizable window.
                let footer_height = if self.busy() { 105.0 } else {
                    ui.painter().layout(self.status.clone(), egui::FontId::proportional(ui.text_style_height(&egui::TextStyle::Body)), LABEL_DARK, ui.available_width()).size().y + 12.0
                };
                let content_height = (ui.available_height() - footer_height).max(80.0).min((ctx.content_rect().height() - 220.0).max(200.0));
                ScrollArea::vertical().id_salt("usb-workspace").max_height(content_height).show(ui, |ui| {
                    match self.tab {
                        Tab::Files => self.files_ui(ui, cx, enabled),
                        Tab::Screen => self.screen_ui(ui, cx, enabled),
                        Tab::Calculator => self.calculator_ui(ui, cx, enabled),
                        Tab::Developer => self.developer_ui(ui, cx, enabled),
                    }
                });
                ui.separator();
                if self.busy() {
                    if self.cancelled.load(Ordering::Relaxed) { ui.label("Cancelling transfer…"); }
                    if let Some(p) = &self.progress {
                        ui.label(&p.phase);
                        if let Some(total) = p.total.filter(|&n| n > 0) {
                            ui.add(egui::ProgressBar::new((p.transferred as f32 / total as f32).min(1.0)).show_percentage());
                        } else { ui.label(format!("{} transferred", byte_size(p.transferred))); }
                    } else { ui.label(&self.status); }
                    if ui.add_enabled(!self.cancelled.load(Ordering::Relaxed), Button::new("Cancel transfer")).clicked() {
                        self.cancelled.store(true, Ordering::Relaxed); self.status = "Cancelling transfer…".into();
                    }
                    ctx.request_repaint_after(Duration::from_millis(40));
                } else {
                    ui.label(RichText::new(&self.status).color(if self.error { crate::theme::TOAST_ERROR_BG } else { BODY_OUTLINE }));
                }
            });
        self.open = open;
    }

    fn active(&self, action: Action) -> bool {
        action == Action::UsbTools && self.open
    }
    fn next_deadline(&self) -> Option<u64> {
        None
    }
    fn blocks_input(&self) -> bool {
        self.busy()
    }
    fn on_exit(&mut self, _: &mut FeatureCtx<'_>) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

fn byte_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn file_type(extension: &str) -> &str {
    match extension.trim_start_matches('.') {
        "8xn2" => "Number",
        "8xl2" => "List",
        "8xm2" => "Matrix",
        "8xp2" => "Program",
        "py" => "Python",
        "8mp2" => "Python module",
        "8ca2" => "Image",
        "8ci2" => "Picture",
        "8xs2" => "String",
        "8xd2" => "Graph database",
        "8xy2" => "Equation",
        "8xv2" => "App variable",
        "8xg2" => "Group",
        "8ek2" => "App",
        "8xw2" => "Window",
        "8xz2" => "User zoom",
        "8xt2" => "Table setup",
        _ => extension,
    }
}

fn fixed_location(filename: &str) -> Option<bool> {
    match Path::new(filename)
        .extension()?
        .to_str()?
        .to_ascii_lowercase()
        .as_str()
    {
        "png" | "8ci2" | "8ca2" | "8xg2" | "8ek2" | "8mp2" => Some(true),
        "8xy2" | "8xw2" | "8xz2" | "8xt2" => Some(false),
        _ => None,
    }
}

fn preview_bytes(bytes: &[u8]) -> String {
    let truncated = &bytes[..bytes.len().min(16 * 1024)];
    let mut text = match std::str::from_utf8(truncated) {
        Ok(s)
            if !s
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) =>
        {
            s.to_owned()
        }
        _ => truncated
            .chunks(16)
            .enumerate()
            .map(|(i, row)| {
                format!(
                    "{:08X}  {}",
                    i * 16,
                    row.iter()
                        .map(|b| format!("{b:02X}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    if bytes.len() > truncated.len() {
        text.push_str("\n… Preview limited to 16 KiB. Save response for all bytes.");
    }
    text
}

fn safe_filename(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim_matches([' ', '.']);
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if name.is_empty() {
        return "calculator-file.bin".into();
    }
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
    {
        format!("_{name}")
    } else {
        name.to_owned()
    }
}

fn save_received(folder: &Path, files: &[ReceivedFile]) -> Result<(), String> {
    use std::io::Write;
    for file in files {
        let name = safe_filename(&file.name);
        let path = Path::new(&name);
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let extension = path
            .extension()
            .map(|s| format!(".{}", s.to_string_lossy()))
            .unwrap_or_default();
        let mut saved = false;
        for suffix in 0..10_000 {
            let destination = folder.join(if suffix == 0 {
                name.clone()
            } else {
                format!("{stem} ({suffix}){extension}")
            });
            match std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&destination)
            {
                Ok(mut output) => {
                    if let Err(e) = output
                        .write_all(&file.bytes)
                        .and_then(|()| output.sync_all())
                    {
                        drop(output);
                        let _ = std::fs::remove_file(&destination);
                        return Err(format!("Could not save {}: {e}", destination.display()));
                    }
                    saved = true;
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("Could not save {}: {e}", destination.display())),
            }
        }
        if !saved {
            return Err(format!(
                "Too many existing copies of {name}. Choose another folder."
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_names_cannot_escape_the_download_folder_or_name_windows_devices() {
        for name in [
            "../../outside.py",
            "C:\\outside.py",
            "\\\\server\\share",
            ".",
            "..",
            "CON",
            "LPT1.txt",
            "NUL.py",
        ] {
            let safe = safe_filename(name);
            assert!(!safe.is_empty());
            assert_eq!(Path::new(&safe).components().count(), 1);
            assert!(!matches!(
                safe.split('.').next().unwrap(),
                "CON" | "LPT1" | "NUL"
            ));
        }
    }

    #[test]
    fn received_files_never_overwrite_existing_files() {
        let folder = std::env::temp_dir().join(format!(
            "evo-receive-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&folder).unwrap();
        let file = ReceivedFile {
            name: "hello.py".into(),
            bytes: b"print(1)".to_vec(),
        };
        save_received(&folder, &[file.clone(), file]).unwrap();
        assert_eq!(std::fs::read(folder.join("hello.py")).unwrap(), b"print(1)");
        assert_eq!(
            std::fs::read(folder.join("hello (1).py")).unwrap(),
            b"print(1)"
        );
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn developer_preview_is_bounded_and_preserves_binary_data_as_hex() {
        assert!(preview_bytes(&[0, 255, 1]).contains("00 FF 01"));
        assert!(preview_bytes(&vec![b'a'; 20_000]).ends_with("Save response for all bytes."));
        assert_eq!(preview_bytes(b"{\"ok\":true}"), "{\"ok\":true}");
    }
}
