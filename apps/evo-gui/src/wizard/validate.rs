//! Checks a file the user picked before the wizard opens it as an OS bundle.

use std::io::Read;
use std::path::Path;

use crate::backend::machine::{BundleInfo, inspect_bundle};

/// `bundle_target_prodid` of the TI-84 Evo OS bundles.
pub const EVO_PRODID: u32 = 23;
/// The OS version provisioning is known to work with (it is tied to its BSP 7.1.0.4413).
pub const SUPPORTED_OS: &str = "7.1.0.4421";
/// Largest file the wizard reads as a bundle.
pub const BUNDLE_MAX_BYTES: u64 = 256 << 20;

/// A file that passed [`check_bundle`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub info: BundleInfo,
    /// Shown inline; the bundle may still be used.
    pub warning: Option<String>,
}

/// The file name of `path` for messages (the whole path when it has none).
pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Checks that `path` is a TI-84 Evo OS bundle. The size is checked before anything is read,
/// and the read itself never goes past [`BUNDLE_MAX_BYTES`] + 1 bytes (the file may grow in
/// between). The error is the text the wizard shows.
pub fn check_bundle(path: &Path) -> Result<Checked, String> {
    let name = display_name(path);
    let cannot_read = |e: &dyn std::fmt::Display| format!("Cannot read {name}: {e}");
    let meta = std::fs::metadata(path).map_err(|e| cannot_read(&e))?;
    if !meta.is_file() {
        return Err(cannot_read(&"not a file"));
    }
    if meta.len() > BUNDLE_MAX_BYTES {
        return Err(too_large());
    }
    let file = std::fs::File::open(path).map_err(|e| cannot_read(&e))?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(BUNDLE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| cannot_read(&e))?;
    if bytes.len() as u64 > BUNDLE_MAX_BYTES {
        return Err(too_large());
    }
    let info = inspect_bundle(&bytes).map_err(|e| format!("Not an OS bundle: {e}"))?;
    if info.prodid != EVO_PRODID {
        return Err(format!(
            "This is not a TI-84 Evo OS bundle (product id {})",
            info.prodid
        ));
    }
    let warning = (info.build_version != SUPPORTED_OS).then(|| {
        format!(
            "OS {}: provisioning is tied to OS {SUPPORTED_OS} and may fail with this version",
            info.build_version
        )
    });
    Ok(Checked { info, warning })
}

fn too_large() -> String {
    "File is too large to be an OS bundle".to_owned()
}
