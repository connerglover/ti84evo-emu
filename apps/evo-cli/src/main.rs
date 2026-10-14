//! `evo-cli`: command line of the TI-84 Evo emulator.

mod inspect;
mod run;
mod score;

use std::io::{self, BufWriter, Write};
use std::ops::Range;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use evo_image::{Bundle, Layout};

#[derive(Parser)]
#[command(name = "evo-cli", version, about = "TI-84 Evo emulator")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show a .84b2 bundle's metadata, images, and the vector tables and
    /// reset handlers of images 0 and 1 placed per Layout::EVO_7_1.
    Inspect {
        /// Path to the .84b2 bundle.
        bundle: PathBuf,
        /// Also rank candidate flash bases for images 0 and 1.
        #[arg(long)]
        score_bases: bool,
    },
    /// Boot a .84b2 bundle (images placed per Layout::EVO_7_1) and run it headless.
    ///
    /// Prints `stop: <reason> pc=<hex> cycles=<n>` at the end. Exit status: 0 when the
    /// cycle budget runs out, the guest sleeps with nothing left to wake it (idle), or a
    /// breakpoint or guest BKPT is reached; 2 on CPU lockup; 1 on errors.
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
        /// Serve a GDB remote stub on PORT (not implemented yet).
        #[arg(long, value_name = "PORT")]
        gdb: Option<u16>,
    },
}

fn main() -> ExitCode {
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
        Command::Run {
            bundle,
            max_cycles,
            trace,
            trace_range,
            breaks,
            unmapped_out,
            gdb,
        } => run::main(run::RunArgs {
            bundle,
            max_cycles,
            trace,
            trace_range,
            breaks,
            unmapped_out,
            gdb,
        }),
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
        } = cli.command
        else {
            panic!("not run");
        };
        assert_eq!(bundle, PathBuf::from("os.84b2"));
        assert_eq!(max_cycles, 1_000_000_000);
        assert!(!trace);
        assert_eq!(trace_range, None);
        assert!(breaks.is_empty());
        assert_eq!(unmapped_out, None);
        assert_eq!(gdb, None);
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
            "--gdb",
            "3333",
        ])
        .expect("parse");
        let Command::Run {
            max_cycles,
            trace,
            trace_range,
            breaks,
            unmapped_out,
            gdb,
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
        assert_eq!(gdb, Some(3333));

        assert!(Cli::try_parse_from(["evo-cli", "run"]).is_err());
        assert!(Cli::try_parse_from(["evo-cli", "run", "x", "--break", "zz"]).is_err());
        assert!(Cli::try_parse_from(["evo-cli", "run", "x", "--max-cycles", "-5"]).is_err());
        assert!(Cli::try_parse_from(["evo-cli", "run", "x", "--trace-range", "9..1"]).is_err());
    }
}
