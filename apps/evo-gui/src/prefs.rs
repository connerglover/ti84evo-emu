//! User preferences kept between runs in eframe's storage.

use serde::{Deserialize, Serialize};

use crate::input::Keymap;
use crate::lcd::ScaleFilter;

/// The eframe storage key the preferences are saved under.
pub const PREFS_KEY: &str = "evo-gui-prefs";

/// What the GUI remembers between runs. A field missing from the stored
/// value takes its default, so older or partial preferences still load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// PC-key bindings.
    pub keymap: Keymap,
    /// How the LCD image is scaled.
    pub filter: ScaleFilter,
    /// Whether the debug panel is open.
    pub debug_open: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            keymap: Keymap::default_map(),
            filter: ScaleFilter::Nearest,
            debug_open: false,
        }
    }
}
