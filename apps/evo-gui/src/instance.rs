//! Emulator instances: each has its own data directory and lock.
//!
//! The default instance lives in `base` itself; instance `id` lives in `base/instances/<id>`.
//! A running instance holds an exclusive lock on [`LOCK_FILE`] in its directory, so a second
//! process for the same instance is refused. "Change ID" starts a new process with
//! `--instance <new> --rename-from <old>`: it waits for the old process to release its lock,
//! then renames the directory.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

/// The lock file in every instance directory.
pub const LOCK_FILE: &str = "instance.lock";

const INVALID_ID: &str = "IDs use 1–32 letters, digits, - or _";

/// A prepared instance. The lock is held until this value drops.
#[derive(Debug)]
pub struct InstanceLock {
    /// The instance's data directory.
    pub dir: PathBuf,
    /// Holds the exclusive lock on `dir/LOCK_FILE`; never read, only kept open.
    #[allow(dead_code)]
    file: File,
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

/// Checks an instance id: 1–32 ASCII letters, digits, `-` or `_`.
pub fn validate_id(id: &str) -> Result<(), InstanceError> {
    let ok = (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(InstanceError::Invalid(INVALID_ID.to_owned()))
    }
}

/// The data directory of instance `id`: `base` for the default instance (`None`), else
/// `base/instances/<id>`.
pub fn instance_dir(base: &Path, id: Option<&str>) -> PathBuf {
    match id {
        None => base.to_path_buf(),
        Some(id) => base.join("instances").join(id),
    }
}

/// The first of `instance-2`, `instance-3`, … that has no directory (or other entry) yet.
pub fn next_free_id(base: &Path) -> String {
    (2u32..)
        .map(|n| format!("instance-{n}"))
        .find(|id| !instance_dir(base, Some(id)).exists())
        .expect("some instance id is free")
}

fn io_err(path: &Path, e: std::io::Error) -> InstanceError {
    InstanceError::Io(format!("{}: {e}", path.display()))
}

fn open_lock_file(path: &Path) -> Result<File, InstanceError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| io_err(path, e))
}

/// Prepares the data directory of instance `id` under `base` (see [`instance_dir`]),
/// created if missing, and locks it.
///
/// With `rename_from = Some(old)`, `id` must be `Some(new)` with no `instances/new` yet:
/// this first waits until the process of `old` has released its lock (it holds it until it
/// exits), then renames `instances/old` to `instances/new`.
///
/// Errors: a bad id or rename request ⇒ [`InstanceError::Invalid`]; the instance already
/// locked by another process ⇒ [`InstanceError::InUse`] (`id`, or `"default"`).
pub fn prepare(
    base: &Path,
    id: Option<&str>,
    rename_from: Option<&str>,
) -> Result<InstanceLock, InstanceError> {
    if let Some(id) = id {
        validate_id(id)?;
    }
    if let Some(old) = rename_from {
        validate_id(old)?;
        let Some(new) = id else {
            return Err(InstanceError::Invalid(
                "--rename-from needs --instance with the new ID".to_owned(),
            ));
        };
        rename(base, old, new)?;
    }
    let dir = instance_dir(base, id);
    std::fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
    let path = dir.join(LOCK_FILE);
    let file = open_lock_file(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(InstanceLock { dir, file }),
        Err(TryLockError::WouldBlock) => {
            Err(InstanceError::InUse(id.unwrap_or("default").to_owned()))
        }
        Err(TryLockError::Error(e)) => Err(io_err(&path, e)),
    }
}

/// Renames `instances/old` to `instances/new` once the old process has released its lock.
fn rename(base: &Path, old: &str, new: &str) -> Result<(), InstanceError> {
    if old == new {
        return Err(InstanceError::Invalid(format!(
            "instance {new} already has this ID"
        )));
    }
    let from = instance_dir(base, Some(old));
    let to = instance_dir(base, Some(new));
    if !from.is_dir() {
        return Err(InstanceError::Invalid(format!(
            "instance {old} does not exist"
        )));
    }
    if to.exists() {
        return Err(InstanceError::Invalid(format!(
            "instance {new} already exists"
        )));
    }
    {
        let path = from.join(LOCK_FILE);
        let file = open_lock_file(&path)?;
        // Blocks until the old process exits (or otherwise drops its lock).
        file.lock().map_err(|e| io_err(&path, e))?;
        // The handle closes here: Windows cannot rename a directory with an open file in it.
    }
    retry_rename(
        || {
            // The destination can appear while waiting for the old process or retrying.
            if to.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("instance {new} already exists"),
                ));
            }
            std::fs::rename(&from, &to)
        },
        std::thread::sleep,
    )
    .map_err(|e| io_err(&from, e))
}

/// Windows indexers and antivirus can briefly hold a handle after the app exits.
/// Retry only sharing/lock violations and permission errors, with a bounded backoff.
fn retry_rename(
    mut rename: impl FnMut() -> std::io::Result<()>,
    mut sleep: impl FnMut(std::time::Duration),
) -> std::io::Result<()> {
    for attempt in 0..6 {
        match rename() {
            Err(e)
                if attempt < 5
                    && (e.kind() == std::io::ErrorKind::PermissionDenied
                        || (cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33)))) =>
            {
                sleep(std::time::Duration::from_millis(50 * (attempt + 1)));
            }
            result => return result,
        }
    }
    unreachable!("the last attempt always returns")
}

#[cfg(test)]
mod tests {
    use super::retry_rename;
    use std::io::{Error, ErrorKind};
    use std::time::Duration;

    #[test]
    fn transient_rename_failure_retries_then_succeeds() {
        let mut calls = 0;
        let mut delays = Vec::new();
        retry_rename(
            || {
                calls += 1;
                if calls < 3 {
                    Err(Error::from(ErrorKind::PermissionDenied))
                } else {
                    Ok(())
                }
            },
            |d| delays.push(d),
        )
        .unwrap();
        assert_eq!(calls, 3);
        assert_eq!(
            delays,
            [Duration::from_millis(50), Duration::from_millis(100)]
        );
    }

    #[test]
    fn rename_retry_is_bounded_and_permanent_errors_return_immediately() {
        for (kind, expected) in [
            (ErrorKind::PermissionDenied, 6),
            (ErrorKind::NotFound, 1),
            (ErrorKind::AlreadyExists, 1),
        ] {
            let mut calls = 0;
            let mut sleeps = 0;
            let error = retry_rename(
                || {
                    calls += 1;
                    Err(Error::from(kind))
                },
                |_| sleeps += 1,
            )
            .unwrap_err();
            assert_eq!(error.kind(), kind);
            assert_eq!(calls, expected);
            assert_eq!(sleeps, expected - 1);
        }
    }
}
