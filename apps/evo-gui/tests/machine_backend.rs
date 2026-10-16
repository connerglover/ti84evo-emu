//! `MachineBackend` on a synthetic secure image (and on the real OS when `EVO_OS_IMAGE` is set).

mod common;

use common::TempDir;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use evo_core::{Key, NvState};
use evo_gui::backend::machine::{
    MachineBackend, OpenError, core_key, fnv1a64, inspect_bundle, rgb888_to_rgba,
};
use evo_gui::backend::{
    AccessDir, BackendError, EmuBackend, Goal, RunStatus, WatchKinds, Watchpoint,
};
use evo_gui::keypad::KeyId;
use evo_hw::dev::keypad::KEY_NAMES;

/// The counter program at 0x100B_0600:
/// `movs r2,#0x40; lsls r2,r2,#24; ldr r1,[r2]; adds r0,#1; b.n` back to `adds`.
const COUNTER: [u16; 5] = [0x2240, 0x0612, 0x6811, 0x3001, 0xE7FD];

fn bundle_bytes(code: &[u16]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0x3000_0800u32.to_le_bytes());
    body.extend_from_slice(&(0x100B_0600u32 | 1).to_le_bytes());
    body.resize(0x200, 0);
    for hw in code {
        body.extend_from_slice(&hw.to_le_bytes());
    }
    evo_image::testutil::secure_image_bundle(body)
}

/// A fresh, empty directory for one test's flash files, removed when the guard drops.
fn nv_dir() -> TempDir {
    TempDir::new("evo-gui-tests-nv", true)
}

fn backend(code: &[u16]) -> MachineBackend {
    MachineBackend::from_bytes(&bundle_bytes(code), "test.84b2".into(), &nv_dir()).unwrap()
}

/// Where `MachineBackend` keeps the flash file of `bundle` under `dir`.
fn nv_path_for(dir: &Path, bundle: &[u8]) -> PathBuf {
    dir.join(format!("{:016x}.nv", fnv1a64(bundle)))
}

#[test]
fn runs_and_reports_registers() {
    let mut b = backend(&COUNTER);
    assert_eq!(b.name(), "test.84b2");
    assert_eq!(b.run_for(1000), RunStatus::Running);
    let cpu = b.cpu();
    assert!(cpu.core[0] > 0, "r0 = {}", cpu.core[0]);
    assert!(cpu.secure);
    assert!(!cpu.handler);
    assert_eq!(cpu.core[13], 0x3000_0800);
    assert!(
        cpu.banked
            .iter()
            .any(|&(n, v)| n == "msp_s" && v == 0x3000_0800)
    );
    assert_eq!(cpu.banked.len(), 16);
    assert!(b.cycles() >= 1000);
}

#[test]
fn disassembles_at_pc() {
    let b = backend(&COUNTER);
    let lines = b.disasm(0x100B_0600, 4);
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0].addr, 0x100B_0600);
    assert_eq!(lines[0].len, 2);
    assert_eq!(lines[0].raw, 0x2240);
    assert_eq!(lines[3].addr, 0x100B_0606);
    assert_eq!(lines[3].text, "adds r0, #1");
}

#[test]
fn disasm_stops_at_unreadable_memory() {
    let b = backend(&COUNTER);
    assert!(b.disasm(0x4000_0000, 4).is_empty());
}

#[test]
fn breakpoint_stops() {
    let mut b = backend(&COUNTER);
    b.toggle_breakpoint(0x100B_0608);
    assert_eq!(b.breakpoints(), vec![0x100B_0608]);
    assert_eq!(b.run_for(1000), RunStatus::Breakpoint(0x100B_0608));
    b.toggle_breakpoint(0x100B_0608);
    assert!(b.breakpoints().is_empty());
}

#[test]
fn peek_reads_flash_and_none_when_unmapped() {
    let b = backend(&COUNTER);
    assert_eq!(
        b.peek(0x100B_0400, 4),
        vec![Some(0x00), Some(0x08), Some(0x00), Some(0x30)]
    );
    assert_eq!(b.peek(0x4000_0000, 1), vec![None]);
}

#[test]
fn unmapped_log_is_exposed() {
    let mut b = backend(&COUNTER);
    b.run_for(1000);
    let rows = b.unmapped();
    assert!(
        rows.iter().any(|r| r.addr == 0x4000_0000
            && r.width == 4
            && r.kind == "read"
            && r.value.is_none()
            && r.pc == 0x100B_0604
            && r.secure
            && r.count >= 1),
        "{rows:?}"
    );
}

#[test]
fn step_runs_one_instruction() {
    let mut b = backend(&COUNTER);
    assert_eq!(b.step(), RunStatus::Running);
    assert_eq!(b.cpu().core[15], 0x100B_0602);
}

#[test]
fn reset_rebuilds() {
    let mut b = backend(&COUNTER);
    b.toggle_breakpoint(0x100B_0700);
    b.run_for(1000);
    assert!(b.cycles() > 0);
    b.reset();
    assert_eq!(b.cycles(), 0);
    assert_eq!(b.cpu().core[0], 0);
    assert_eq!(b.cpu().core[15], 0x100B_0600);
    assert_eq!(b.breakpoints(), vec![0x100B_0700]);
}

#[test]
fn state_tag_identifies_bundle_bytes() {
    let a = backend(&COUNTER);
    let b = backend(&COUNTER);
    assert_eq!(a.state_tag(), b.state_tag());
    assert!(a.state_tag().starts_with("machine:"), "{}", a.state_tag());
    assert_eq!(a.state_tag().len(), "machine:".len() + 16);
    let mut other = COUNTER;
    other[0] = 0x2241; // movs r2,#0x41
    let c = backend(&other);
    assert_eq!(c.name(), "test.84b2");
    assert_ne!(a.state_tag(), c.state_tag());
    assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
}

#[test]
fn garbage_bundle_is_an_error() {
    assert!(MachineBackend::from_bytes(b"not a zip", "x.84b2".into(), &nv_dir()).is_err());
    let missing = std::env::temp_dir().join("evo-gui-tests-no-such-bundle.84b2");
    let err = MachineBackend::open(&missing, &nv_dir())
        .err()
        .expect("open must fail");
    assert!(err.to_string().starts_with("cannot read bundle:"), "{err}");
}

#[test]
fn open_names_backend_after_file() {
    let dir = nv_dir();
    let path = dir.join("counter.84b2");
    std::fs::write(&path, bundle_bytes(&COUNTER)).unwrap();
    let b = MachineBackend::open(&path, &nv_dir()).unwrap();
    assert_eq!(b.name(), "counter.84b2");
    assert_eq!(b.state_tag(), backend(&COUNTER).state_tag());
}

#[test]
fn real_os_runs_in_gui_backend() {
    let Some(path) = std::env::var_os("EVO_OS_IMAGE") else {
        eprintln!("skipping real_os_runs_in_gui_backend: EVO_OS_IMAGE is not set");
        return;
    };
    let mut b = MachineBackend::open(Path::new(&path), &nv_dir()).unwrap();
    let status = b.run_for(10_000_000);
    assert!(!matches!(status, RunStatus::Lockup { .. }), "{status:?}");
}

#[test]
fn rgb888_conversion() {
    let mut dst = vec![9; 12];
    rgb888_to_rgba(&[1, 2, 3, 4, 5, 6], &mut dst);
    assert_eq!(dst, [1, 2, 3, 255, 4, 5, 6, 255]);
}

#[test]
fn every_gui_key_is_the_core_key_of_its_name() {
    let mut names = BTreeSet::new();
    for id in KeyId::ALL {
        let name = id.core_name();
        let want = Key::from_name(name).unwrap_or_else(|| panic!("{id:?}: no core key {name}"));
        assert_eq!(core_key(id), want, "{id:?}");
        assert!(names.insert(name), "{id:?}: {name} used twice");
    }
    // The GUI has exactly the core's keys: 49 matrix keys and ON.
    let core: BTreeSet<&str> = KEY_NAMES.iter().map(|&(n, _)| n).collect();
    assert_eq!(names, core);
}

#[test]
fn gui_keys_follow_the_evo_keypad() {
    let mx = |row, col| Key::Matrix { row, col };
    for (id, key) in [
        (KeyId::On, Key::On),
        (KeyId::Enter, mx(6, 0)),
        (KeyId::Toggle, mx(6, 1)),
        (KeyId::Add, mx(6, 2)),
        (KeyId::Sub, mx(6, 3)),
        (KeyId::Mul, mx(6, 4)),
        (KeyId::Div, mx(6, 5)),
        (KeyId::Power, mx(2, 5)),
        (KeyId::Frac, mx(3, 6)),
        (KeyId::Up, mx(7, 3)),
        (KeyId::Second, mx(1, 5)),
        (KeyId::Two, mx(4, 1)),
        (KeyId::Three, mx(5, 1)),
    ] {
        assert_eq!(core_key(id), key, "{id:?}");
    }
}

/// `ldr r2,[pc,#4]; ldrh r0,[r2]; b.n` back to `ldrh`; `nop`; the literal `0x4010_xxxx`.
fn keypad_reader(row_reg: u16) -> [u16; 6] {
    [0x4A01, 0x8810, 0xE7FD, 0xBF00, row_reg, 0x4010]
}

#[test]
fn enter_sets_its_keypad_row_bit() {
    let mut b = backend(&keypad_reader(0x301C));
    b.set_key(KeyId::Enter, true);
    b.run_for(1000);
    assert_eq!(b.cpu().core[0], 0x0001);
    b.set_key(KeyId::Enter, false);
    b.run_for(1000);
    assert_eq!(b.cpu().core[0], 0);
}

#[test]
fn up_sets_its_keypad_row_bit() {
    let mut b = backend(&keypad_reader(0x301E));
    b.set_key(KeyId::Up, true);
    b.run_for(1000);
    assert_eq!(b.cpu().core[0], 0x0008);
}

#[test]
fn uart_bytes_reach_the_gui() {
    // Stores 'A' to 0x4010_6000 (UART0 THR), then spins.
    let mut b = backend(&[
        0x2141, 0x2240, 0x0212, 0x3210, 0x0412, 0x2360, 0x021B, 0x18D2, 0x7011, 0xE7FE,
    ]);
    b.run_for(100);
    assert_eq!(b.take_uart(), b"A");
    assert!(b.take_uart().is_empty());
}

#[test]
fn state_roundtrip_restores_cycles_and_registers() {
    let mut b = backend(&COUNTER);
    b.run_for(1000);
    let state = b.save_state().unwrap();
    let (cycles, r0) = (b.cycles(), b.cpu().core[0]);
    b.run_for(1000);
    assert_ne!(b.cycles(), cycles);
    b.load_state(&state).unwrap();
    assert_eq!(b.cycles(), cycles);
    assert_eq!(b.cpu().core[0], r0);
}

#[test]
fn load_garbage_state_errors_unchanged() {
    let mut b = backend(&COUNTER);
    b.run_for(1000);
    let cycles = b.cycles();
    assert!(matches!(
        b.load_state(b"xx"),
        Err(BackendError::BadState(_))
    ));
    assert_eq!(b.cycles(), cycles);
}

#[test]
fn flash_file_is_loaded_and_written_back() {
    let dir = nv_dir();
    let bundle = bundle_bytes(&COUNTER);
    let path = nv_path_for(&dir, &bundle);
    let erased = NvState::erased();
    let mut flash = erased.flash().to_vec();
    flash[0x9_0000] = 0xA5;
    NvState::new(flash, erased.secregs().to_vec())
        .unwrap()
        .write_file(&path)
        .unwrap();
    let mut b = MachineBackend::from_bytes(&bundle, "test.84b2".into(), &dir).unwrap();
    assert_eq!(b.nv_path(), path);
    assert_eq!(b.peek(0x0009_0000, 1), vec![Some(0xA5)]);
    b.persist().unwrap();
    assert_eq!(NvState::read_file(&path).unwrap().flash()[0x9_0000], 0xA5);
}

/// Writes erased flash with `marker` at 0x9_0000 to `path`.
fn write_marked_flash(path: &Path, marker: u8) {
    let erased = NvState::erased();
    let mut flash = erased.flash().to_vec();
    flash[0x9_0000] = marker;
    NvState::new(flash, erased.secregs().to_vec())
        .unwrap()
        .write_file(path)
        .unwrap();
}

#[test]
fn unprovisioned_unit_is_not_persisted() {
    // A fresh unit (no flash file) stays fresh until provisioning succeeds; writing its flash
    // would make the next launch skip provisioning.
    let root = nv_dir();
    let dir = root.join("flash");
    let bundle = bundle_bytes(&COUNTER);
    let path = nv_path_for(&dir, &bundle);
    let mut b = MachineBackend::from_bytes(&bundle, "test.84b2".into(), &dir).unwrap();
    assert!(b.needs_provision());
    b.run_for(1000);
    b.persist().unwrap();
    assert!(!path.exists());
    assert!(!dir.exists());
}

#[test]
fn unchanged_flash_is_not_rewritten() {
    let dir = nv_dir();
    let bundle = bundle_bytes(&COUNTER);
    let path = nv_path_for(&dir, &bundle);
    write_marked_flash(&path, 0xA5);
    let mut b = MachineBackend::from_bytes(&bundle, "test.84b2".into(), &dir).unwrap();
    b.run_for(1000);
    // Something else replaces the file: a persist of the unchanged machine leaves it alone.
    write_marked_flash(&path, 0x5A);
    b.persist().unwrap();
    assert_eq!(NvState::read_file(&path).unwrap().flash()[0x9_0000], 0x5A);
}

#[test]
fn persist_recreates_a_missing_flash_file() {
    let root = nv_dir();
    let dir = root.join("flash");
    let bundle = bundle_bytes(&COUNTER);
    let path = nv_path_for(&dir, &bundle);
    std::fs::create_dir_all(&dir).unwrap();
    write_marked_flash(&path, 0xA5);
    let mut b = MachineBackend::from_bytes(&bundle, "test.84b2".into(), &dir).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    b.persist().unwrap();
    assert_eq!(NvState::read_file(&path).unwrap().flash()[0x9_0000], 0xA5);
}

#[test]
fn corrupt_flash_file_fails_open_unchanged() {
    let dir = nv_dir();
    let bundle = bundle_bytes(&COUNTER);
    let path = nv_path_for(&dir, &bundle);
    std::fs::write(&path, [0x5A; 100]).unwrap();
    let err = MachineBackend::from_bytes(&bundle, "test.84b2".into(), &dir)
        .err()
        .expect("a corrupt flash file must fail");
    assert!(matches!(err, OpenError::Flash { .. }), "{err}");
    assert!(err.to_string().contains(".nv"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), [0x5A; 100]);
}

#[test]
fn real_os_lcd_frame() {
    let Some(path) = std::env::var_os("EVO_OS_IMAGE") else {
        eprintln!("skipping real_os_lcd_frame: EVO_OS_IMAGE is not set");
        return;
    };
    let mut b = MachineBackend::open(Path::new(&path), &nv_dir()).unwrap();
    let mut status = RunStatus::Running;
    // The LCD turns on a little before the OS raises the backlight PWM, so wait for both.
    while b.framebuffer().is_none() || b.backlight() == 0.0 {
        assert!(
            b.cycles() < 2_000_000_000,
            "no lit LCD frame: last status {status:?} at cycle {}, backlight {}",
            b.cycles(),
            b.backlight()
        );
        status = b.run_for(10_000_000);
        assert!(
            !status.is_stop(),
            "stopped: {status:?} at cycle {}",
            b.cycles()
        );
    }
    let frame = b.framebuffer().unwrap();
    assert!(frame.is_valid());
    assert_eq!((frame.width, frame.height), (320, 240));
    assert!(b.backlight() > 0.0, "backlight {}", b.backlight());
}

#[test]
fn test_dirs_are_removed() {
    let dir = nv_dir();
    let path = dir.to_path_buf();
    std::fs::write(dir.join("x.nv"), b"x").unwrap();
    assert!(path.exists());
    drop(dir);
    assert!(!path.exists(), "{} was left behind", path.display());
}

#[test]
fn inspect_bundle_reads_metadata() {
    let info = inspect_bundle(&bundle_bytes(&COUNTER)).unwrap();
    assert_eq!(info.build_version, "9.8.7.6543");
    assert_eq!(info.target_device, "84XX");
    assert_eq!(info.prodid, 42);
    assert!(inspect_bundle(b"nope").is_err());
}

#[test]
fn io_methods_are_unsupported_until_wired() {
    let mut b = backend(&COUNTER);
    assert!(matches!(b.export_ram(), Err(BackendError::Unsupported(_))));
    assert!(b.take_key_events().is_empty());
}

// ---- Task W1: the debugger on the real machine ----

/// The store program at 0x100B_0600: `movs r1,#0x41; movs r2,#0x30; lsls r2,r2,#24;
/// str r1,[r2]` (at 0x100B_0606, to 0x3000_0000), then `b.` forever.
const STORE: [u16; 5] = [0x2141, 0x2230, 0x0612, 0x6011, 0xE7FE];
const STORE_PC: u32 = 0x100B_0606;
const STORE_ADDR: u32 = 0x3000_0000;

/// The call program at 0x100B_0600: `bl 0x100B_0610`; `movs r0,#7` (0x0604); `b.` (0x0606);
/// four `nop`s; at 0x0610 `adds r1,#1; bx lr`.
const CALL: [u16; 10] = [
    0xF000, 0xF806, 0x2007, 0xE7FE, 0xBF00, 0xBF00, 0xBF00, 0xBF00, 0x3101, 0x4770,
];

fn write_watch(start: u32, len: u32) -> Watchpoint {
    Watchpoint {
        start,
        len,
        kinds: WatchKinds {
            read: false,
            write: true,
            exec: false,
        },
    }
}

#[test]
fn poke_writes_sram_and_refuses_devices() {
    let mut b = backend(&COUNTER);
    b.poke(0x3000_0010, &[1, 2, 3]).unwrap();
    assert_eq!(b.peek(0x3000_0010, 3), vec![Some(1), Some(2), Some(3)]);
    // The debugger may patch flash.
    b.poke(0x0009_0000, &[0xA5]).unwrap();
    assert_eq!(b.peek(0x0009_0000, 1), vec![Some(0xA5)]);
    assert!(matches!(
        b.poke(0x4010_6000, &[1]),
        Err(BackendError::Unwritable(0x4010_6000))
    ));
    // All or nothing: the byte in SRAM is not written when the next one is past its end.
    let before = b.peek(0x3017_FFFF, 1);
    assert!(matches!(
        b.poke(0x3017_FFFF, &[9, 9]),
        Err(BackendError::Unwritable(0x3018_0000))
    ));
    assert_eq!(b.peek(0x3017_FFFF, 1), before);
    // A write past 0xffffffff is refused at the first address that does not exist.
    assert!(matches!(
        b.poke(0xFFFF_FFFF, &[1, 2]),
        Err(BackendError::Unwritable(_))
    ));
    b.poke(0x3000_0000, &[]).unwrap();
}

#[test]
fn regions_cover_flash_and_sram() {
    let b = backend(&COUNTER);
    let regions: Vec<(String, u32, u32)> = b
        .memory_regions()
        .into_iter()
        .map(|r| (r.name, r.start, r.len))
        .collect();
    assert_eq!(
        regions,
        vec![
            ("Flash (NS)".into(), 0x0000_0000, 16 << 20),
            ("Flash (S)".into(), 0x1000_0000, 16 << 20),
            ("SRAM (NS)".into(), 0x2000_0000, 0x18_0000),
            ("SRAM (S)".into(), 0x3000_0000, 0x18_0000),
        ]
    );
    for (_, start, len) in regions {
        assert!(b.peek(start, 1)[0].is_some(), "{start:#x}");
        assert!(b.peek(start + len - 1, 1)[0].is_some(), "{start:#x}");
    }
}

#[test]
fn register_writes_reach_the_cpu() {
    let mut b = backend(&COUNTER);
    b.set_core_reg(0, 0x1234).unwrap();
    b.set_core_reg(12, 0xC0DE).unwrap();
    b.set_core_reg(13, 0x3000_0700).unwrap();
    b.set_core_reg(14, 0x100B_0601).unwrap();
    b.set_core_reg(15, 0x100B_0605).unwrap();
    b.set_core_reg(16, 0x6100_0000).unwrap();
    b.set_banked_reg("msp_ns", 0x2000_0400).unwrap();
    let cpu = b.cpu();
    assert_eq!(cpu.core[0], 0x1234);
    assert_eq!(cpu.core[12], 0xC0DE);
    assert_eq!(cpu.core[13], 0x3000_0700);
    assert_eq!(cpu.core[14], 0x100B_0601);
    assert_eq!(cpu.core[15], 0x100B_0604, "pc drops the Thumb bit");
    assert_eq!(cpu.xpsr, 0x6100_0000);
    assert!(cpu.banked.contains(&("msp_ns", 0x2000_0400)));
    assert!(matches!(
        b.set_banked_reg("nope", 1),
        Err(BackendError::BadRegister(_))
    ));
    assert!(matches!(
        b.set_core_reg(17, 1),
        Err(BackendError::BadRegister(_))
    ));
    assert_eq!(b.cpu(), cpu, "a refused write changes nothing");
}

#[test]
fn watchpoint_stops_run_for() {
    let mut b = backend(&STORE);
    let set = vec![write_watch(STORE_ADDR, 4)];
    b.set_watchpoints(set.clone()).unwrap();
    assert_eq!(b.watchpoints(), set);
    assert_eq!(
        b.run_for(1000),
        RunStatus::Watchpoint {
            addr: STORE_ADDR,
            dir: AccessDir::Write,
            value: 0x41,
            pc: STORE_PC,
        }
    );
    // An invalid set is refused and changes nothing.
    assert!(matches!(
        b.set_watchpoints(vec![write_watch(0x3000_0100, 4), write_watch(0, 0)]),
        Err(BackendError::Invalid(_))
    ));
    assert_eq!(b.watchpoints(), set);
    // Continue moves on: the store does not run again.
    assert_eq!(b.run_for(1000), RunStatus::Running);
    b.set_watchpoints(vec![]).unwrap();
    assert!(b.watchpoints().is_empty());
}

#[test]
fn bus_monitor_records_the_store() {
    let mut b = backend(&STORE);
    b.set_bus_monitor(Some((STORE_ADDR, 4)));
    b.run_for(1000);
    let recs = b.bus_records();
    assert_eq!(recs.len(), 1, "{recs:?}");
    let r = recs[0];
    assert_eq!(
        (r.addr, r.width, r.dir, r.value, r.pc),
        (STORE_ADDR, 4, AccessDir::Write, 0x41, STORE_PC)
    );
    assert!(r.cycle > 0 && r.cycle <= b.cycles());
    // Off keeps the records; clear drops them.
    b.set_bus_monitor(None);
    assert_eq!(b.bus_records().len(), 1);
    b.clear_bus_records();
    assert!(b.bus_records().is_empty());
}

#[test]
fn step_over_and_run_to_goals() {
    let mut b = backend(&CALL);
    // Over the call: it runs, and the run stops after it.
    assert!(b.begin_goal(Goal::StepOver).unwrap());
    assert_eq!(b.run_for(1000), RunStatus::Goal { pc: 0x100B_0604 });
    assert_eq!(b.cpu().core[1], 1);
    // Over a plain instruction: done at once.
    assert!(!b.begin_goal(Goal::StepOver).unwrap());
    assert_eq!(b.cpu().core[15], 0x100B_0606);
    assert_eq!(b.cpu().core[0], 7);
    // Run to an address the program never reaches: armed until cancelled.
    assert!(b.begin_goal(Goal::RunTo(0x100B_0608)).unwrap());
    assert_eq!(b.run_for(1000), RunStatus::Running);
    b.cancel_goal();
    assert_eq!(b.run_for(1000), RunStatus::Running);

    let mut b = backend(&CALL);
    assert!(b.begin_goal(Goal::RunTo(0x100B_0611)).unwrap());
    assert_eq!(b.run_for(1000), RunStatus::Goal { pc: 0x100B_0610 });
    // Step out of the function returns to the instruction after the call.
    assert!(b.begin_goal(Goal::StepOut).unwrap());
    assert_eq!(b.run_for(1000), RunStatus::Goal { pc: 0x100B_0604 });
}

#[test]
fn peripherals_include_uart0_and_nvic() {
    let b = backend(&COUNTER);
    let devs = b.peripherals();
    let names: Vec<&str> = devs.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"uart0"), "{names:?}");
    assert!(names.contains(&"nvic"), "{names:?}");
    let uart0 = devs.iter().find(|d| d.name == "uart0").unwrap();
    assert_eq!(uart0.base, 0x4010_6000);
    assert!(!uart0.regs.is_empty());
    let nvic = devs.iter().find(|d| d.name == "nvic").unwrap();
    assert_eq!(nvic.base, 0xE000_E100);
}

#[test]
fn reset_keeps_watchpoints() {
    let mut b = backend(&STORE);
    let set = vec![write_watch(STORE_ADDR, 4)];
    b.set_watchpoints(set.clone()).unwrap();
    b.set_breakpoints(vec![0x100B_0700]);
    b.set_bus_monitor(Some((STORE_ADDR, 4)));
    b.run_for(1000);
    b.clear_bus_records();
    // A goal armed before the reset is dropped.
    assert!(b.begin_goal(Goal::RunTo(0x100B_0602)).unwrap());
    b.reset();
    assert_eq!(b.watchpoints(), set);
    assert_eq!(b.breakpoints(), vec![0x100B_0700]);
    assert!(matches!(b.run_for(1000), RunStatus::Watchpoint { .. }));
    // The monitor range survived the reset too.
    assert_eq!(b.bus_records().len(), 1, "{:?}", b.bus_records());
}
