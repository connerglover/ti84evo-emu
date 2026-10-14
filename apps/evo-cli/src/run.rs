//! `evo-cli run`: boot a `.84b2` headless and run it until a stop condition, or serve it to
//! GDB.

use std::fs;
use std::io::{self, BufWriter, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::ops::{ControlFlow, Range};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use evo_core::{Machine, StopReason, UartPort};
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
    /// Stream UART0 TX to stdout.
    pub uart: bool,
    /// Bytes to feed into UART0 RX at cycle 0.
    pub uart_in: Option<PathBuf>,
}

/// Exit status for a lockup (0 is a normal stop, 1 a host-side error).
const EXIT_LOCKUP: u8 = 2;

/// The run goes in slices of at most this many cycles (10 ms of guest time); the UARTs are
/// drained after each.
pub const UART_SLICE: u64 = 1_560_000;

/// What the guest sent on the UARTs so far.
#[derive(Default)]
struct UartSink {
    /// Print UART0 bytes (`--uart`); otherwise they are discarded.
    print: bool,
    /// Bytes seen on UART2, the link port; reported on stderr at the end.
    uart2: u64,
}

impl UartSink {
    /// Drain both UARTs; UART0 goes to `out` if printing.
    fn drain(&mut self, m: &mut Machine, out: &mut impl Write) -> io::Result<()> {
        let tx = m.take_uart_tx(UartPort::Uart0);
        self.uart2 += m.take_uart_tx(UartPort::Uart2).len() as u64;
        if self.print && !tx.is_empty() {
            out.write_all(&tx)?;
        }
        Ok(())
    }

    /// `uart2: N bytes` on stderr if there was traffic on UART2.
    fn report(&self) {
        if self.uart2 > 0 {
            eprintln!("uart2: {} bytes", self.uart2);
        }
    }
}

pub fn main(args: RunArgs) -> ExitCode {
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
    if let Some(path) = &args.uart_in {
        match fs::read(path) {
            Ok(bytes) => machine.inject_uart_rx(UartPort::Uart0, &bytes),
            Err(e) => {
                eprintln!("evo-cli: {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        }
    }
    let mut uarts = UartSink {
        print: args.uart,
        ..UartSink::default()
    };
    if let Some(port) = args.gdb {
        return gdb(&mut machine, port, args.unmapped_out.as_deref(), &mut uarts);
    }

    // Written as the run goes, in buffered blocks: a trace can be gigabytes.
    let mut out = BufWriter::new(io::stdout().lock());
    // The first stdout error. It stops the run at the end of the slice, or at once while
    // tracing (e.g. `| head` closed the pipe): nothing more can be printed. The unmapped log is
    // still written.
    let mut stdout_err: Option<io::Error> = None;
    let tracing = args.trace || args.trace_range.is_some();
    let range = args.trace_range.clone().unwrap_or(0..u32::MAX);
    let end = machine.sched.now.saturating_add(args.max_cycles);
    let stop = loop {
        let slice = end.saturating_sub(machine.sched.now).min(UART_SLICE);
        let stop = if tracing {
            machine.run_with(slice, |m| {
                if range.contains(&m.cpu.regs.pc) {
                    stdout_err = writeln!(out, "{}", m.trace_line()).err();
                }
                match stdout_err {
                    Some(_) => ControlFlow::Break(()),
                    None => ControlFlow::Continue(()),
                }
            })
        } else {
            machine.run(slice)
        };
        if stdout_err.is_none() {
            stdout_err = uarts.drain(&mut machine, &mut out).err();
        }
        if stop != StopReason::CycleLimit || machine.sched.now >= end || stdout_err.is_some() {
            break stop;
        }
    };
    if stdout_err.is_none() {
        stdout_err = summary(&mut out, &machine, stop)
            .and_then(|()| out.flush())
            .err();
    }
    uarts.report();

    if !write_unmapped(args.unmapped_out.as_deref(), &machine) {
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

/// `--gdb PORT`: serve one GDB client on localhost, then report where it left the machine.
/// UART output the guest produced during the session is printed (with `--uart`) when it ends.
fn gdb(
    machine: &mut Machine,
    port: u16,
    unmapped_out: Option<&Path>,
    uarts: &mut UartSink,
) -> ExitCode {
    // Loopback only: whoever connects controls the machine.
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("evo-cli: --gdb {port}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match listener.local_addr() {
        Ok(addr) => {
            eprintln!("evo-cli: GDB stub listening on {addr} (in GDB: target remote {addr})");
        }
        Err(e) => {
            eprintln!("evo-cli: --gdb {port}: {e}");
            return ExitCode::FAILURE;
        }
    }
    if let Err(e) = evo_core::gdb::serve(machine, listener) {
        eprintln!("evo-cli: --gdb {port}: {e}");
        return ExitCode::FAILURE;
    }

    let mut out = io::stdout().lock();
    let printed = uarts
        .drain(machine, &mut out)
        .and_then(|()| {
            writeln!(
                out,
                "gdb: session ended pc=0x{:08x} cycles={}",
                machine.cpu.regs.pc, machine.sched.now
            )
        })
        .and_then(|()| notes(&mut out, machine, false))
        .and_then(|()| out.flush());
    uarts.report();
    if !write_unmapped(unmapped_out, machine) {
        return ExitCode::FAILURE;
    }
    match printed {
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => {
            eprintln!("evo-cli: {e}");
            ExitCode::FAILURE
        }
        _ => ExitCode::SUCCESS,
    }
}

/// Write the unmapped-access log to `path`, if given; false (reported) if that fails.
fn write_unmapped(path: Option<&Path>, m: &Machine) -> bool {
    let Some(path) = path else {
        return true;
    };
    match fs::write(path, m.bus.log.to_markdown()) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("evo-cli: {}: {e}", path.display());
            false
        }
    }
}

/// The stop line, then the notes.
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
    notes(out, m, stop == StopReason::Idle)
}

/// Notes on sleep (`idle`: nothing can end it), system resets and the unmapped log.
fn notes(out: &mut impl Write, m: &Machine, idle: bool) -> io::Result<()> {
    if idle {
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
    fn uart_slice_is_10_ms() {
        assert_eq!(UART_SLICE, evo_core::CPU_HZ / 100);
    }

    #[test]
    fn counts_are_decimal() {
        assert_eq!(parse_count("1_000_000_000"), Ok(1_000_000_000));
        assert_eq!(parse_count("0"), Ok(0));
        assert!(parse_count("0x10").is_err());
        assert!(parse_count("-1").is_err());
    }
}
