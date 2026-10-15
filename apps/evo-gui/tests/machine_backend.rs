//! `MachineBackend` on a synthetic secure image (and on the real OS when `EVO_OS_IMAGE` is set).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use evo_core::{Key, NvState};
use evo_gui::backend::machine::{KEY_MATRIX, MachineBackend, OpenError, fnv1a64, rgb888_to_rgba};
use evo_gui::backend::{BackendError, EmuBackend, RunStatus};
use evo_gui::keypad::KeyId;

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

/// A fresh, empty directory for one test's flash files.
fn nv_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "evo-gui-tests-nv-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
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
    let dir = std::env::temp_dir().join("evo-gui-tests-machine");
    std::fs::create_dir_all(&dir).unwrap();
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
fn key_matrix_table_is_complete() {
    assert_eq!(KEY_MATRIX.map(|e| e.0), KeyId::ALL);
    let mut seen = std::collections::BTreeSet::new();
    for (id, key) in KEY_MATRIX {
        match key {
            Key::Matrix { row, col } => {
                assert!(row < 8 && col < 8, "{id:?} at r{row}c{col}");
                assert!(seen.insert((row, col)), "{id:?}: r{row}c{col} used twice");
            }
            Key::On => assert_eq!(id, KeyId::On),
        }
    }
    assert_eq!(seen.len(), 49);
    let key = |id: KeyId| KEY_MATRIX.iter().find(|e| e.0 == id).unwrap().1;
    assert_eq!(key(KeyId::On), Key::On);
    assert_eq!(key(KeyId::Enter), Key::Matrix { row: 6, col: 0 });
    assert_eq!(key(KeyId::Up), Key::Matrix { row: 7, col: 3 });
    assert_eq!(key(KeyId::Second), Key::Matrix { row: 1, col: 5 });
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

#[test]
fn persist_creates_flash_file() {
    let dir = nv_dir().join("flash");
    let bundle = bundle_bytes(&COUNTER);
    let path = nv_path_for(&dir, &bundle);
    let mut b = MachineBackend::from_bytes(&bundle, "test.84b2".into(), &dir).unwrap();
    assert!(!path.exists());
    b.persist().unwrap();
    assert!(path.exists());
    assert!(NvState::read_file(&path).is_ok());
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
