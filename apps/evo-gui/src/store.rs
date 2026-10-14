//! State slots and screenshots on disk.
//!
//! A slot file is
//! 1. [`SLOT_MAGIC`];
//! 2. [`SLOT_VERSION`] (u16 LE);
//! 3. the length of the backend's `state_tag()` (u16 LE), then the tag bytes;
//! 4. the backend's `save_state()` payload.
//!
//! A slot holds RAM and device state only (spec §6.5). It is valid only with
//! the flash contents it was saved against; loading it after the guest has
//! rewritten flash is not detected.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use crate::backend::{BackendError, EmuBackend, Frame};

/// Number of state slots, numbered `1..=SLOTS`.
pub const SLOTS: u8 = 4;
/// First bytes of a slot file (distinct from the core's `EVOSTATE`).
pub const SLOT_MAGIC: &[u8; 8] = b"EVOGUIST";
/// The slot format version this build writes and reads.
pub const SLOT_VERSION: u16 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("slot {0} is empty")]
    Empty(u8),
    #[error("{0}: not an emulator state file")]
    NotAState(PathBuf),
    #[error("state is for `{found}`, this is `{expected}`")]
    WrongBackend { expected: String, found: String },
    #[error("state format version {0} is not supported")]
    Version(u16),
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("no display frame to capture")]
    NoFrame,
    #[error("PNG encoding failed: {0}")]
    Png(String),
}

/// State slots and screenshots under one data directory.
#[derive(Clone, Debug)]
pub struct Store {
    data_dir: PathBuf,
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Splits `n` bytes off the front of `bytes`.
fn take<'a>(bytes: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    if bytes.len() < n {
        return None;
    }
    let (head, rest) = bytes.split_at(n);
    *bytes = rest;
    Some(head)
}

fn take_u16(bytes: &mut &[u8]) -> Option<u16> {
    take(bytes, 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

impl Store {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    /// `{data_dir}/states/slot{n}.state`.
    pub fn slot_path(&self, n: u8) -> PathBuf {
        self.data_dir.join("states").join(format!("slot{n}.state"))
    }

    /// True when slot `n` has a file (it may still fail to load).
    pub fn has_slot(&self, n: u8) -> bool {
        self.slot_path(n).is_file()
    }

    /// Saves `b` into slot `n`, through a temporary file and a rename so a
    /// crash never leaves a half-written slot.
    pub fn save_slot(&self, n: u8, b: &dyn EmuBackend) -> Result<PathBuf, StoreError> {
        let payload = b.save_state()?;
        let tag = b.state_tag();
        let tag_len = u16::try_from(tag.len())
            .map_err(|_| BackendError::BadState(format!("state tag is {} bytes", tag.len())))?;
        let mut bytes = Vec::with_capacity(12 + tag.len() + payload.len());
        bytes.extend_from_slice(SLOT_MAGIC);
        bytes.extend_from_slice(&SLOT_VERSION.to_le_bytes());
        bytes.extend_from_slice(&tag_len.to_le_bytes());
        bytes.extend_from_slice(tag.as_bytes());
        bytes.extend_from_slice(&payload);

        let path = self.slot_path(n);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io_err(dir))?;
        }
        let tmp = path.with_extension("state.tmp");
        std::fs::write(&tmp, &bytes).map_err(io_err(&tmp))?;
        std::fs::rename(&tmp, &path).map_err(io_err(&path))?;
        Ok(path)
    }

    /// Loads slot `n` into `b`. Magic, version and tag are checked before the
    /// backend sees the payload; on any `Err` the machine is unchanged.
    pub fn load_slot(&self, n: u8, b: &mut dyn EmuBackend) -> Result<(), StoreError> {
        let path = self.slot_path(n);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::Empty(n));
            }
            Err(e) => return Err(io_err(&path)(e)),
        };
        let not_a_state = || StoreError::NotAState(path.clone());
        let mut rest = bytes.as_slice();
        if take(&mut rest, SLOT_MAGIC.len()) != Some(SLOT_MAGIC.as_slice()) {
            return Err(not_a_state());
        }
        let version = take_u16(&mut rest).ok_or_else(not_a_state)?;
        if version != SLOT_VERSION {
            return Err(StoreError::Version(version));
        }
        let tag_len = take_u16(&mut rest).ok_or_else(not_a_state)?;
        let found = take(&mut rest, usize::from(tag_len)).ok_or_else(not_a_state)?;
        let expected = b.state_tag();
        if found != expected.as_bytes() {
            return Err(StoreError::WrongBackend {
                expected,
                found: String::from_utf8_lossy(found).into_owned(),
            });
        }
        b.load_state(rest)?;
        Ok(())
    }

    /// Writes `frame` at native resolution to
    /// `{data_dir}/screenshots/evo-{unix_ms}.png`.
    pub fn screenshot(&self, frame: &Frame, unix_ms: u128) -> Result<PathBuf, StoreError> {
        if !frame.is_valid() {
            return Err(StoreError::NoFrame);
        }
        let img = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.clone())
            .ok_or(StoreError::NoFrame)?;
        let mut png = Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png)
            .map_err(|e| StoreError::Png(e.to_string()))?;
        let dir = self.data_dir.join("screenshots");
        std::fs::create_dir_all(&dir).map_err(io_err(&dir))?;
        let path = dir.join(format!("evo-{unix_ms}.png"));
        std::fs::write(&path, png.into_inner()).map_err(io_err(&path))?;
        Ok(path)
    }
}
