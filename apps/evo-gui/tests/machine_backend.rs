//! `MachineBackend` on a synthetic secure image (and on the real OS when `EVO_OS_IMAGE` is set).

use evo_gui::backend::machine::{MachineBackend, fnv1a64};
use evo_gui::backend::{BackendError, EmuBackend, RunStatus};

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

fn backend(code: &[u16]) -> MachineBackend {
    MachineBackend::from_bytes(&bundle_bytes(code), "test.84b2".into()).unwrap()
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
fn plan2_features_report_unsupported() {
    let mut b = backend(&COUNTER);
    assert!(matches!(
        b.save_state(),
        Err(BackendError::Unsupported("save states"))
    ));
    assert!(matches!(
        b.load_state(b"{}"),
        Err(BackendError::Unsupported("save states"))
    ));
    assert!(b.framebuffer().is_none());
    assert_eq!(b.backlight(), 1.0);
    assert!(b.persist().is_ok());
    assert!(b.take_uart().is_empty());
    b.set_key(evo_gui::keypad::KeyId::Enter, true);
}

#[test]
fn garbage_bundle_is_an_error() {
    assert!(MachineBackend::from_bytes(b"not a zip", "x.84b2".into()).is_err());
    let missing = std::env::temp_dir().join("evo-gui-tests-no-such-bundle.84b2");
    let err = MachineBackend::open(&missing)
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
    let b = MachineBackend::open(&path).unwrap();
    assert_eq!(b.name(), "counter.84b2");
    assert_eq!(b.state_tag(), backend(&COUNTER).state_tag());
}

#[test]
fn real_os_runs_in_gui_backend() {
    let Some(path) = std::env::var_os("EVO_OS_IMAGE") else {
        eprintln!("skipping real_os_runs_in_gui_backend: EVO_OS_IMAGE is not set");
        return;
    };
    let mut b = MachineBackend::open(std::path::Path::new(&path)).unwrap();
    let status = b.run_for(10_000_000);
    assert!(!matches!(status, RunStatus::Lockup { .. }), "{status:?}");
}
