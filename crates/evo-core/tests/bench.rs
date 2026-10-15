//! Throughput benchmarks (`docs/perf.md`). Ignored by default; run in release:
//!
//! ```text
//! cargo test --release -p evo-core --test bench -- --ignored --nocapture
//! ```
//!
//! `busy_loop` runs a synthetic program and needs nothing else. `boot_to_self_test` boots the
//! user's own bundle from `EVO_OS_IMAGE` (no TI code is part of the repository) and prints a
//! `skipping:` line without it.

use std::time::Instant;

use evo_core::{Machine, StopReason};
use evo_image::testutil::secure_image_bundle;
use evo_image::{Bundle, Layout};

/// The guest clock: real time is this many cycles per second.
const CPU_HZ: f64 = 156e6;

/// `VTOR_S` for `Layout::EVO_7_1`: Secure alias of slot 0x000B_0000 plus the 0x400 header.
const VTOR: u32 = 0x100B_0400;
/// Payload offset of the code, after room for 128 vectors.
const CODE: u32 = 0x200;
/// Secure SRAM word the loop increments.
const COUNTER: u32 = 0x3001_0000;

/// The busy loop: r1 = `COUNTER`, then forever load, add, store, count, shift, xor, branch.
const BUSY_LOOP: [u16; 12] = [
    0x2130, // movs r1, #0x30
    0x0609, // lsls r1, r1, #24
    0x2501, // movs r5, #1
    0x042D, // lsls r5, r5, #16
    0x1949, // adds r1, r1, r5
    // loop:
    0x680A, // ldr r2, [r1]
    0x3201, // adds r2, #1
    0x600A, // str r2, [r1]
    0x3001, // adds r0, #1
    0x0083, // lsls r3, r0, #2
    0x4053, // eors r3, r2
    0xE7F8, // b loop
];

fn busy_machine() -> Machine {
    let mut p = vec![0u8; 0x400];
    p[0..4].copy_from_slice(&0x3000_0800u32.to_le_bytes());
    p[4..8].copy_from_slice(&((VTOR + CODE) | 1).to_le_bytes());
    for (i, hw) in BUSY_LOOP.iter().enumerate() {
        let o = CODE as usize + 2 * i;
        p[o..o + 2].copy_from_slice(&hw.to_le_bytes());
    }
    let bundle = Bundle::from_bytes(&secure_image_bundle(p)).expect("synthetic bundle");
    Machine::from_bundle(&bundle, &Layout::EVO_7_1).expect("boot")
}

/// Print one result line: emulated cycles per host second, and as a share of real time.
fn report(what: &str, cycles: u64, awake: u64, secs: f64) {
    let rate = cycles as f64 / secs;
    let busy = awake as f64 / secs;
    println!(
        "bench {what}: {cycles} cycles ({awake} awake) in {secs:.2} s = {:.1} M cycles/s \
         ({:.0} % of real time), {:.1} M awake cycles/s ({:.0} %)",
        rate / 1e6,
        100.0 * rate / CPU_HZ,
        busy / 1e6,
        100.0 * busy / CPU_HZ,
    );
}

/// Busy guest code: no sleep, no devices, RAM loads and stores from flash-resident code.
#[test]
#[ignore = "benchmark: run with --release -- --ignored --nocapture"]
fn busy_loop() {
    let cycles: u64 = std::env::var("BENCH_CYCLES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000_000);
    let mut m = busy_machine();
    let t = Instant::now();
    let stop = m.run(cycles);
    let secs = t.elapsed().as_secs_f64();
    assert_eq!(stop, StopReason::CycleLimit);
    // The loop really ran: one store per iteration of 7 instructions (8 cycles); r0 counts
    // the same iterations one instruction later.
    let n = m.bus.peek(COUNTER, 4).expect("SRAM");
    assert!(
        n.wrapping_sub(m.cpu.regs.r[0]) <= 1,
        "counter {n}, r0 {}",
        m.cpu.regs.r[0]
    );
    assert!(
        u64::from(n) >= cycles / 9,
        "{n} iterations in {cycles} cycles"
    );
    report("busy_loop", m.sched.now, m.awake_cycles(), secs);
}

/// The real OS from power-on to the Self-Test info screen (shown by 2.5 s of guest time on a
/// fresh unit; `milestones.rs`, M4).
#[test]
#[ignore = "benchmark: run with --release -- --ignored --nocapture"]
fn boot_to_self_test() {
    let Some(path) = std::env::var_os("EVO_OS_IMAGE") else {
        println!("skipping: EVO_OS_IMAGE is not set");
        return;
    };
    let bundle = Bundle::open(path.as_ref()).expect("EVO_OS_IMAGE is a readable .84b2 bundle");
    let mut m = Machine::from_bundle(&bundle, &Layout::EVO_7_1).expect("the bundle boots");
    let cycles = 390_000_000;
    let t = Instant::now();
    let stop = m.run(cycles);
    let secs = t.elapsed().as_secs_f64();
    assert_eq!(stop, StopReason::CycleLimit);
    report("boot_to_self_test", m.sched.now, m.awake_cycles(), secs);
}
