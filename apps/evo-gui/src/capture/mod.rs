//! Capture: Copy screen, animated PNG and MP4 recording.

mod apng;
mod mp4;
mod sampler;

pub use self::mp4::{Mp4Recorder, scale2x_rgb};
pub(crate) use apng::encode_still;
pub use apng::{APNG_MAX_FRAMES, ApngRecorder, CaptureError};
pub use sampler::{FrameSampler, LCD_FRAME_CYCLES};

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::AppOptions;
use crate::backend::Frame;
use crate::feature::{Feature, FeatureCtx};
use crate::toast::ToastKind;
use crate::toolbar::Action;

/// Toast after Copy screen succeeds.
pub const COPIED: &str = "Screen copied";
/// Why Copy screen fails when the machine shows no valid frame.
pub const NO_FRAME: &str = "no display frame to capture";
/// Toast when an animated PNG recording starts.
pub const RECORDING_APNG: &str = "Recording animated PNG…";
/// Toast when an MP4 recording starts.
pub const RECORDING_MP4: &str = "Recording MP4…";
/// Toast when a recording stops before any frame was taken.
pub const NOTHING_RECORDED: &str = "Nothing was recorded";
/// Toast when one kind of recording is asked for while the other runs.
pub const STOP_FIRST: &str = "Stop the current recording first";

/// What is being recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordKind {
    Apng,
    Mp4,
}

/// The encoder of the recording in progress.
enum Recorder {
    Apng(ApngRecorder),
    Mp4(Box<Mp4Recorder>),
}

impl Recorder {
    fn kind(&self) -> RecordKind {
        match self {
            Recorder::Apng(_) => RecordKind::Apng,
            Recorder::Mp4(_) => RecordKind::Mp4,
        }
    }

    fn push(&mut self, frame: &Frame, ticks: u32) -> Result<(), CaptureError> {
        match self {
            Recorder::Apng(r) => r.push(frame, ticks),
            Recorder::Mp4(r) => r.push(frame, ticks),
        }
    }

    fn finish(self) -> Result<Option<PathBuf>, CaptureError> {
        match self {
            Recorder::Apng(r) => r.finish(),
            Recorder::Mp4(r) => r.finish(),
        }
    }
}

/// A recording in progress: its encoder and the clock that says how long each frame showed.
struct Recording {
    recorder: Recorder,
    sampler: FrameSampler,
}

/// The capture feature: Copy screen and screen recording.
pub struct Capture {
    rec: Option<Recording>,
}

impl Capture {
    pub fn new(_opts: &AppOptions) -> Self {
        Self { rec: None }
    }

    /// The recording in progress, if any.
    pub fn recording(&self) -> Option<RecordKind> {
        self.rec.as_ref().map(|r| r.recorder.kind())
    }

    /// Puts the current frame on the clipboard and says how it went.
    fn copy_screen(cx: &mut FeatureCtx<'_>) {
        let result = match cx.backend.framebuffer().filter(|f| f.is_valid()) {
            Some(frame) => cx.platform.copy_image(frame),
            None => Err(NO_FRAME.to_owned()),
        };
        match result {
            Ok(()) => cx.toasts.push(COPIED, ToastKind::Info),
            Err(e) => cx
                .toasts
                .push(format!("Copy failed: {e}"), ToastKind::Error),
        }
    }

    /// Starts recording `kind`, stops it when it is the one running, or refuses while the
    /// other kind runs.
    fn toggle(&mut self, kind: RecordKind, cx: &mut FeatureCtx<'_>) {
        match self.recording() {
            Some(k) if k == kind => self.stop(cx),
            Some(_) => cx.toasts.push(STOP_FIRST, ToastKind::Error),
            None => self.start(kind, cx),
        }
    }

    /// Creates `{data_dir}/recordings/` and starts a recording there; on failure nothing starts.
    fn start(&mut self, kind: RecordKind, cx: &mut FeatureCtx<'_>) {
        let path = match recording_path(cx.data_dir, kind) {
            Ok(p) => p,
            Err(e) => {
                cx.toasts
                    .push(format!("Recording failed: {e}"), ToastKind::Error);
                return;
            }
        };
        let (recorder, toast) = match kind {
            RecordKind::Apng => (Recorder::Apng(ApngRecorder::new(path)), RECORDING_APNG),
            RecordKind::Mp4 => (
                Recorder::Mp4(Box::new(Mp4Recorder::new(path))),
                RECORDING_MP4,
            ),
        };
        self.rec = Some(Recording {
            recorder,
            sampler: FrameSampler::new(cx.backend.cycles()),
        });
        cx.toasts.push(toast, ToastKind::Info);
    }

    /// Ends the recording (if any), writes it and says how it went.
    fn stop(&mut self, cx: &mut FeatureCtx<'_>) {
        if let Some(rec) = self.rec.take() {
            match rec.recorder.finish() {
                Ok(Some(p)) => cx.toasts.push(
                    format!("Recording saved to {}", p.display()),
                    ToastKind::Info,
                ),
                Ok(None) => cx.toasts.push(NOTHING_RECORDED, ToastKind::Info),
                Err(e) => cx
                    .toasts
                    .push(format!("Recording stopped: {e}"), ToastKind::Error),
            }
        }
    }

    /// Ends the recording after `push` refused a frame: the frames so far are written. When
    /// nothing can be written (no frame yet, or the error broke the file) the one toast says
    /// why it stopped.
    fn stop_after(&mut self, err: CaptureError, cx: &mut FeatureCtx<'_>) {
        let first = format!("Recording stopped: {err}");
        cx.toasts.push(first.clone(), ToastKind::Error);
        let Some(rec) = self.rec.take() else {
            return;
        };
        match rec.recorder.finish() {
            Ok(Some(p)) => cx.toasts.push(
                format!("Recording saved to {}", p.display()),
                ToastKind::Info,
            ),
            Ok(None) => {}
            Err(e) => {
                let again = format!("Recording stopped: {e}");
                if again != first {
                    cx.toasts.push(again, ToastKind::Error);
                }
            }
        }
    }
}

/// `{data_dir}/recordings/evo-{unix_ms}.{png|mp4}`, creating the directory.
fn recording_path(data_dir: &Path, kind: RecordKind) -> std::io::Result<PathBuf> {
    let dir = data_dir.join("recordings");
    std::fs::create_dir_all(&dir)?;
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let ext = match kind {
        RecordKind::Apng => "png",
        RecordKind::Mp4 => "mp4",
    };
    Ok(dir.join(format!("evo-{unix_ms}.{ext}")))
}

impl Feature for Capture {
    fn on_action(&mut self, action: Action, cx: &mut FeatureCtx<'_>) -> bool {
        match action {
            Action::CopyScreen => {
                Self::copy_screen(cx);
                true
            }
            Action::RecordApng => {
                self.toggle(RecordKind::Apng, cx);
                true
            }
            Action::RecordVideo => {
                self.toggle(RecordKind::Mp4, cx);
                true
            }
            _ => false,
        }
    }

    /// Pushes the screen when at least one 1/60 s tick passed since the last push. Paused
    /// (no ticks) or without a valid frame nothing is pushed; the clock still advances.
    fn frame(&mut self, cx: &mut FeatureCtx<'_>) {
        let Some(rec) = self.rec.as_mut() else {
            return;
        };
        let ticks = rec.sampler.ticks(cx.backend.cycles());
        if ticks == 0 {
            return;
        }
        let Some(frame) = cx.backend.framebuffer().filter(|f| f.is_valid()) else {
            return;
        };
        if let Err(e) = rec.recorder.push(frame, ticks) {
            self.stop_after(e, cx);
        }
    }

    fn ui(&mut self, _ctx: &egui::Context, _cx: &mut FeatureCtx<'_>) {}

    fn active(&self, action: Action) -> bool {
        match action {
            Action::RecordApng => self.recording() == Some(RecordKind::Apng),
            Action::RecordVideo => self.recording() == Some(RecordKind::Mp4),
            _ => false,
        }
    }

    fn next_deadline(&self) -> Option<u64> {
        None
    }

    fn blocks_input(&self) -> bool {
        false
    }

    /// Writes out a recording in progress.
    fn on_exit(&mut self, cx: &mut FeatureCtx<'_>) {
        self.stop(cx);
    }
}
