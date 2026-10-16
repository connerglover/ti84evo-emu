//! Animated PNG recording: each distinct screen becomes one APNG frame shown for as many
//! 1/60 s ticks as it stayed on screen.

use std::fs::File;
use std::io::{BufWriter, Cursor};
use std::path::{Path, PathBuf};

use crate::backend::Frame;

/// The most distinct frames one recording keeps: 30 s of a screen that changes every frame.
pub const APNG_MAX_FRAMES: usize = 1800;

/// Why a recording could not take a frame or be written.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Encode(String),
    #[error("recording is full")]
    Full,
}

impl From<std::io::Error> for CaptureError {
    fn from(e: std::io::Error) -> Self {
        CaptureError::Io(e.to_string())
    }
}

impl From<png::EncodingError> for CaptureError {
    fn from(e: png::EncodingError) -> Self {
        match e {
            png::EncodingError::IoError(e) => CaptureError::Io(e.to_string()),
            e => CaptureError::Encode(e.to_string()),
        }
    }
}

/// One distinct screen: a standalone PNG in memory and how long it was shown.
struct Kept {
    png: Vec<u8>,
    ticks: u64,
}

/// Collects distinct frames and writes them as an APNG at [`finish`](Self::finish).
///
/// Each new frame is compressed at once, so memory holds compressed PNGs plus the last
/// frame's raw RGBA (for the identical-frame check), not every raw frame.
pub struct ApngRecorder {
    path: PathBuf,
    cap: usize,
    size: Option<(u32, u32)>,
    last: Vec<u8>,
    frames: Vec<Kept>,
}

impl ApngRecorder {
    /// A recorder that writes `path` and keeps at most [`APNG_MAX_FRAMES`] distinct frames.
    pub fn new(path: PathBuf) -> Self {
        Self::with_cap(path, APNG_MAX_FRAMES)
    }

    /// A recorder that keeps at most `cap` distinct frames.
    pub fn with_cap(path: PathBuf, cap: usize) -> Self {
        Self {
            path,
            cap,
            size: None,
            last: Vec::new(),
            frames: Vec::new(),
        }
    }

    /// Adds `frame`, shown for `ticks` 1/60 s ticks. A frame identical to the last one adds
    /// its ticks to that one. A new frame past the cap is refused with [`CaptureError::Full`];
    /// an invalid frame or one of another size than the first with [`CaptureError::Encode`].
    /// A refused frame leaves the recording as it was.
    pub fn push(&mut self, frame: &Frame, ticks: u32) -> Result<(), CaptureError> {
        if !frame.is_valid() {
            return Err(CaptureError::Encode("invalid frame".to_owned()));
        }
        let size = (frame.width, frame.height);
        if let Some(first) = self.size
            && first != size
        {
            return Err(CaptureError::Encode(format!(
                "frame size changed from {}×{} to {}×{}",
                first.0, first.1, size.0, size.1
            )));
        }
        if let Some(kept) = self.frames.last_mut()
            && self.last == frame.rgba
        {
            kept.ticks = kept.ticks.saturating_add(u64::from(ticks));
            return Ok(());
        }
        if self.frames.len() >= self.cap {
            return Err(CaptureError::Full);
        }
        let png = encode_still(frame)?;
        self.size = Some(size);
        self.last.clear();
        self.last.extend_from_slice(&frame.rgba);
        self.frames.push(Kept {
            png,
            ticks: u64::from(ticks),
        });
        Ok(())
    }

    /// Writes the APNG and returns its path, or `None` (and no file) when no frame was
    /// pushed. On an error the partial file is removed.
    pub fn finish(self) -> Result<Option<PathBuf>, CaptureError> {
        let Some((width, height)) = self.size else {
            return Ok(None);
        };
        let result = write_apng(&self.path, width, height, &self.frames);
        match result {
            Ok(()) => Ok(Some(self.path)),
            Err(e) => {
                let _ = std::fs::remove_file(&self.path);
                Err(e)
            }
        }
    }
}

/// The APNG delays (in 1/60 s) of a frame shown `ticks` ticks: one chunk per `u16::MAX`
/// ticks, and at least one chunk.
fn delays(ticks: u64) -> impl Iterator<Item = u16> {
    let max = u64::from(u16::MAX);
    let full = ticks / max;
    let rest = ticks % max;
    let rest = (rest > 0 || full == 0).then_some(rest);
    std::iter::repeat_n(u16::MAX, full as usize)
        .chain(rest.map(|r| u16::try_from(r).unwrap_or(u16::MAX)))
}

/// `frame` as a standalone RGBA 8-bit PNG.
fn encode_still(frame: &Frame) -> Result<Vec<u8>, CaptureError> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, frame.width, frame.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header()?;
    writer.write_image_data(&frame.rgba)?;
    writer.finish()?;
    Ok(out)
}

/// Decodes a PNG made by [`encode_still`] back to RGBA.
fn decode_still(bytes: &[u8]) -> Result<Vec<u8>, CaptureError> {
    let encode = |e: png::DecodingError| CaptureError::Encode(e.to_string());
    let mut reader = png::Decoder::new(Cursor::new(bytes))
        .read_info()
        .map_err(encode)?;
    let len = reader
        .output_buffer_size()
        .ok_or_else(|| CaptureError::Encode("frame too large".to_owned()))?;
    let mut buf = vec![0; len];
    let info = reader.next_frame(&mut buf).map_err(encode)?;
    buf.truncate(info.buffer_size());
    Ok(buf)
}

/// Writes `frames` to `path` as one APNG of `width`×`height` that loops forever.
fn write_apng(path: &Path, width: u32, height: u32, frames: &[Kept]) -> Result<(), CaptureError> {
    let count: usize = frames.iter().map(|k| delays(k.ticks).count()).sum();
    let count = u32::try_from(count).map_err(|_| CaptureError::Encode("too many frames".into()))?;
    let file = BufWriter::new(File::create(path)?);
    let mut enc = png::Encoder::new(file, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_animated(count, 0)?;
    let mut writer = enc.write_header()?;
    for kept in frames {
        let rgba = decode_still(&kept.png)?;
        for delay in delays(kept.ticks) {
            writer.set_frame_delay(delay, 60)?;
            writer.write_image_data(&rgba)?;
        }
    }
    // Writes IEND and flushes the buffered file, reporting a failed write.
    writer.finish()?;
    Ok(())
}
