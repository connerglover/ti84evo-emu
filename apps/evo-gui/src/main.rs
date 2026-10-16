//! The `evo-gui` binary: opens the emulator window.

// Release builds on Windows are GUI apps: no console window next to the emulator.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use evo_gui::app::{AppOptions, Choice, EvoApp, StartUp, TITLE, choose_backend, start_backend};
use evo_gui::backend::EmuBackend;
use evo_gui::backend::fake::FakeBackend;
use evo_gui::backend::machine::MachineBackend;
use evo_gui::platform::DesktopPlatform;
use evo_gui::prefs::{PREFS_KEY, Prefs};

/// TI-84 Evo emulator window.
#[derive(Parser)]
#[command(name = "evo-gui", version)]
struct Args {
    /// The OS bundle (.84b2) to boot. Defaults to $EVO_OS_IMAGE, then to the bundle the ROM
    /// Setup Wizard linked.
    bundle: Option<PathBuf>,
    /// Run the demo backend instead of a bundle.
    #[arg(long)]
    demo: bool,
    /// Run as this instance, with its own flash, slots and settings.
    #[arg(long, value_name = "ID")]
    instance: Option<String>,
    /// Rename instance ID to --instance before starting ("Change ID").
    #[arg(long, value_name = "ID", requires = "instance")]
    rename_from: Option<String>,
}

/// The backend `main` opened before the window, or `None` to decide from the preferences.
type Opened = Option<(Box<dyn EmuBackend>, Option<PathBuf>)>;

fn main() -> ExitCode {
    evo_gui::logbuf::init();
    let args = Args::parse();
    // Preferences, slots, screenshots and flash files live together (eframe's storage dir);
    // another instance lives in `instances/<id>` below it.
    let base = eframe::storage_dir("evo-gui").unwrap_or_else(|| PathBuf::from("./evo-gui-data"));
    let lock = match evo_gui::instance::prepare(
        &base,
        args.instance.as_deref(),
        args.rename_from.as_deref(),
    ) {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!("evo-gui: {e}");
            return ExitCode::from(1);
        }
    };
    let data_dir = lock.dir.clone();
    let opened: Opened =
        match choose_backend(args.bundle, args.demo, std::env::var_os("EVO_OS_IMAGE")) {
            Choice::Demo => Some((Box::new(FakeBackend::new()), None)),
            Choice::Bundle(path) => match MachineBackend::open(&path, &data_dir.join("flash")) {
                Ok(b) => Some((Box::new(b), Some(path))),
                Err(e) => {
                    eprintln!("evo-gui: {}: {e}", path.display());
                    return ExitCode::from(1);
                }
            },
            Choice::FromPrefs => None,
        };
    let result = run(opened, data_dir, args.instance);
    // The instance lock is held until the window has closed.
    drop(lock);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("evo-gui: {e}");
            ExitCode::from(1)
        }
    }
}

fn run(opened: Opened, data_dir: PathBuf, instance: Option<String>) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size([520.0, 960.0])
            .with_min_inner_size([320.0, 600.0]),
        // An instance keeps its window and preferences in its own directory.
        persistence_path: instance.as_ref().map(|_| data_dir.join("app.ron")),
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
            let platform = DesktopPlatform::shared();
            let (backend, rom, notice, open_wizard) = match opened {
                Some((backend, rom)) => (backend, rom, None, false),
                None => {
                    let StartUp {
                        backend,
                        notice,
                        open_wizard,
                    } = start_backend(&prefs, &platform, &data_dir);
                    let rom = if open_wizard {
                        None
                    } else {
                        prefs.bundle.clone()
                    };
                    (backend, rom, notice, open_wizard)
                }
            };
            let opts = AppOptions {
                prefs,
                notice,
                platform,
                open_wizard,
                rom,
                instance,
                ..AppOptions::new(data_dir)
            };
            Ok(Box::new(EvoApp::new(&cc.egui_ctx, backend, opts)))
        }),
    )
}
