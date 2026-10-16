//! Emulator instances: each has its own data directory.
//!
//! Stub written by Lane 0; Lane B replaces it (ids, `instance.lock`, rename).

use std::path::{Path, PathBuf};

/// A prepared instance. Lane B's version holds the instance lock until it drops.
#[derive(Debug)]
pub struct InstanceLock {
    /// The instance's data directory.
    pub dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum InstanceError {
    #[error("{0}")]
    Invalid(String),
    #[error("instance {0} is already running")]
    InUse(String),
    #[error("{0}")]
    Io(String),
}

/// Prepares the data directory of instance `id` under `base`: `base` itself for the default
/// instance (`None`), else `base/instances/<id>`, created if missing. The stub ignores
/// `rename_from` and takes no lock.
pub fn prepare(
    base: &Path,
    id: Option<&str>,
    _rename_from: Option<&str>,
) -> Result<InstanceLock, InstanceError> {
    let dir = match id {
        None => base.to_path_buf(),
        Some(id) => base.join("instances").join(id),
    };
    std::fs::create_dir_all(&dir)
        .map_err(|e| InstanceError::Io(format!("{}: {e}", dir.display())))?;
    Ok(InstanceLock { dir })
}
