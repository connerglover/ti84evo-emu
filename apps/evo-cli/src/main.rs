//! `evo-cli`: command line of the TI-84 Evo emulator.

mod inspect;
mod score;

use std::io::{self, BufWriter, Write};
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
}

fn main() -> ExitCode {
    match Cli::parse().command {
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
        } = cli.command;
        assert_eq!(bundle, PathBuf::from("os.84b2"));
        assert!(score_bases);

        let cli = Cli::try_parse_from(["evo-cli", "inspect", "os.84b2"]).expect("parse");
        let Command::Inspect { score_bases, .. } = cli.command;
        assert!(!score_bases);

        assert!(Cli::try_parse_from(["evo-cli", "inspect"]).is_err());
    }
}
