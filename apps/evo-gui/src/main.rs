//! The `evo-gui` binary: opens the emulator window.

// Release builds on Windows are GUI apps: no console window next to the emulator.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use evo_gui::app::{AppOptions, Choice, DEMO_NOTICE, EvoApp, TITLE, choose_backend};
use evo_gui::backend::EmuBackend;
use evo_gui::backend::fake::FakeBackend;
use evo_gui::backend::machine::MachineBackend;
use evo_gui::prefs::{PREFS_KEY, Prefs};

/// TI-84 Evo emulator window.
#[derive(Parser)]
#[command(name = "evo-gui", version)]
struct Args {
    /// The OS bundle (.84b2) to boot. Defaults to $EVO_OS_IMAGE.
    bundle: Option<PathBuf>,
    /// Run the demo backend instead of a bundle.
    #[arg(long)]
    demo: bool,
}

fn main() -> ExitCode {
    evo_gui::logbuf::init();
    let args = Args::parse();
    // Preferences, slots, screenshots and flash files live together (eframe's storage dir).
    let data_dir =
        eframe::storage_dir("evo-gui").unwrap_or_else(|| PathBuf::from("./evo-gui-data"));
    let (backend, notice): (Box<dyn EmuBackend>, Option<String>) =
        match choose_backend(args.bundle, args.demo, std::env::var_os("EVO_OS_IMAGE")) {
            Choice::Demo { notice } => (
                Box::new(FakeBackend::new()),
                notice.then(|| DEMO_NOTICE.to_owned()),
            ),
            Choice::Bundle(path) => match MachineBackend::open(&path, &data_dir.join("flash")) {
                Ok(b) => (Box::new(b), None),
                Err(e) => {
                    eprintln!("evo-gui: {}: {e}", path.display());
                    return ExitCode::from(1);
                }
            },
        };
    match run(backend, notice, data_dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("evo-gui: {e}");
            ExitCode::from(1)
        }
    }
}

fn run(backend: Box<dyn EmuBackend>, notice: Option<String>, data_dir: PathBuf) -> eframe::Result {
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
                notice,
                ..AppOptions::new(data_dir)
            };
            Ok(Box::new(EvoApp::new(&cc.egui_ctx, backend, opts)))
        }),
    )
}
