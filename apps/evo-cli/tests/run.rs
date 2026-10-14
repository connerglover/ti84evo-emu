//! `evo-cli run` end to end, on synthetic bundles written to the target's temp directory.

use std::io::Read;
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
