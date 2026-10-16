//! `evo-cli`: command line of the TI-84 Evo emulator.

mod inspect;
mod run;
mod score;
mod usb_bridge;

use std::io::{self, BufWriter, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use evo_core::Machine;
use evo_image::{Bundle, Layout};

#[derive(Parser)]
#[command(name = "evo-cli", version, about = "TI-84 Evo emulator")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

// Parsed once per process; boxing the `run` options would only add noise.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Command {
    /// Serve USB bus tokens over newline-delimited JSON on stdin/stdout.
    /// Used by the TI Connect serial adapter; no network port is opened.
    UsbBridge {
        bundle: PathBuf,
        /// Persist calculator storage, including variables received over USB.
        #[arg(long)]
        flash_file: Option<PathBuf>,
        /// Perform first-use provisioning before accepting requests.
        #[arg(long)]
        provision: bool,
    },
    /// Show a .84b2 bundle's metadata, images, and the vector tables and
    /// reset handlers of images 0 and 1 placed per Layout::EVO_7_1.
    Inspect {
        /// Path to the .84b2 bundle.
        bundle: PathBuf,
        /// Also rank candidate flash bases for images 0 and 1.
        #[arg(long)]
        score_bases: bool,
    },
    /// Boot a .84b2 bundle (images placed per Layout::EVO_7_1) and run it headless, or
    /// under GDB.
    ///
    /// Prints `stop: <reason> pc=<hex> cycles=<n>` at the end. Exit status: 0 when the
    /// cycle budget runs out, the guest sleeps with nothing left to wake it (idle), or a
    /// breakpoint or guest BKPT is reached; 2 on CPU lockup; 1 on errors.
    ///
    /// Ctrl-C stops a headless run within one 10 ms slice (`stop: interrupted`, exit status
    /// 130) and still writes --save-state, --flash-file, --screenshot and --unmapped-out; a
    /// second Ctrl-C quits at once without writing them. Under --gdb, Ctrl-C ends the process.
    Run {
        /// Path to the .84b2 bundle.
        bundle: PathBuf,
        /// Cycle budget (decimal; `_` separators allowed).
        #[arg(long, value_name = "N", default_value = "1_000_000_000", value_parser = run::parse_count)]
        max_cycles: u64,
        /// Print every instruction before it executes:
        /// `{cycles:>12} {S|N} {pc:08x}: {disasm}`.
        #[arg(long)]
        trace: bool,
        /// Trace only instructions at addresses in A..B (hex, B exclusive); implies --trace.
        #[arg(long, value_name = "A..B", value_parser = run::parse_range)]
        trace_range: Option<Range<u32>>,
        /// Stop before the instruction at ADDR (hex) executes. Repeatable.
        #[arg(long = "break", value_name = "ADDR", value_parser = run::parse_addr)]
        breaks: Vec<u32>,
        /// Write the unmapped-access log as a markdown table to FILE.md.
        #[arg(long, value_name = "FILE.md")]
        unmapped_out: Option<PathBuf>,
        /// Instead of running, serve one GDB client on 127.0.0.1:PORT (0 picks a free port;
        /// the address is printed on stderr), with the CPU halted at reset. GDB controls the
        /// run, so --max-cycles and tracing are not allowed. When the client detaches or
        /// hangs up, prints `gdb: session ended pc=<hex> cycles=<n>` and exits 0.
        #[arg(
            long,
            value_name = "PORT",
            conflicts_with_all = ["max_cycles", "trace", "trace_range"]
        )]
        gdb: Option<u16>,
        /// Stream the guest's UART0 output (the TF-M and BSP log) to stdout, raw, as it runs.
        /// UART2 (the link port) is not printed; a run that saw traffic on it ends with
        /// `uart2: N bytes` on stderr.
        #[arg(long)]
        uart: bool,
        /// Feed the bytes of FILE into UART0's receiver at the start of the run (after
        /// --load-state).
        #[arg(long, value_name = "FILE")]
        uart_in: Option<PathBuf>,
        /// Keep the flash and the NOR security registers in PATH across runs: read at the
        /// start (an erased part if PATH does not exist; an invalid file is an error that
        /// leaves it untouched), the bundle's images placed over it, and written back however
        /// the run ends. A failed write-back exits 1.
        #[arg(long, value_name = "PATH")]
        flash_file: Option<PathBuf>,
        /// Restore the machine from a save state written by --save-state, after booting the
        /// bundle (and reading --flash-file). The state must come from the same bundle and the
        /// same flash: the flash file as the run that saved the state wrote it back. A corrupt,
        /// invalid or mismatched state is an error (exit 1) before the run starts, and the flash
        /// file is not touched. The --max-cycles budget counts from the restored cycle.
        #[arg(long, value_name = "FILE")]
        load_state: Option<PathBuf>,
        /// Write a save state of the machine to FILE when the run ends, however it ends. The
        /// flash and the NOR security registers are not in it (see --flash-file); without
        /// --flash-file a note on stderr says that loading it needs the same flash. A failed
        /// write exits 1.
        #[arg(long, value_name = "FILE")]
        save_state: Option<PathBuf>,
        /// Write the LCD image (320x240) to FILE.png as an RGB8 PNG when the run stops, or at
        /// --after. While the LCD is off, prints `screenshot: display is off` on stderr and
        /// writes nothing; the exit status is unchanged.
        #[arg(long, value_name = "FILE.png", conflicts_with = "gdb")]
        screenshot: Option<PathBuf>,
        /// Take the screenshot at cycle N of the run (counted from its start) instead of when
        /// it stops. Needs --screenshot.
        #[arg(
            long,
            value_name = "N",
            requires = "screenshot",
            value_parser = run::parse_count
        )]
        after: Option<u64>,
        /// Press keys as the run goes: a key script of tokens separated by whitespace or `;`.
        /// `KEY` taps it (50 ms hold, 50 ms gap), `+KEY` presses, `-KEY` releases, `wait=MS`
        /// moves on. KEY is a TI-84 key name (`enter`, `alpha`, `2nd`, `on`, digits, arrows,
        /// ...) or `r<R>c<C>`. Times are milliseconds of guest time from the start of the run.
        #[arg(
            long,
            value_name = "SCRIPT",
            conflicts_with = "gdb",
            value_parser = run::parse_keys
        )]
        keys: Option<run::KeyScript>,
    },
    /// Provision a unit to boot straight to the OS, as the factory does: power on with
    /// ALPHA + SIN + ENTER held, set Engineering > Boot Select to Calc OS, exit, then check
    /// that a power-on with no key held enters OS image 1 (docs/hardware/boot-flow.md §3.5).
    ///
    /// Reads FILE (an erased part if it does not exist) and writes the provisioned flash back
    /// only when every step succeeds; on failure FILE is left untouched and the exit status is
    /// 1. Takes about 0.8 G emulated cycles (about a minute in a release build).
    Provision {
        /// Path to the .84b2 bundle.
        bundle: PathBuf,
        /// The flash file to provision (as `run --flash-file` uses it).
        #[arg(long, value_name = "FILE")]
        flash_file: PathBuf,
    },
}

fn main() -> ExitCode {
    // Core diagnostics (`log::warn!`: undefined instructions with their PC, lockup,
    // unimplemented PPB registers, bad EXC_RETURN values, GDB protocol errors) go to stderr.
    // `RUST_LOG` overrides the default `warn` filter (e.g. `RUST_LOG=debug`, `RUST_LOG=off`).
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .init();
    // Not `Cli::parse()`: clap exits with 2 on a usage error, which `run` uses for lockup.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // Printing to a closed stdout/stderr is not worth failing over.
            let _ = e.print();
            // `--help` and `--version` also arrive here, on stdout, and are not errors.
            return if e.use_stderr() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    match cli.command {
        Command::UsbBridge {
            bundle,
            flash_file,
            provision,
        } => usb_bridge::main(&bundle, flash_file.as_deref(), provision),
        Command::Run {
            bundle,
            max_cycles,
            trace,
            trace_range,
            breaks,
            unmapped_out,
            gdb,
            uart,
            uart_in,
            flash_file,
            load_state,
            save_state,
            screenshot,
            after,
            keys,
        } => run::main(run::RunArgs {
            bundle,
            max_cycles,
            trace,
            trace_range,
            breaks,
            unmapped_out,
            gdb,
            uart,
            uart_in,
            flash_file,
            load_state,
            save_state,
            screenshot,
            after,
            keys: keys.map(|k| k.0).unwrap_or_default(),
        }),
        Command::Provision { bundle, flash_file } => provision(&bundle, &flash_file),
        Command::Inspect {
            bundle,
            score_bases,
        } => {
            let parsed = match Bundle::open(&bundle) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("evo-cli: {}: {e}", bundle.display());
                    return ExitCode::FAILURE;
                }
            };
            let mut out = BufWriter::new(io::stdout().lock());
            let written = inspect::run(&parsed, &Layout::EVO_7_1, score_bases, &mut out)
                .and_then(|()| out.flush());
            match written {
                Ok(()) => ExitCode::SUCCESS,
                // A closed pipe (`| head`) is not an error worth reporting.
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("evo-cli: {e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

/// `evo-cli provision`: provision the flash in `flash_file` and write it back on success.
fn provision(bundle: &Path, flash_file: &Path) -> ExitCode {
    let parsed = match Bundle::open(bundle) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("evo-cli: {}: {e}", bundle.display());
            return ExitCode::FAILURE;
        }
    };
    let Ok(nv) = run::read_flash_file(flash_file) else {
        return ExitCode::FAILURE;
    };
    let mut machine = match Machine::from_bundle_nv(&parsed, &Layout::EVO_7_1, nv) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("evo-cli: {}: {e}", bundle.display());
            return ExitCode::FAILURE;
        }
    };
    let report = match evo_core::provision(&mut machine) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "evo-cli: provisioning failed: {e}; {} not written",
                flash_file.display()
            );
            return ExitCode::FAILURE;
        }
    };
    println!(
        "provision: Engineering combo at cycle {}, menu at {}, Boot Select {}, OS launched at {}",
        report.combo_cycle,
        report.menu_cycle,
        if report.toggled {
            "set to Calc OS"
        } else {
            "already Calc OS"
        },
        report.launch_cycle
    );
    println!(
        "provision: power-on with no key held entered the OS at cycle {}",
        report.boot_cycle
    );
    match machine.nv_state().write_file(flash_file) {
        Ok(()) => {
            println!("provision: wrote {}", flash_file.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("evo-cli: {}: {e}", flash_file.display());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn inspect_parses_bundle_and_flag() {
        let cli =
            Cli::try_parse_from(["evo-cli", "inspect", "os.84b2", "--score-bases"]).expect("parse");
        let Command::Inspect {
            bundle,
            score_bases,
        } = cli.command
        else {
            panic!("not inspect");
        };
        assert_eq!(bundle, PathBuf::from("os.84b2"));
        assert!(score_bases);

        let cli = Cli::try_parse_from(["evo-cli", "inspect", "os.84b2"]).expect("parse");
        let Command::Inspect { score_bases, .. } = cli.command else {
            panic!("not inspect");
        };
        assert!(!score_bases);

        assert!(Cli::try_parse_from(["evo-cli", "inspect"]).is_err());
    }

    #[test]
    fn run_defaults() {
        let cli = Cli::try_parse_from(["evo-cli", "run", "os.84b2"]).expect("parse");
        let Command::Run {
            bundle,
            max_cycles,
            trace,
            trace_range,
            breaks,
            unmapped_out,
            gdb,
            uart,
            uart_in,
            flash_file,
            load_state,
            save_state,
            screenshot,
            after,
            keys,
        } = cli.command
        else {
            panic!("not run");
        };
        assert_eq!(screenshot, None);
        assert_eq!(load_state, None);
        assert_eq!(save_state, None);
        assert_eq!(after, None);
        assert_eq!(keys, None);
        assert_eq!(bundle, PathBuf::from("os.84b2"));
        assert_eq!(max_cycles, 1_000_000_000);
        assert!(!trace);
        assert_eq!(trace_range, None);
        assert!(breaks.is_empty());
        assert_eq!(unmapped_out, None);
        assert_eq!(gdb, None);
        assert!(!uart);
        assert_eq!(uart_in, None);
        assert_eq!(flash_file, None);
    }

    #[test]
    fn run_parses_every_option() {
        let cli = Cli::try_parse_from([
            "evo-cli",
            "run",
            "os.84b2",
            "--max-cycles",
            "2_000_000",
            "--trace",
            "--trace-range",
            "0x100b0000..0x100c0000",
            "--break",
            "0x100b1adc",
            "--break",
            "44bc90",
            "--unmapped-out",
            "log.md",
            "--uart",
            "--uart-in",
            "in.bin",
            "--flash-file",
            "nv.evoflash",
            "--load-state",
            "in.state",
            "--save-state",
            "out.state",
            "--screenshot",
            "s.png",
            "--after",
            "1_000",
            "--keys",
            "+on wait=10;-on",
        ])
        .expect("parse");
        let Command::Run {
            max_cycles,
            trace,
            trace_range,
            breaks,
            unmapped_out,
            gdb,
            uart,
            uart_in,
            flash_file,
            load_state,
            save_state,
            screenshot,
            after,
            keys,
            ..
        } = cli.command
        else {
            panic!("not run");
        };
        assert_eq!(max_cycles, 2_000_000);
        assert!(trace);
        assert_eq!(trace_range, Some(0x100b_0000..0x100c_0000));
        assert_eq!(breaks, [0x100b_1adc, 0x0044_bc90]);
        assert_eq!(unmapped_out, Some(PathBuf::from("log.md")));
        assert_eq!(gdb, None);
        assert!(uart);
        assert_eq!(uart_in, Some(PathBuf::from("in.bin")));
        assert_eq!(flash_file, Some(PathBuf::from("nv.evoflash")));
        assert_eq!(load_state, Some(PathBuf::from("in.state")));
        assert_eq!(save_state, Some(PathBuf::from("out.state")));
        assert_eq!(screenshot, Some(PathBuf::from("s.png")));
        assert_eq!(after, Some(1_000));
        let on = evo_core::Key::On;
        assert_eq!(
            keys.map(|k| k.0),
            Some(vec![
                evo_core::KeyAction {
                    at_ms: 0,
                    key: on,
                    pressed: true
                },
                evo_core::KeyAction {
                    at_ms: 10,
                    key: on,
                    pressed: false
                },
            ])
        );
        assert!(Cli::try_parse_from(["evo-cli", "run", "x", "--keys", "bogus"]).is_err());
        assert!(Cli::try_parse_from(["evo-cli", "run", "x", "--after", "5"]).is_err());

        assert!(Cli::try_parse_from(["evo-cli", "run"]).is_err());
        assert!(Cli::try_parse_from(["evo-cli", "run", "x", "--break", "zz"]).is_err());
        assert!(Cli::try_parse_from(["evo-cli", "run", "x", "--max-cycles", "-5"]).is_err());
        assert!(Cli::try_parse_from(["evo-cli", "run", "x", "--trace-range", "9..1"]).is_err());
    }

    #[test]
    fn provision_needs_bundle_and_flash_file() {
        let cli = Cli::try_parse_from([
            "evo-cli",
            "provision",
            "os.84b2",
            "--flash-file",
            "nv.evoflash",
        ])
        .expect("parse");
        let Command::Provision { bundle, flash_file } = cli.command else {
            panic!("not provision");
        };
        assert_eq!(bundle, PathBuf::from("os.84b2"));
        assert_eq!(flash_file, PathBuf::from("nv.evoflash"));
        assert!(Cli::try_parse_from(["evo-cli", "provision", "os.84b2"]).is_err());
        assert!(Cli::try_parse_from(["evo-cli", "provision", "--flash-file", "f"]).is_err());
    }

    /// GDB drives the run: no budget and no trace, but breakpoints and the unmapped log.
    #[test]
    fn run_gdb_excludes_budget_and_trace() {
        let cli = Cli::try_parse_from([
            "evo-cli",
            "run",
            "os.84b2",
            "--gdb",
            "3333",
            "--break",
            "10",
            "--unmapped-out",
            "log.md",
        ])
        .expect("parse");
        let Command::Run { gdb, breaks, .. } = cli.command else {
            panic!("not run");
        };
        assert_eq!((gdb, breaks), (Some(3333), vec![0x10]));

        for other in [
            &["--trace"][..],
            &["--trace-range", "0..10"],
            &["--max-cycles", "5"],
            &["--screenshot", "s.png"],
            &["--keys", "on"],
        ] {
            let args = ["evo-cli", "run", "x", "--gdb", "3333"];
            let parsed = Cli::try_parse_from(args.iter().chain(other));
            assert!(parsed.is_err(), "--gdb with {other:?}");
        }
    }
}
