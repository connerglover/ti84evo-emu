//! Test helpers shared by the integration tests.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// A directory under the system temp dir, unique to this process and call, removed with
/// everything in it when the guard drops (also when the test panics).
pub struct TempDir(PathBuf);

impl TempDir {
    /// `{temp}/{prefix}-{pid}-{n}`, emptied; created when `create` is true.
    pub fn new(prefix: &str, create: bool) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        if create {
            std::fs::create_dir_all(&path).unwrap();
        }
        Self(path)
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
