//! `evo-cli run`: boot a `.84b2` headless and run it until a stop condition.

use std::fs;
use std::io::{self, BufWriter, Write};
use std::ops::{ControlFlow, Range};
use std::path::PathBuf;
use std::process::ExitCode;

use evo_core::{Machine, StopReason};
use evo_image::{Bundle, Layout};

/// What `evo-cli run` was asked to do.
pub struct RunArgs {
    pub bundle: PathBuf,
    pub max_cycles: u64,
    pub trace: bool,
    pub trace_range: Option<Range<u32>>,
    pub breaks: Vec<u32>,
    pub unmapped_out: Option<PathBuf>,
    pub gdb: Option<u16>,
}

/// Exit status for a lockup (0 is a normal stop, 1 a host-side error).
const EXIT_LOCKUP: u8 = 2;

pub fn main(args: RunArgs) -> ExitCode {
    if let Some(port) = args.gdb {
        eprintln!("evo-cli: --gdb {port}: the GDB remote stub is not implemented yet");
        return ExitCode::FAILURE;
    }
    let bundle = match Bundle::open(&args.bundle) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("evo-cli: {}: {e}", args.bundle.display());
            return ExitCode::FAILURE;
        }
    };
    let mut machine = match Machine::from_bundle(&bundle, &Layout::EVO_7_1) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("evo-cli: {}: {e}", args.bundle.display());
            return ExitCode::FAILURE;
        }
    };
    // Breakpoints compare against the PC, whose bit 0 is always clear.
    machine
        .breakpoints
        .extend(args.breaks.iter().map(|a| a & !1));

    // Written as the run goes, in buffered blocks: a trace can be gigabytes.
    let mut out = BufWriter::new(io::stdout().lock());
    // The first stdout error. A trace stops the run at the first one (e.g. `| head` closed
    // the pipe): nothing more can be printed. The unmapped log is still written.
    let mut stdout_err: Option<io::Error> = None;
    let stop = if args.trace || args.trace_range.is_some() {
        let range = args.trace_range.clone().unwrap_or(0..u32::MAX);
        machine.run_with(args.max_cycles, |m| {
            if range.contains(&m.cpu.regs.pc) {
                stdout_err = writeln!(out, "{}", m.trace_line()).err();
            }
            match stdout_err {
                Some(_) => ControlFlow::Break(()),
                None => ControlFlow::Continue(()),
            }
        })
    } else {
        machine.run(args.max_cycles)
    };
    if stdout_err.is_none() {
        stdout_err = summary(&mut out, &machine, stop)
            .and_then(|()| out.flush())
            .err();
    }

    if let Some(path) = &args.unmapped_out
        && let Err(e) = fs::write(path, machine.bus.log.to_markdown())
    {
        eprintln!("evo-cli: {}: {e}", path.display());
        return ExitCode::FAILURE;
    }
    match stdout_err {
        // A closed pipe is not an error worth reporting.
        Some(e) if e.kind() != io::ErrorKind::BrokenPipe => {
            eprintln!("evo-cli: {e}");
            ExitCode::FAILURE
        }
        _ => exit_code(stop),
    }
}

/// The stop line, then notes on sleep, system resets and the unmapped log.
fn summary(out: &mut impl Write, m: &Machine, stop: StopReason) -> io::Result<()> {
    let (reason, pc) = match stop {
        StopReason::CycleLimit => ("cycle-limit", m.cpu.regs.pc),
        StopReason::Idle => ("idle", m.cpu.regs.pc),
        // `run` reports neither; `Halted` only when the trace output is gone.
        StopReason::Stepped => ("stepped", m.cpu.regs.pc),
        StopReason::Halted => ("halted", m.cpu.regs.pc),
        StopReason::Breakpoint(pc) => ("breakpoint", pc),
        StopReason::Lockup { pc } => ("lockup", pc),
        StopReason::Bkpt { pc, .. } => ("bkpt", pc),
    };
    write!(out, "stop: {reason} pc=0x{pc:08x} cycles={}", m.sched.now)?;
    if let StopReason::Bkpt { imm, .. } = stop {
        write!(out, " imm=0x{imm:02x}")?;
    }
    writeln!(out)?;
    if stop == StopReason::Idle {
        writeln!(
            out,
            "idle: the CPU is asleep (WFI/WFE) and nothing is pending or scheduled to wake it"
        )?;
    } else if m.asleep() {
        writeln!(
            out,
            "asleep: the CPU is in WFI/WFE, waiting for an interrupt or event"
        )?;
    }
    writeln!(
        out,
        "resets: {} (system resets the guest requested through AIRCR.SYSRESETREQ)",
        m.resets
    )?;
    let log = &m.bus.log;
    write!(out, "unmapped: {} distinct accesses logged", log.len())?;
    if log.dropped() > 0 {
        write!(out, ", {} more not recorded (log full)", log.dropped())?;
    }
    writeln!(out)
}

fn exit_code(stop: StopReason) -> ExitCode {
    match stop {
        StopReason::Lockup { .. } => ExitCode::from(EXIT_LOCKUP),
        StopReason::CycleLimit
        | StopReason::Idle
        | StopReason::Stepped
        | StopReason::Halted
        | StopReason::Breakpoint(_)
        | StopReason::Bkpt { .. } => ExitCode::SUCCESS,
    }
}

/// An address in hex, with or without `0x`.
pub fn parse_addr(s: &str) -> Result<u32, String> {
    let digits = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s)
        .replace('_', "");
    u32::from_str_radix(&digits, 16).map_err(|e| format!("`{s}` is not a hex address: {e}"))
}

/// `A..B` (hex addresses, B exclusive).
pub fn parse_range(s: &str) -> Result<Range<u32>, String> {
    let (a, b) = s
        .split_once("..")
        .ok_or_else(|| format!("`{s}` is not a range A..B"))?;
    let (a, b) = (parse_addr(a)?, parse_addr(b)?);
    if a > b {
        return Err(format!("range `{s}` ends before it starts"));
    }
    Ok(a..b)
}

/// A decimal count; `_` separators allowed (`1_000_000`).
pub fn parse_count(s: &str) -> Result<u64, String> {
    s.replace('_', "")
        .parse()
        .map_err(|e| format!("`{s}` is not a cycle count: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_hex_with_optional_prefix() {
        assert_eq!(parse_addr("0x100b0604"), Ok(0x100b_0604));
        assert_eq!(parse_addr("0X10"), Ok(0x10));
        assert_eq!(parse_addr("100b_0604"), Ok(0x100b_0604));
        assert_eq!(parse_addr("10"), Ok(0x10));
        assert!(parse_addr("").is_err());
        assert!(parse_addr("0x").is_err());
        assert!(parse_addr("xyz").is_err());
        assert!(parse_addr("0x1_0000_0000").is_err());
    }

    #[test]
    fn ranges_are_half_open() {
        assert_eq!(parse_range("0x10..0x20"), Ok(0x10..0x20));
        assert_eq!(parse_range("10..10"), Ok(0x10..0x10));
        assert!(parse_range("0x20..0x10").is_err());
        assert!(parse_range("0x10").is_err());
        assert!(parse_range("0x10..").is_err());
    }

    #[test]
    fn counts_are_decimal() {
        assert_eq!(parse_count("1_000_000_000"), Ok(1_000_000_000));
        assert_eq!(parse_count("0"), Ok(0));
        assert!(parse_count("0x10").is_err());
        assert!(parse_count("-1").is_err());
    }
}
