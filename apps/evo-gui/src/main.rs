//! The `evo-gui` binary: opens the emulator window.

use std::path::PathBuf;

use evo_gui::app::{AppOptions, EvoApp, TITLE};
use evo_gui::backend::fake::FakeBackend;
use evo_gui::prefs::{PREFS_KEY, Prefs};

fn main() -> eframe::Result {
    env_logger::init();
    let data_dir =
        eframe::storage_dir("evo-gui").unwrap_or_else(|| PathBuf::from("./evo-gui-data"));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size([520.0, 960.0])
            .with_min_inner_size([320.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        TITLE,
        options,
        Box::new(move |cc| {
            // Missing or unreadable preferences fall back to the defaults.
            let prefs = cc
                .storage
                .and_then(|s| eframe::get_value::<Prefs>(s, PREFS_KEY))
                .unwrap_or_default();
            let opts = AppOptions {
                prefs,
                ..AppOptions::new(data_dir)
            };
            Ok(Box::new(EvoApp::new(
                &cc.egui_ctx,
                Box::new(FakeBackend::new()),
                opts,
            )))
        }),
    )
}
