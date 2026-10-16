//! GUI-owned USB commands and results. No emulation implementation leaks into the UI.

use std::sync::{Arc, atomic::AtomicBool, mpsc::Sender};

use crate::backend::Frame;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalculatorFile {
    pub name: String,
    pub token_name: String,
    pub kind: u32,
    pub size: usize,
    pub archived: bool,
    pub extension: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CsvTarget {
    #[default]
    Lists,
    Matrix,
}

#[derive(Clone, Debug)]
pub struct SendOptions {
    pub name: String,
    pub archived: bool,
    pub overwrite: bool,
    pub image_slot: u8,
    pub csv_target: CsvTarget,
}

impl Default for SendOptions {
    fn default() -> Self {
        Self {
            name: String::new(),
            archived: false,
            overwrite: false,
            image_slot: 1,
            csv_target: CsvTarget::Lists,
        }
    }
}

#[derive(Clone, Debug)]
pub struct UploadFile {
    pub filename: String,
    pub bytes: Vec<u8>,
    pub options: SendOptions,
}

#[derive(Clone, Debug)]
pub enum UsbOperation {
    Refresh,
    Receive(Vec<CalculatorFile>),
    Send {
        filename: String,
        bytes: Vec<u8>,
        options: SendOptions,
    },
    SendMany(Vec<UploadFile>),
    Delete(Vec<CalculatorFile>),
    Screen,
    DeviceInfo,
    ExitExam,
    InstallOs(Vec<u8>),
    Logs,
    DebugMessage(String),
    GetResource(String),
    PutResource {
        uri: String,
        bytes: Vec<u8>,
    },
}

impl UsbOperation {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Refresh => "Reading calculator files",
            Self::Receive(_) => "Receiving files",
            Self::Send { .. } => "Sending file",
            Self::SendMany(_) => "Sending files",
            Self::Delete(_) => "Deleting files",
            Self::Screen => "Capturing calculator screen",
            Self::DeviceInfo => "Reading calculator information",
            Self::ExitExam => "Exiting exam mode",
            Self::InstallOs(_) => "Installing operating system",
            Self::Logs => "Reading calculator diagnostics",
            Self::DebugMessage(_) => "Sending debug message",
            Self::GetResource(_) => "Reading USB resource",
            Self::PutResource { .. } => "Writing USB resource",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReceivedFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub enum UsbResult {
    Batch {
        files: Vec<ReceivedFile>,
        errors: Vec<String>,
        message: String,
    },
    Directory {
        files: Vec<CalculatorFile>,
        info: String,
    },
    Files(Vec<ReceivedFile>),
    Screen(Frame),
    Text(String),
    Bytes(Vec<u8>),
    Done(String),
}

#[derive(Clone, Debug)]
pub struct UsbProgress {
    pub phase: String,
    pub transferred: usize,
    pub total: Option<usize>,
    pub detail: Option<String>,
}

#[derive(Debug)]
pub enum UsbEvent {
    Progress(UsbProgress),
    Finished(Result<UsbResult, String>),
}

#[derive(Debug)]
pub struct UsbTask {
    pub operation: UsbOperation,
    pub events: Sender<UsbEvent>,
    pub cancelled: Arc<AtomicBool>,
}
