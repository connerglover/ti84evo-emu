//! Create a disposable, exam-enabled flash for the official-client USB tests.
//! Uses the 7.1 firmware's real boot combo and setup UI; no guest data is patched.
use evo_core::{CPU_HZ, Key, Machine, StopReason};
use evo_image::{Bundle, Layout};
use std::path::PathBuf;

fn run(m: &mut Machine, cycles: u64) {
    assert!(matches!(
        m.run(cycles),
        StopReason::CycleLimit | StopReason::Idle
    ));
}

fn tap(m: &mut Machine, name: &str, settle: u64) {
    let key = Key::from_name(name).unwrap();
    m.set_key(key, true);
    run(m, CPU_HZ / 20);
    m.set_key(key, false);
    run(m, settle);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(debug_assertions) {
        return Err("run this firmware scenario with --release".into());
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: usb_exam_fixture <OS.84b2> <new-disposable.evoflash>".into());
    }
    let output = PathBuf::from(&args[1]);
    if output.exists() {
        return Err("output already exists; choose a new disposable flash path".into());
    }
    let bundle = Bundle::open(args[0].as_ref())?;
    let mut m = Machine::from_bundle(&bundle, &Layout::EVO_7_1)?;
    evo_core::provision(&mut m)?;
    m.power_cycle();
    for name in ["left", "right"] {
        m.set_key(Key::from_name(name).unwrap(), true);
    }
    // Stop after the BSP reads the held combo, solely to release the keys at
    // the correct time. The guest chooses and handles combo 2 itself.
    m.breakpoints.insert(0x0015_D488);
    assert_eq!(m.run(CPU_HZ * 10), StopReason::Breakpoint(0x0015_D488));
    assert_eq!(m.cpu.regs.r[4], 2);
    m.breakpoints.clear();
    for name in ["left", "right"] {
        m.set_key(Key::from_name(name).unwrap(), false);
    }
    run(&mut m, CPU_HZ * 5);
    // ZOOM selects START, then OK on the reset/restrictions summary.
    tap(&mut m, "zoom", CPU_HZ * 5);
    tap(&mut m, "zoom", CPU_HZ);
    m.nv_state().write_file(&output)?;
    println!(
        "Wrote exam setup fixture to {}. Verify p2t with the official client.",
        output.display()
    );
    Ok(())
}
