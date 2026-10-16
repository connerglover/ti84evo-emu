//! `evo-cli run` end to end, on synthetic bundles written to the target's temp directory.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use evo_image::testutil::secure_image_bundle;

/// `VTOR_S` for `Layout::EVO_7_1`; the payload starts here (Secure alias).
const VTOR: u32 = 0x100B_0400;
/// Payload offset of the code.
const CODE: u32 = 0x200;

/// A secure-image payload: initial SP, the given vectors (number, payload offset of the
/// handler) and code (payload offset, halfwords).
fn payload(vectors: &[(u32, u32)], code: &[(u32, &[u16])]) -> Vec<u8> {
    let mut p = vec![0u8; 0x400];
    p[0..4].copy_from_slice(&0x3000_0800u32.to_le_bytes());
    for &(n, off) in vectors {
        let v = 4 * n as usize;
        p[v..v + 4].copy_from_slice(&((VTOR + off) | 1).to_le_bytes());
    }
    for &(off, hws) in code {
        for (i, hw) in hws.iter().enumerate() {
            let o = off as usize + 2 * i;
            p[o..o + 2].copy_from_slice(&hw.to_le_bytes());
        }
    }
    p
}

/// Reset handler: `adds r0, #1 ; adds r1, #2 ; subs r2, #1 ; nop ; nop ; b <start>`.
fn looping() -> Vec<u8> {
    payload(
        &[(1, CODE)],
        &[(CODE, &[0x3001, 0x3102, 0x3A01, 0xBF00, 0xBF00, 0xE7F9])],
    )
}

/// Reset handler and HardFault handler are both `udf #0`: the CPU locks up.
fn locking_up() -> Vec<u8> {
    payload(
        &[(1, CODE), (3, 0x210)],
        &[(CODE, &[0xDE00]), (0x210, &[0xDE00])],
    )
}

/// Reset handler: `wfi ; adds r0, #1 ; b <start>`, with no interrupt that could wake it.
fn sleeping() -> Vec<u8> {
    payload(&[(1, CODE)], &[(CODE, &[0xBF30, 0x3001, 0xE7FC])])
}

/// Wait up to `limit` for `child` to exit; kill it and fail if it does not.
fn wait_exit(child: &mut Child, limit: Duration, what: &str) -> ExitStatus {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().expect("wait for evo-cli") {
            return status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("evo-cli still running after {limit:?}: {what}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Write a bundle around `payload` to a temp file named after the test.
fn bundle_file(name: &str, payload: Vec<u8>) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.84b2"));
    std::fs::write(&path, secure_image_bundle(payload)).expect("write bundle");
    path
}

fn evo_cli() -> Command {
    cargo_bin_cmd!("evo-cli")
}

fn stdout(cmd: &mut Command, code: i32) -> String {
    let out = cmd.assert().code(code).get_output().clone();
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

#[test]
fn cli_lockup_exit_code() {
    let bundle = bundle_file("cli_lockup_exit_code", locking_up());
    let out = stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .args(["--max-cycles", "1000000"]),
        2,
    );
    assert!(out.contains("stop: lockup pc=0x100b0610 cycles="), "{out}");
}

/// Core diagnostics (`log::warn!`) reach stderr by default: the undefined instruction is
/// reported with its PC.
#[test]
fn cli_logs_warnings_to_stderr() {
    let bundle = bundle_file("cli_logs_warnings", locking_up());
    let out = evo_cli()
        .env_remove("RUST_LOG")
        .arg("run")
        .arg(&bundle)
        .args(["--max-cycles", "1000"])
        .assert()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("undefined instruction 0xde00 at 0x100b0600"),
        "{stderr}"
    );
    assert!(stderr.contains("lockup"), "{stderr}");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("undefined"),
        "logs go to stderr only"
    );

    // RUST_LOG overrides the default filter.
    let out = evo_cli()
        .env("RUST_LOG", "off")
        .arg("run")
        .arg(&bundle)
        .args(["--max-cycles", "1000"])
        .assert()
        .code(2)
        .get_output()
        .clone();
    assert!(
        out.stderr.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A clean run prints nothing on stderr.
    let bundle = bundle_file("cli_logs_quiet", looping());
    let out = evo_cli()
        .env_remove("RUST_LOG")
        .arg("run")
        .arg(&bundle)
        .args(["--max-cycles", "1000"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    assert!(
        out.stderr.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn cli_cycle_limit_exits_zero_and_traces() {
    let bundle = bundle_file("cli_cycle_limit", looping());
    let out = stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .args(["--max-cycles", "10", "--trace"]),
        0,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "           0 S 100b0600: adds r0, #1", "{out}");
    assert_eq!(lines[1], "           1 S 100b0602: adds r1, #2", "{out}");
    assert_eq!(lines[5], "           5 S 100b060a: b 0x100b0600", "{out}");
    assert_eq!(lines[6], "           7 S 100b0600: adds r0, #1", "{out}");
    assert!(
        out.contains("\nstop: cycle-limit pc=0x100b0606 cycles=10\n"),
        "{out}"
    );
    assert!(out.contains("\nresets: 0 "), "{out}");
}

#[test]
fn cli_trace_range_limits_the_trace() {
    let bundle = bundle_file("cli_trace_range", looping());
    let out = stdout(
        evo_cli().arg("run").arg(&bundle).args([
            "--max-cycles",
            "21",
            "--trace-range",
            "0x100b0604..100b0606",
        ]),
        0,
    );
    // Trace lines start with the right-aligned cycle count.
    let traced: Vec<&str> = out.lines().filter(|l| l.starts_with(' ')).collect();
    assert_eq!(
        traced,
        [
            "           2 S 100b0604: subs r2, #1",
            "           9 S 100b0604: subs r2, #1",
            "          16 S 100b0604: subs r2, #1",
        ],
        "{out}"
    );
}

#[test]
fn cli_breakpoint_exits_zero() {
    let bundle = bundle_file("cli_breakpoint", looping());
    let out = stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .args(["--break", "0x100b0605", "--break", "0x200"]),
        0,
    );
    // The Thumb bit of a breakpoint address is ignored.
    assert!(
        out.contains("stop: breakpoint pc=0x100b0604 cycles=2"),
        "{out}"
    );
}

/// A guest asleep with nothing to wake it stops the run at once, as `idle`, not `cycle-limit`.
#[test]
fn cli_idle_guest_stops_as_idle() {
    let bundle = bundle_file("cli_idle", sleeping());
    let out = stdout(evo_cli().arg("run").arg(&bundle), 0);
    assert!(out.contains("stop: idle pc=0x100b0602 cycles=1\n"), "{out}");
    assert!(out.contains("\nidle: "), "{out}");
}

/// Once stdout is closed (`| head`), tracing stops the run instead of running out the budget.
#[test]
fn cli_trace_stops_when_stdout_closes() {
    let bundle = bundle_file("cli_trace_closed_stdout", looping());
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_evo-cli"))
        .arg("run")
        .arg(&bundle)
        .args(["--max-cycles", "1_000_000_000_000", "--trace"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn evo-cli");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut first = [0u8; 4096];
    stdout.read_exact(&mut first).expect("trace output");
    drop(stdout);
    // Tracing 10^12 cycles takes hours.
    let status = wait_exit(&mut child, Duration::from_secs(30), "stdout closed");
    assert_eq!(status.code(), Some(0));
}

/// Send one RSP packet in ack mode and return the reply's body (no run-length decoding).
fn rsp(stream: &mut TcpStream, body: &str) -> String {
    let sum = body.bytes().fold(0u8, |a, b| a.wrapping_add(b));
    write!(stream, "${body}#{sum:02x}").expect("send");
    let mut reply = Vec::new();
    let mut byte = [0u8];
    while !(reply.len() > 3 && reply[reply.len() - 3] == b'#') {
        stream.read_exact(&mut byte).expect("receive");
        reply.push(byte[0]);
    }
    stream.write_all(b"+").expect("ack");
    let reply = String::from_utf8(reply).expect("ASCII");
    let reply = reply.strip_prefix("+$").expect("ack, then the reply");
    reply[..reply.len() - 3].to_string()
}

/// `--gdb 0` serves on a free localhost port, which it reports on stderr; after the client
/// detaches, the run ends with a summary.
#[test]
fn cli_gdb_serves_one_client_on_localhost() {
    let bundle = bundle_file("cli_gdb", looping());
    let flash = flash_file("cli_gdb");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_evo-cli"))
        .arg("run")
        .arg(&bundle)
        .args(["--gdb", "0", "--break", "0x100b0604", "--flash-file"])
        .arg(&flash)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn evo-cli");
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr"));
    let mut line = String::new();
    stderr.read_line(&mut line).expect("listening line");
    let addr: SocketAddr = line
        .split_whitespace()
        .find_map(|word| word.parse().ok())
        .unwrap_or_else(|| panic!("no address in {line:?}"));
    assert!(addr.ip().is_loopback(), "{line}");

    let mut gdb = TcpStream::connect(addr).expect("connect");
    gdb.set_read_timeout(Some(Duration::from_secs(20)))
        .expect("timeout");
    let stop = rsp(&mut gdb, "c");
    assert!(
        stop.starts_with("T05") && stop.contains("swbreak"),
        "{stop}"
    );
    assert_eq!(rsp(&mut gdb, "D"), "OK");

    let status = wait_exit(&mut child, Duration::from_secs(20), "after detach");
    assert_eq!(status.code(), Some(0));
    let mut out = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut out)
        .expect("stdout");
    assert!(
        out.starts_with("gdb: session ended pc=0x100b0604 cycles=2\n"),
        "{out}"
    );
    assert!(out.contains("\nunmapped: 0 distinct accesses"), "{out}");
    // The end of the session writes the flash file back.
    assert_eq!(
        std::fs::metadata(&flash).expect("written").len(),
        FLASH_FILE_LEN
    );
}

#[test]
fn cli_writes_unmapped_log() {
    let bundle = bundle_file("cli_unmapped_out", locking_up());
    let md = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli_unmapped_out.md");
    let _ = std::fs::remove_file(&md);
    let out = stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .arg("--unmapped-out")
            .arg(&md),
        2,
    );
    assert!(out.contains("unmapped: 0 distinct accesses"), "{out}");
    let table = std::fs::read_to_string(&md).expect("unmapped log written");
    assert!(table.starts_with("| addr | width | kind |"), "{table}");
}

/// Exit status 2 means lockup, so a usage error must not use clap's default of 2.
#[test]
fn cli_usage_errors_exit_one() {
    let out = evo_cli()
        .args(["run", "x.84b2", "--break", "zz"])
        .assert()
        .code(1);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(stderr.contains("--break"), "{stderr}");
    evo_cli().arg("run").assert().code(1);
    evo_cli()
        .args(["run", "x.84b2", "--no-such-flag"])
        .assert()
        .code(1);
    evo_cli().assert().code(1);

    // Help and version are not errors.
    let out = evo_cli().args(["run", "--help"]).assert().code(0);
    let help = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(help.contains("--max-cycles"), "{help}");
    evo_cli().arg("--version").assert().code(0);
}

#[test]
fn cli_bad_bundle_exits_one() {
    let missing = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("no-such-bundle.84b2");
    let out = evo_cli().arg("run").arg(&missing).assert().code(1);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(stderr.contains("no-such-bundle.84b2"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");

    let corrupt = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("corrupt.84b2");
    std::fs::write(&corrupt, b"PK\x03\x04 not really a zip").expect("write");
    let out = evo_cli().arg("run").arg(&corrupt).assert().code(1);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(!stderr.contains("panicked"), "{stderr}");
}

/// Payload offset of a program's literal pool.
const LIT: u32 = 0x300;

/// `ldr r0, [pc, #..]` at payload offset `at`, loading the word at [`LIT`].
fn ldr_r0_lit(at: u32) -> u16 {
    0x4800 | ((LIT - ((at + 4) & !3)) / 4) as u16
}

/// Reset handler: `ldr r0, =uart ; movs r1, #a ; str r1, [r0] ; movs r1, #b ; str r1, [r0] ;
/// b .` (THR is at +0).
fn writes_two(uart: u32, a: u8, b: u8) -> Vec<u8> {
    payload(
        &[(1, CODE)],
        &[
            (
                CODE,
                &[
                    ldr_r0_lit(CODE),
                    0x2100 | u16::from(a),
                    0x6001,
                    0x2100 | u16::from(b),
                    0x6001,
                    0xE7FE,
                ],
            ),
            (LIT, &[uart as u16, (uart >> 16) as u16]),
        ],
    )
}

/// Reset handler: wait for an RX byte on UART0 (LSR bit 0), then echo it to THR; `b .`.
/// `ldr r0, =uart0 ; 1: ldr r1, [r0, #20] ; lsrs r1, r1, #1 ; bcc 1b ; ldr r2, [r0] ;
/// str r2, [r0] ; b .`
fn echoes_one() -> Vec<u8> {
    payload(
        &[(1, CODE)],
        &[
            (
                CODE,
                &[
                    ldr_r0_lit(CODE),
                    0x6941,
                    0x0849,
                    0xD3FC,
                    0x6802,
                    0x6002,
                    0xE7FE,
                ],
            ),
            (LIT, &[0x6000, 0x4010]),
        ],
    )
}

/// `--uart` streams UART0 TX to stdout, raw, ahead of the summary.
#[test]
fn cli_uart_streams_to_stdout() {
    let bundle = bundle_file("cli_uart_streams", writes_two(0x4010_6000, b'h', b'i'));
    let out = stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .args(["--max-cycles", "10000", "--uart"]),
        0,
    );
    let hi = out
        .find("hi")
        .unwrap_or_else(|| panic!("no UART bytes: {out}"));
    let stop = out.find("stop: cycle-limit").expect("stop line");
    assert!(hi < stop, "{out}");
}

/// `--uart-in FILE` feeds the file's bytes into UART0 RX at cycle 0.
#[test]
fn cli_uart_in_feeds_rx() {
    let bundle = bundle_file("cli_uart_in", echoes_one());
    let input = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli_uart_in.bin");
    std::fs::write(&input, b"Z").expect("write input");
    let out = stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .args(["--max-cycles", "10000", "--uart", "--uart-in"])
            .arg(&input),
        0,
    );
    assert!(out.starts_with('Z'), "{out}");

    // Without input the guest waits, and prints nothing.
    let out = stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .args(["--max-cycles", "10000", "--uart"]),
        0,
    );
    assert!(out.starts_with("stop: "), "{out}");

    // A missing input file is an error.
    let missing = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("no-such-uart-in.bin");
    let out = evo_cli()
        .arg("run")
        .arg(&bundle)
        .arg("--uart-in")
        .arg(&missing)
        .assert()
        .code(1);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(stderr.contains("no-such-uart-in.bin"), "{stderr}");
}

/// Without `--uart`, UART0 bytes are discarded: stdout is only the summary.
#[test]
fn cli_without_uart_prints_no_guest_bytes() {
    let bundle = bundle_file("cli_without_uart", writes_two(0x4010_6000, b'h', b'i'));
    let out = evo_cli()
        .env_remove("RUST_LOG")
        .arg("run")
        .arg(&bundle)
        .args(["--max-cycles", "10000"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.starts_with("stop: cycle-limit"), "{stdout}");
    assert!(
        out.stderr.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Traffic on UART2 (the link port) is counted and reported on stderr.
#[test]
fn cli_reports_uart2_bytes() {
    let bundle = bundle_file("cli_uart2", writes_two(0x4010_B000, b'a', b'b'));
    let out = evo_cli()
        .arg("run")
        .arg(&bundle)
        .args(["--max-cycles", "10000", "--uart"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("uart2: 2 bytes"), "{stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        stdout.starts_with("stop: "),
        "UART2 bytes are not printed: {stdout}"
    );
}

/// A budget of several UART slices still stops right at the budget, like one long run.
#[test]
fn cli_budget_spans_slices() {
    let bundle = bundle_file("cli_budget_spans_slices", looping());
    let out = stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .args(["--max-cycles", "5_000_000", "--uart"]),
        0,
    );
    let cycles: u64 = out
        .lines()
        .find_map(|l| l.strip_prefix("stop: cycle-limit pc="))
        .and_then(|l| l.split("cycles=").nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("no stop line: {out}"));
    // The loop's longest instruction (a taken branch) takes 2 cycles.
    assert!((5_000_000..5_000_002).contains(&cycles), "{out}");
}

/// Size of a flash file: magic, version, flash length, 16 MiB of flash, 768 secreg bytes.
const FLASH_FILE_LEN: u64 = 8 + 4 + 4 + (16 << 20) + 768;

/// A fresh path for a flash file named after the test (any earlier one removed).
fn flash_file(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.evoflash"));
    let _ = std::fs::remove_file(&path);
    path
}

#[test]
fn cli_flash_file_created_and_reused() {
    let bundle = bundle_file("cli_flash_file_created", looping());
    let flash = flash_file("cli_flash_file_created");
    let run = || {
        stdout(
            evo_cli()
                .arg("run")
                .arg(&bundle)
                .args(["--max-cycles", "1000", "--flash-file"])
                .arg(&flash),
            0,
        )
    };
    run();
    assert_eq!(
        std::fs::metadata(&flash).expect("created").len(),
        FLASH_FILE_LEN
    );

    // Mark a byte outside the image slots (flash offset 0x9_0000): run 2 keeps it.
    let mut bytes = std::fs::read(&flash).expect("read");
    assert_eq!(&bytes[..8], b"EVOFLASH");
    assert_eq!(bytes[16 + 0x9_0000], 0xFF);
    bytes[16 + 0x9_0000] = 0xA5;
    std::fs::write(&flash, &bytes).expect("write");
    run();
    let bytes = std::fs::read(&flash).expect("read");
    assert_eq!(bytes.len() as u64, FLASH_FILE_LEN);
    assert_eq!(bytes[16 + 0x9_0000], 0xA5);
    assert!(!flash.with_extension("evoflash.tmp").exists());
}

#[test]
fn cli_executes_stored_firmware_even_without_its_original_bundle() {
    let bundle = bundle_file("cli_stored_firmware", looping());
    let flash = flash_file("cli_stored_firmware");
    let run = || {
        stdout(
            evo_cli()
                .arg("run")
                .arg(&bundle)
                .args(["--max-cycles", "1", "--trace", "--flash-file"])
                .arg(&flash),
            0,
        )
    };
    run();
    let mut bytes = std::fs::read(&flash).unwrap();
    // Replace the first stored instruction with movs r0,#9. Opening the old bundle
    // again must not restore adds r0,#1 over it.
    let code = 16 + 0xB_0600;
    bytes[code..code + 2].copy_from_slice(&0x2009u16.to_le_bytes());
    std::fs::write(&flash, &bytes).unwrap();
    assert!(run().contains("movs r0, #9"));
    std::fs::remove_file(&bundle).unwrap();
    assert!(run().contains("movs r0, #9"));
    assert_eq!(
        &std::fs::read(&flash).unwrap()[code..code + 2],
        &0x2009u16.to_le_bytes()
    );
}

#[test]
fn cli_state_from_fresh_flash_loads_after_reopen() {
    let bundle = bundle_file("cli_state_fresh_flash", looping());
    let flash = flash_file("cli_state_fresh_flash");
    let state = flash.with_extension("state");
    let first = stop_cycles(&stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .args(["--max-cycles", "1000", "--flash-file"])
            .arg(&flash)
            .arg("--save-state")
            .arg(&state),
        0,
    ));
    std::fs::remove_file(&bundle).unwrap();
    let second = stop_cycles(&stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .args(["--max-cycles", "1000", "--flash-file"])
            .arg(&flash)
            .arg("--load-state")
            .arg(&state),
        0,
    ));
    assert!((first + 1000..=first + 1010).contains(&second));
}

#[test]
fn cli_invalid_stored_firmware_is_not_reinstalled() {
    let bundle = bundle_file("cli_invalid_stored_firmware", looping());
    let flash = flash_file("cli_invalid_stored_firmware");
    evo_core::NvState::erased().write_file(&flash).unwrap();
    let before = std::fs::read(&flash).unwrap();
    let out = evo_cli()
        .arg("run")
        .arg(&bundle)
        .args(["--max-cycles", "1", "--flash-file"])
        .arg(&flash)
        .assert()
        .code(1)
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid persisted boot image"));
    assert_eq!(std::fs::read(&flash).unwrap(), before);
}

/// Review Focus 2: a corrupt flash file is an error that leaves the file as it was.
#[test]
fn cli_bad_flash_file_exits_1_and_keeps_file() {
    let bundle = bundle_file("cli_bad_flash_file", looping());
    let flash = flash_file("cli_bad_flash_file");
    let junk: Vec<u8> = (0..100u8).collect();
    std::fs::write(&flash, &junk).expect("write");
    let out = evo_cli()
        .arg("run")
        .arg(&bundle)
        .args(["--max-cycles", "1000", "--flash-file"])
        .arg(&flash)
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("cli_bad_flash_file.evoflash"), "{stderr}");
    assert!(out.stdout.is_empty(), "the run never started");
    assert_eq!(std::fs::read(&flash).expect("read"), junk);

    // Truncated after a valid header: also refused, also untouched.
    let mut short = b"EVOFLASH".to_vec();
    short.extend_from_slice(&1u32.to_le_bytes());
    short.extend_from_slice(&(16u32 << 20).to_le_bytes());
    short.extend_from_slice(&[0xFF; 64]);
    std::fs::write(&flash, &short).expect("write");
    evo_cli()
        .arg("run")
        .arg(&bundle)
        .args(["--max-cycles", "1000", "--flash-file"])
        .arg(&flash)
        .assert()
        .code(1);
    assert_eq!(std::fs::read(&flash).expect("read"), short);
}

#[test]
fn cli_flash_file_written_after_lockup() {
    let bundle = bundle_file("cli_flash_file_lockup", locking_up());
    let flash = flash_file("cli_flash_file_lockup");
    evo_cli()
        .arg("run")
        .arg(&bundle)
        .args(["--max-cycles", "1000000", "--flash-file"])
        .arg(&flash)
        .assert()
        .code(2);
    assert_eq!(
        std::fs::metadata(&flash).expect("written").len(),
        FLASH_FILE_LEN
    );
}

/// A flash file that cannot be written back is reported (exit 1), naming the path.
#[test]
fn cli_flash_file_write_failure_exits_1() {
    let bundle = bundle_file("cli_flash_file_unwritable", looping());
    // A directory that does not exist: nothing to read (erased start), nothing can be written.
    let flash = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("no-such-dir")
        .join("x.evoflash");
    let out = evo_cli()
        .arg("run")
        .arg(&bundle)
        .args(["--max-cycles", "1000", "--flash-file"])
        .arg(&flash)
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("x.evoflash"), "{stderr}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("stop: cycle-limit"),
        "the run itself completed"
    );
}

/// The cycle count of the `stop: cycle-limit` line.
fn stop_cycles(out: &str) -> u64 {
    out.lines()
        .find_map(|l| l.strip_prefix("stop: cycle-limit pc="))
        .and_then(|l| l.split("cycles=").nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("no stop line: {out}"))
}

/// A run saved with `--save-state` resumes with `--load-state`: the second run's budget
/// counts from where the first one stopped.
#[test]
fn cli_save_then_load_state() {
    let bundle = bundle_file("cli_save_then_load_state", looping());
    let state = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli_save_then_load_state.state");
    let _ = std::fs::remove_file(&state);
    let c1 = stop_cycles(&stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .args(["--max-cycles", "1000", "--save-state"])
            .arg(&state),
        0,
    ));
    assert!(state.exists(), "state written");
    let c2 = stop_cycles(&stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .args(["--max-cycles", "1000", "--load-state"])
            .arg(&state),
        0,
    ));
    // The overshoot bound covers the longest instruction.
    assert!((c1 + 1000..=c1 + 1010).contains(&c2), "c1={c1} c2={c2}");
}

/// A state file that is not a save state is reported (exit 1, naming the path) before the run
/// starts, and the flash file is not touched.
#[test]
fn cli_bad_state_file_exits_1() {
    let bundle = bundle_file("cli_bad_state_file", looping());
    let state = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli_bad_state_file.state");
    std::fs::write(&state, b"not a save state").expect("write");
    let flash = flash_file("cli_bad_state_file");
    let out = evo_cli()
        .arg("run")
        .arg(&bundle)
        .args(["--max-cycles", "1000", "--flash-file"])
        .arg(&flash)
        .arg("--load-state")
        .arg(&state)
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("cli_bad_state_file.state"), "{stderr}");
    assert!(out.stdout.is_empty(), "the run never started");
    assert!(!flash.exists(), "flash file not written");
}

/// A guest that drops the power hold (GPIO pin 1) is reported on stderr after the run.
#[test]
fn cli_reports_power_off() {
    // ldr r0, =0x4010_5000 ; movs r1, #2 ; str r1, [r0, #8] (DIR) ; str r1, [r0, #0x10] (SET) ;
    // str r1, [r0, #0x14] (CLR) ; b .
    let p = payload(
        &[(1, CODE)],
        &[
            (CODE, &[0x483F, 0x2102, 0x6081, 0x6101, 0x6141, 0xE7FE]),
            (0x300, &[0x5000, 0x4010]),
        ],
    );
    let bundle = bundle_file("cli_power_off", p);
    let out = evo_cli()
        .env_remove("RUST_LOG")
        .arg("run")
        .arg(&bundle)
        .args(["--max-cycles", "1000"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("power: off (GPIO pin 1 released)\n"),
        "{stderr}"
    );
}

/// `ldr rt, [pc, #..]` at payload offset `at`, loading the word at payload offset `lit`.
fn ldr_lit(rt: u16, at: u32, lit: u32) -> u16 {
    0x4800 | (rt << 8) | ((lit - ((at + 4) & !3)) / 4) as u16
}

/// Bundle A: program the LCDC (0x4000_4000) with the 320×240 timing words and UPBASE
/// 0x2004_8000, wait ~2_000 cycles, fill the 240×320 RGB565 buffer with 0x07E0 (green, through
/// the Secure alias 0x3004_8000), then turn the LCD on (CONTROL 0x392D: enable, 16 bpp 565,
/// BGR, power) and loop.
fn draws_green() -> Vec<u8> {
    let c = CODE;
    let lit = |i: u32| LIT + 4 * i;
    let code = [
        ldr_lit(0, c, lit(0)),      // r0 = LCDC
        ldr_lit(1, c + 2, lit(1)),  // TIM0
        0x6001,                     // str r1, [r0, #0]
        ldr_lit(1, c + 6, lit(2)),  // TIM1
        0x6041,                     // str r1, [r0, #4]
        ldr_lit(1, c + 10, lit(3)), // UPBASE
        0x6101,                     // str r1, [r0, #0x10]
        ldr_lit(2, c + 14, lit(4)), // delay count
        0x3A01,                     // 1: subs r2, #1
        0xD1FD,                     // bne 1b
        ldr_lit(1, c + 20, lit(5)), // buffer (Secure alias)
        ldr_lit(2, c + 22, lit(6)), // two green pixels
        ldr_lit(3, c + 24, lit(7)), // word count
        0x600A,                     // 2: str r2, [r1]
        0x3104,                     // adds r1, #4
        0x3B01,                     // subs r3, #1
        0xD1FB,                     // bne 2b
        ldr_lit(1, c + 34, lit(8)), // CONTROL
        0x6181,                     // str r1, [r0, #0x18]
        0xE7FE,                     // b .
    ];
    let words: [u32; 9] = [
        0x4000_4000,
        0x0A0A_0A38,
        0x042D_093F,
        0x2004_8000,
        667,
        0x3004_8000,
        0x07E0_07E0,
        240 * 320 / 2,
        0x392D,
    ];
    let pool: Vec<u16> = words
        .iter()
        .flat_map(|w| [*w as u16, (w >> 16) as u16])
        .collect();
    payload(&[(1, CODE)], &[(CODE, &code), (LIT, &pool)])
}

/// Bundle B: enable IRQ 0 (the ON key) and loop; the IRQ 0 handler writes `K` to UART0.
fn on_writes_k() -> Vec<u8> {
    const HANDLER: u32 = 0x280;
    payload(
        &[(1, CODE), (16, HANDLER)],
        &[
            // ldr r0, =NVIC_ISER0 ; movs r1, #1 ; str r1, [r0] ; b .
            (CODE, &[ldr_lit(0, CODE, LIT), 0x2101, 0x6001, 0xE7FE]),
            // ldr r0, =UART0 ; movs r1, #'K' ; str r1, [r0] ; bx lr
            (
                HANDLER,
                &[ldr_lit(0, HANDLER, LIT + 4), 0x214B, 0x6001, 0x4770],
            ),
            (LIT, &[0xE100, 0xE000, 0x6000, 0x4010]),
        ],
    )
}

/// A fresh path for a PNG named after the test (any earlier one removed).
fn png_file(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.png"));
    let _ = std::fs::remove_file(&path);
    path
}

#[test]
fn cli_screenshot_png() {
    let bundle = bundle_file("cli_screenshot_png", draws_green());
    let shot = png_file("cli_screenshot_png");
    let out = stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .args(["--max-cycles", "1_000_000", "--screenshot"])
            .arg(&shot),
        0,
    );
    assert!(out.contains("stop: cycle-limit"), "{out}");
    let decoder = png::Decoder::new(std::fs::File::open(&shot).expect("PNG written"));
    let mut reader = decoder.read_info().expect("PNG header");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("PNG data");
    assert_eq!((info.width, info.height), (320, 240));
    assert_eq!(info.color_type, png::ColorType::Rgb);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    let rgb = &buf[..info.buffer_size()];
    assert_eq!(rgb.len(), 320 * 240 * 3);
    assert!(
        rgb.as_chunks::<3>().0.iter().all(|p| *p == [0, 255, 0]),
        "every pixel is green"
    );
}

/// `--after N` takes the screenshot at cycle N: before the program turns the LCD on there is
/// nothing to show, which is reported, writes no file and keeps the exit status.
#[test]
fn cli_screenshot_after() {
    let bundle = bundle_file("cli_screenshot_after", draws_green());
    let shot = png_file("cli_screenshot_after");
    let out = evo_cli()
        .arg("run")
        .arg(&bundle)
        .args([
            "--max-cycles",
            "1_000_000",
            "--after",
            "1000",
            "--screenshot",
        ])
        .arg(&shot)
        .assert()
        .code(0)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("screenshot: display is off"), "{stderr}");
    assert!(!shot.exists(), "no file for a dark screen");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        stdout.contains("stop: cycle-limit"),
        "the run went on: {stdout}"
    );
}

/// `--keys on` taps the ON key at the start of the run; the guest's IRQ 0 handler sees it.
#[test]
fn cli_keys_on_reaches_guest() {
    let bundle = bundle_file("cli_keys_on", on_writes_k());
    let out = stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .args(["--max-cycles", "10000", "--keys", "on", "--uart"]),
        0,
    );
    assert!(out.starts_with('K'), "{out}");

    // Without the key, nothing.
    let out = stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .args(["--max-cycles", "10000", "--uart"]),
        0,
    );
    assert!(out.starts_with("stop: "), "{out}");
}

/// A bad key script is a usage error, reported before the bundle is opened.
#[test]
fn cli_bad_key_script_exits_1() {
    let missing = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("no-such-keys-bundle.84b2");
    let out = evo_cli()
        .arg("run")
        .arg(&missing)
        .args(["--keys", "r9c9"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("r9c9"), "{stderr}");
    assert!(
        !stderr.contains("no-such-keys-bundle"),
        "the bundle is never opened: {stderr}"
    );
    assert!(out.stdout.is_empty());
}

#[test]
fn cli_after_requires_screenshot() {
    let bundle = bundle_file("cli_after_requires_screenshot", looping());
    let out = evo_cli()
        .arg("run")
        .arg(&bundle)
        .args(["--after", "1000"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("--screenshot"), "{stderr}");
    assert!(out.stdout.is_empty());
}

/// A guest asleep with nothing scheduled still gets a key press that comes later: time moves
/// to the press, the ON key wakes it, and the run ends as `idle` once nothing is left.
#[test]
fn cli_keys_wake_idle_guest() {
    const HANDLER: u32 = 0x280;
    // ldr r0, =NVIC_ISER0 ; movs r1, #1 ; str r1, [r0] ; 1: wfi ; b 1b
    let p = payload(
        &[(1, CODE), (16, HANDLER)],
        &[
            (
                CODE,
                &[ldr_lit(0, CODE, LIT), 0x2101, 0x6001, 0xBF30, 0xE7FD],
            ),
            (
                HANDLER,
                &[ldr_lit(0, HANDLER, LIT + 4), 0x214B, 0x6001, 0x4770],
            ),
            (LIT, &[0xE100, 0xE000, 0x6000, 0x4010]),
        ],
    );
    let bundle = bundle_file("cli_keys_wake_idle", p);
    let out = stdout(
        evo_cli()
            .arg("run")
            .arg(&bundle)
            .args(["--keys", "wait=100 on", "--uart"]),
        0,
    );
    assert!(out.starts_with("Kstop: idle "), "{out}");
    let cycles: u64 = out
        .split("cycles=")
        .nth(1)
        .and_then(|c| c.split_whitespace().next())
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("no cycles: {out}"));
    // The press comes at 100 ms; the release at 150 ms is applied before the run ends.
    assert!(cycles >= 150 * evo_core::CPU_HZ / 1000, "{out}");
}

/// `--save-state` without `--flash-file` notes on stderr that loading the state later needs
/// the same flash (the flash is not in a save state); with `--flash-file` there is no note.
#[test]
fn cli_save_state_without_flash_file_notes_it() {
    let bundle = bundle_file("cli_save_state_note", looping());
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let state = dir.join("cli_save_state_note.state");
    let stderr = |cmd: &mut Command| {
        let out = cmd.assert().code(0).get_output().clone();
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let without = stderr(
        evo_cli()
            .env_remove("RUST_LOG")
            .arg("run")
            .arg(&bundle)
            .args(["--max-cycles", "1000", "--save-state"])
            .arg(&state),
    );
    assert!(
        without.contains("note:") && without.contains("--flash-file"),
        "{without}"
    );
    let flash = flash_file("cli_save_state_note");
    let with = stderr(
        evo_cli()
            .env_remove("RUST_LOG")
            .arg("run")
            .arg(&bundle)
            .args(["--max-cycles", "1000", "--save-state"])
            .arg(&state)
            .arg("--flash-file")
            .arg(&flash),
    );
    assert!(!with.contains("note:"), "{with}");
    let _ = std::fs::remove_file(&state);
    let _ = std::fs::remove_file(&flash);
}
