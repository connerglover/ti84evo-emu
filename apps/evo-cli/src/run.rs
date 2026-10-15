//! `evo-cli run`: boot a `.84b2` headless and run it until a stop condition, or serve it to
//! GDB.

use std::fs;
use std::io::{self, BufWriter, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::ops::{ControlFlow, Range};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use evo_core::{
    CPU_HZ, Frame, KeyAction, Machine, NvError, NvState, StopReason, UartPort, parse_key_script,
    write_atomic,
};
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
    /// The flash file: read at the start (missing: an erased part), written back at the end.
    pub flash_file: Option<PathBuf>,
    /// A save state to restore after booting (and reading the flash file).
    pub load_state: Option<PathBuf>,
    /// Where to write a save state when the run ends.
    pub save_state: Option<PathBuf>,
    /// Where to write the screenshot PNG.
    pub screenshot: Option<PathBuf>,
    /// Cycles from the start of the run at which to take the screenshot; `None`: at the stop.
    pub after: Option<u64>,
    /// Key presses and releases, in script order (times never decrease).
    pub keys: Vec<KeyAction>,
}

/// A parsed `--keys` script (a newtype: clap would read a bare `Vec` as repeated values).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyScript(pub Vec<KeyAction>);

/// `--keys SCRIPT`: the `evo_core::keys` grammar.
pub fn parse_keys(s: &str) -> Result<KeyScript, String> {
    parse_key_script(s)
        .map(KeyScript)
        .map_err(|e| e.to_string())
}

/// The cycle at which an action `at_ms` milliseconds after `start` fires:
/// `start + at_ms * CPU_HZ / 1000`, saturating.
fn action_cycle(start: u64, at_ms: u64) -> u64 {
    let offset = u128::from(at_ms) * u128::from(CPU_HZ) / 1000;
    start.saturating_add(u64::try_from(offset).unwrap_or(u64::MAX))
}

/// The key script laid out in cycles, consumed as the run reaches each action.
struct KeyTimeline {
    /// (cycle, action), in script order.
    actions: Vec<(u64, KeyAction)>,
    next: usize,
}

impl KeyTimeline {
    fn new(actions: &[KeyAction], start: u64) -> Self {
        KeyTimeline {
            actions: actions
                .iter()
                .map(|a| (action_cycle(start, a.at_ms), *a))
                .collect(),
            next: 0,
        }
    }

    /// The cycle of the next action not yet applied.
    fn next_at(&self) -> Option<u64> {
        self.actions.get(self.next).map(|&(at, _)| at)
    }

    /// Apply every action due at or before the machine's current time.
    fn apply_due(&mut self, m: &mut Machine) {
        while let Some(&(at, a)) = self.actions.get(self.next) {
            if at > m.sched.now {
                break;
            }
            m.set_key(a.key, a.pressed);
            self.next += 1;
        }
    }
}

/// Exit status for a lockup (0 is a normal stop, 1 a host-side error).
const EXIT_LOCKUP: u8 = 2;
/// Exit status of a run ended by Ctrl-C (128 + SIGINT, as shells report it).
const EXIT_INTERRUPTED: u8 = 130;

/// Set by the Ctrl-C handler: the run stops at the end of the current slice.
static INTERRUPT: AtomicBool = AtomicBool::new(false);

/// Ctrl-C ends a headless run gracefully: the first one stops the run at the end of the
/// current slice, and the outputs (--save-state, --flash-file, ...) are written as for any
/// other stop; a second one exits at once, writing nothing. Not installed for --gdb: there
/// GDB interrupts the target, and Ctrl-C on the stub ends the process as before.
fn install_interrupt_handler() {
    let res = ctrlc::set_handler(|| {
        if INTERRUPT.swap(true, Ordering::SeqCst) {
            eprintln!("evo-cli: interrupted again; exiting without writing outputs");
            std::process::exit(i32::from(EXIT_INTERRUPTED));
        }
        eprintln!("evo-cli: interrupted; stopping the run (Ctrl-C again to quit at once)");
    });
    if let Err(e) = res {
        eprintln!(
            "evo-cli: cannot handle Ctrl-C ({e}); it will end the run without writing outputs"
        );
    }
}

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
    if args.gdb.is_none() {
        install_interrupt_handler();
    }
    run(args, &INTERRUPT)
}

/// `main` with the stop request `interrupt` (set by Ctrl-C; checked after every slice).
fn run(args: RunArgs, interrupt: &AtomicBool) -> ExitCode {
    let bundle = match Bundle::open(&args.bundle) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("evo-cli: {}: {e}", args.bundle.display());
            return ExitCode::FAILURE;
        }
    };
    // Every input is read before the machine exists: an input error never touches the flash
    // file.
    let nv = match args.flash_file.as_deref().map(read_flash_file) {
        Some(Ok(nv)) => nv,
        Some(Err(())) => return ExitCode::FAILURE,
        None => None,
    };
    let uart_in = match &args.uart_in {
        Some(path) => match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                eprintln!("evo-cli: {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    let state = match &args.load_state {
        Some(path) => match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                eprintln!("evo-cli: {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    let mut machine = match Machine::from_bundle_nv(&bundle, &Layout::EVO_7_1, nv) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("evo-cli: {}: {e}", args.bundle.display());
            return ExitCode::FAILURE;
        }
    };
    // A state that does not fit is an input error too: reported before the run, with the flash
    // file untouched.
    if let (Some(path), Some(bytes)) = (&args.load_state, &state)
        && let Err(e) = machine.load_state(bytes)
    {
        eprintln!("evo-cli: {}: {e}", path.display());
        return ExitCode::FAILURE;
    }
    if let Some(bytes) = &uart_in {
        machine.inject_uart_rx(UartPort::Uart0, bytes);
    }
    let code = run_machine(&mut machine, &args, interrupt);
    if machine.take_power_off() {
        eprintln!("power: off (GPIO pin 1 released)");
    }
    let code = match &args.save_state {
        Some(path) => match write_state(path, &machine.save_state()) {
            Ok(()) => code,
            Err(e) => {
                eprintln!("evo-cli: {}: {e}", path.display());
                ExitCode::FAILURE
            }
        },
        None => code,
    };
    // However the run ended (stop, lockup, GDB session end, a trace cut short, an output
    // error), the flash goes back to the file.
    match &args.flash_file {
        Some(path) => match machine.nv_state().write_file(path) {
            Ok(()) => code,
            Err(e) => {
                eprintln!("evo-cli: {}: {e}", path.display());
                ExitCode::FAILURE
            }
        },
        None => code,
    }
}

/// `--save-state PATH`: written atomically (`write_atomic`: a temporary file next to `path`,
/// synced, then renamed), so a failed write never leaves a half-written state under the final
/// name.
fn write_state(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic(path, bytes)
}

/// `--flash-file PATH`: the saved state, `None` (start erased) if there is no file yet. An
/// unreadable or invalid file is reported and is `Err`; the file is not touched.
pub(crate) fn read_flash_file(path: &Path) -> Result<Option<NvState>, ()> {
    match NvState::read_file(path) {
        Ok(nv) => Ok(Some(nv)),
        Err(NvError::Io(e)) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => {
            eprintln!("evo-cli: {}: {e}", path.display());
            Err(())
        }
    }
}

/// Run the booted machine as `args` asks, report, and return the exit status.
fn run_machine(machine: &mut Machine, args: &RunArgs, interrupt: &AtomicBool) -> ExitCode {
    // Breakpoints compare against the PC, whose bit 0 is always clear.
    machine
        .breakpoints
        .extend(args.breaks.iter().map(|a| a & !1));
    let mut uarts = UartSink {
        print: args.uart,
        ..UartSink::default()
    };
    if let Some(port) = args.gdb {
        return gdb(machine, port, args.unmapped_out.as_deref(), &mut uarts);
    }

    // Written as the run goes, in buffered blocks: a trace can be gigabytes.
    let mut out = BufWriter::new(io::stdout().lock());
    // The first stdout error. It stops the run at the end of the slice, or at once while
    // tracing (e.g. `| head` closed the pipe): nothing more can be printed. The unmapped log is
    // still written.
    let mut stdout_err: Option<io::Error> = None;
    let tracing = args.trace || args.trace_range.is_some();
    let range = args.trace_range.clone().unwrap_or(0..u32::MAX);
    let start = machine.sched.now;
    let end = start.saturating_add(args.max_cycles);
    let mut keys = KeyTimeline::new(&args.keys, start);
    // `--after N`: the cycle to take the screenshot at; `None` once taken (or never asked).
    let mut shot_at = args
        .screenshot
        .as_ref()
        .and(args.after)
        .map(|n| start.saturating_add(n));
    let mut shot: Option<Option<Frame>> = None;
    // Set when Ctrl-C ended the run.
    let mut interrupted = false;
    let stop = loop {
        keys.apply_due(machine);
        if shot_at.is_some_and(|at| machine.sched.now >= at) {
            shot = Some(machine.screen());
            shot_at = None;
        }
        // A slice ends at the budget, after UART_SLICE, at the next key action and at --after.
        let now = machine.sched.now;
        let limit = [Some(end), keys.next_at(), shot_at]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(end);
        let slice = limit.saturating_sub(now).min(UART_SLICE);
        let stop = if tracing {
            machine.run_with(slice, |m| {
                if range.contains(&m.cpu.regs.pc) {
                    stdout_err = writeln!(out, "{}", m.trace_line()).err();
                }
                if stdout_err.is_some() || interrupt.load(Ordering::Relaxed) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            })
        } else {
            machine.run(slice)
        };
        if stdout_err.is_none() {
            stdout_err = uarts.drain(machine, &mut out).err();
        }
        if stdout_err.is_some() {
            break stop;
        }
        if interrupt.load(Ordering::Relaxed) {
            interrupted = true;
            break stop;
        }
        // Asleep with nothing scheduled, but a key press is still to come within the budget:
        // nothing can happen before it, so time moves straight there (no event is due and
        // SysTick is not counting, or the machine would not have reported `Idle`).
        if stop == StopReason::Idle {
            match keys.next_at() {
                Some(at) if at <= end => {
                    machine.sched.now = machine.sched.now.max(at);
                    continue;
                }
                _ => break stop,
            }
        }
        if stop != StopReason::CycleLimit || machine.sched.now >= end {
            break stop;
        }
    };
    if args.screenshot.is_some() && shot.is_none() {
        // Reported only when the run stopped short of --after (not when it reached it at the end).
        if let Some(at) = shot_at.filter(|&at| machine.sched.now < at) {
            eprintln!(
                "screenshot: the run stopped at cycle {} before --after (cycle {at}); taken there",
                machine.sched.now
            );
        }
        shot = Some(machine.screen());
    }
    if stdout_err.is_none() {
        stdout_err = summary(&mut out, machine, stop, interrupted)
            .and_then(|()| out.flush())
            .err();
    }
    uarts.report();

    let mut failed = false;
    if let (Some(path), Some(frame)) = (&args.screenshot, shot) {
        failed |= !write_screenshot(path, frame.as_ref());
    }
    if !write_unmapped(args.unmapped_out.as_deref(), machine) || failed {
        return ExitCode::FAILURE;
    }
    match stdout_err {
        // A closed pipe is not an error worth reporting.
        Some(e) if e.kind() != io::ErrorKind::BrokenPipe => {
            eprintln!("evo-cli: {e}");
            ExitCode::FAILURE
        }
        _ if interrupted => ExitCode::from(EXIT_INTERRUPTED),
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

/// `--screenshot PATH`: write `frame` as an RGB8 PNG; with no frame (the LCD is off) say so and
/// write nothing. False (reported) if the file cannot be written.
fn write_screenshot(path: &Path, frame: Option<&Frame>) -> bool {
    let Some(frame) = frame else {
        eprintln!("screenshot: display is off");
        return true;
    };
    match encode_png(path, frame) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("evo-cli: {}: {e}", path.display());
            false
        }
    }
}

fn encode_png(path: &Path, frame: &Frame) -> Result<(), png::EncodingError> {
    let file = io::BufWriter::new(fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&frame.rgb)?;
    writer.finish()
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

/// The stop line (`interrupted` when Ctrl-C ended the run), then the notes.
fn summary(
    out: &mut impl Write,
    m: &Machine,
    stop: StopReason,
    interrupted: bool,
) -> io::Result<()> {
    let (reason, pc) = match stop {
        _ if interrupted => ("interrupted", m.cpu.regs.pc),
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

    /// Ctrl-C (the stop request set before the run) ends the run after its first slice with
    /// `stop: interrupted` and exit status 130, and the flash file and save state are still
    /// written.
    #[test]
    fn interrupted_run_still_writes_outputs() {
        // Reset handler: `adds r0, #1 ; b <start>`, forever.
        let mut payload = vec![0u8; 0x400];
        payload[0..4].copy_from_slice(&0x3000_0800u32.to_le_bytes());
        payload[4..8].copy_from_slice(&(0x100B_0600u32 | 1).to_le_bytes());
        payload[0x200..0x204].copy_from_slice(&[0x01, 0x30, 0xFD, 0xE7]);
        let dir = std::env::temp_dir().join(format!("evo-cli-interrupt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let bundle = dir.join("loop.84b2");
        fs::write(&bundle, evo_image::testutil::secure_image_bundle(payload)).unwrap();
        let (flash, state) = (dir.join("nv.evoflash"), dir.join("out.state"));
        let args = RunArgs {
            bundle,
            max_cycles: 20 * UART_SLICE,
            trace: false,
            trace_range: None,
            breaks: Vec::new(),
            unmapped_out: None,
            gdb: None,
            uart: false,
            uart_in: None,
            flash_file: Some(flash.clone()),
            load_state: None,
            save_state: Some(state.clone()),
            screenshot: None,
            after: None,
            keys: Vec::new(),
        };
        let code = run(args, &AtomicBool::new(true));
        assert_eq!(code, ExitCode::from(EXIT_INTERRUPTED));
        assert!(NvState::read_file(&flash).is_ok(), "flash file written");
        let saved = fs::read(&state).expect("save state written");
        assert_eq!(&saved[..8], evo_core::STATE_MAGIC);
        let _ = fs::remove_dir_all(&dir);
    }
}
