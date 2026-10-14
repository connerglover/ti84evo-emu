//! Boots the real TI-84 Evo OS. It reads the user's own bundle from `EVO_OS_IMAGE` and is
//! skipped (with a printed note) when that is unset; no TI code is part of the repository.
//!
//! What the OS does once booted is recorded in `docs/hardware/discovery-log.md`.

use evo_core::{Machine, StopReason};
use evo_image::{Bundle, Layout};

/// The OS must run 10M cycles without the CPU locking up. Without its peripherals modeled it
/// does not get far: any stop except a lockup passes (R22). A lockup prints the last 64
/// instructions executed.
#[test]
fn real_os_runs_without_lockup_for_10m_cycles() {
    let Some(path) = std::env::var_os("EVO_OS_IMAGE") else {
        eprintln!("skipping real_os_runs_without_lockup_for_10m_cycles: EVO_OS_IMAGE is not set");
        return;
    };
    let bundle =
        Bundle::open(path.as_ref()).expect("EVO_OS_IMAGE should be a readable .84b2 bundle");
    let mut m = Machine::from_bundle(&bundle, &Layout::EVO_7_1).expect("the bundle boots");

    let stop = m.run(10_000_000);

    if let StopReason::Lockup { pc } = stop {
        panic!(
            "the CPU locked up at {pc:#010x} after {} cycles ({} system resets); last \
             instructions executed:\n{}",
            m.sched.now,
            m.resets,
            m.recent_trace().join("\n")
        );
    }
    assert!(
        matches!(
            stop,
            StopReason::CycleLimit | StopReason::Breakpoint(_) | StopReason::Idle
        ),
        "unexpected stop {stop:?} at pc {:#010x}",
        m.cpu.regs.pc
    );
}
