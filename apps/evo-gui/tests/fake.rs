use std::collections::HashSet;

use evo_gui::backend::fake::{FAKE_PC, FakeBackend, FakeProbe};
use evo_gui::backend::{BackendError, EmuBackend, Frame, RunStatus};
use evo_gui::keypad::KeyId;

fn pixel(frame: &Frame, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * frame.width + x) * 4) as usize;
    [
        frame.rgba[i],
        frame.rgba[i + 1],
        frame.rgba[i + 2],
        frame.rgba[i + 3],
    ]
}

#[test]
fn key_ids_are_unique_and_named() {
    assert_eq!(KeyId::ALL.len(), 50);
    let ids: HashSet<KeyId> = KeyId::ALL.iter().copied().collect();
    assert_eq!(ids.len(), 50);
    let names: HashSet<&str> = KeyId::ALL.iter().map(|k| k.name()).collect();
    assert_eq!(names.len(), 50);
    assert!(names.iter().all(|n| !n.is_empty()));
    assert_eq!(KeyId::ALL[0].name(), "y=");
    assert_eq!(KeyId::Sub.name(), "\u{2212}");
    assert_eq!(KeyId::ALL[49], KeyId::Enter);
    assert_eq!(KeyId::Enter.name(), "enter");
}

#[test]
fn frame_validity() {
    assert!(
        !Frame {
            width: 2,
            height: 2,
            rgba: vec![0; 15]
        }
        .is_valid()
    );
    assert!(
        Frame {
            width: 2,
            height: 2,
            rgba: vec![0; 16]
        }
        .is_valid()
    );
    assert!(
        !Frame {
            width: 0,
            height: 2,
            rgba: vec![]
        }
        .is_valid()
    );
    assert!(
        !Frame {
            width: u32::MAX,
            height: u32::MAX,
            rgba: vec![]
        }
        .is_valid()
    );
    let mut fake = FakeBackend::new();
    let frame = fake.framebuffer().expect("fake has a display");
    assert!(frame.is_valid());
    assert_eq!((frame.width, frame.height), (320, 240));
}

#[test]
fn held_key_lights_its_cell() {
    let mut fake = FakeBackend::new();
    fake.set_key(KeyId::Enter, true);
    assert_eq!(
        pixel(fake.framebuffer().unwrap(), 252, 215),
        [0x1F, 0x6F, 0xB5, 0xFF]
    );
    fake.set_key(KeyId::Enter, false);
    assert_eq!(
        pixel(fake.framebuffer().unwrap(), 252, 215),
        [0xD0, 0xD6, 0xDE, 0xFF]
    );
}

#[test]
fn uart_reports_every_100_mcycles() {
    let mut fake = FakeBackend::new();
    assert_eq!(fake.run_for(250_000_000), RunStatus::Running);
    assert_eq!(
        fake.take_uart(),
        b"fake: 100 Mcycles\r\nfake: 200 Mcycles\r\n".to_vec()
    );
    assert!(fake.take_uart().is_empty());
}

#[test]
fn state_roundtrip() {
    let mut fake = FakeBackend::new();
    let probe = fake.probe();
    fake.run_for(123);
    fake.set_key(KeyId::Sin, true);
    let state = fake.save_state().unwrap();
    fake.reset();
    assert_eq!(fake.cycles(), 0);
    assert!(probe.held().is_empty());
    assert_eq!(probe.resets(), 1);
    fake.load_state(&state).unwrap();
    assert_eq!(fake.cycles(), 123);
    assert_eq!(probe.held(), vec![KeyId::Sin]);
}

#[test]
fn corrupt_state_is_rejected_and_changes_nothing() {
    let mut fake = FakeBackend::new();
    let probe = fake.probe();
    fake.run_for(77);
    fake.set_key(KeyId::Two, true);
    let result = fake.load_state(b"{not json");
    assert!(matches!(result, Err(BackendError::BadState(_))));
    assert_eq!(fake.cycles(), 77);
    assert_eq!(probe.held(), vec![KeyId::Two]);
}

#[test]
fn breakpoint_at_pc_stops() {
    let mut fake = FakeBackend::new();
    fake.toggle_breakpoint(FAKE_PC);
    assert_eq!(fake.breakpoints(), vec![FAKE_PC]);
    assert_eq!(fake.run_for(1000), RunStatus::Breakpoint(FAKE_PC));
    assert!(RunStatus::Breakpoint(FAKE_PC).is_stop());
    assert_eq!(fake.cycles(), 0);
    fake.toggle_breakpoint(FAKE_PC);
    assert!(fake.breakpoints().is_empty());
    assert_eq!(fake.run_for(1000), RunStatus::Running);
    assert_eq!(fake.cycles(), 1000);
    assert_eq!(fake.step(), RunStatus::Running);
    assert_eq!(fake.cycles(), 1001);
}

#[test]
fn idle_fake_never_advances() {
    let mut fake = FakeBackend::idle();
    let probe = fake.probe();
    assert_eq!(fake.run_for(1000), RunStatus::Idle);
    assert_eq!(fake.cycles(), 0);
    assert_eq!(probe.run_calls(), 1);
    assert!(!RunStatus::Idle.is_stop());
}

#[test]
fn backend_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Box<dyn EmuBackend>>();
    assert_send::<FakeProbe>();
}

#[test]
fn without_display_has_no_frame() {
    let mut fake = FakeBackend::without_display();
    assert!(fake.framebuffer().is_none());
}

#[test]
fn peek_outside_fake_mem_is_none() {
    let fake = FakeBackend::new();
    assert_eq!(
        fake.peek(0x2000_FFFF, 2),
        vec![Some((0xFFFF % 251) as u8), None]
    );
}

#[test]
fn probe_and_views_match_the_spec() {
    let mut fake = FakeBackend::new().with_state_tag("other");
    let probe = fake.probe();
    assert_eq!(fake.name(), "demo");
    assert_eq!(fake.state_tag(), "other");
    assert_eq!(FakeBackend::new().state_tag(), "demo");
    assert_eq!(fake.backlight(), 1.0);
    fake.persist().unwrap();
    assert_eq!(probe.persists(), 1);
    fake.set_key(KeyId::Up, true);
    fake.set_key(KeyId::Up, false);
    assert_eq!(probe.key_log(), vec![(KeyId::Up, true), (KeyId::Up, false)]);
    fake.run_for(5);
    assert_eq!(probe.cycles(), 5);
    let cpu = fake.cpu();
    assert_eq!(cpu.core[0], 5);
    assert_eq!(cpu.core[13], 0x2000_8000);
    assert_eq!(cpu.core[15], FAKE_PC);
    assert_eq!(cpu.xpsr, 0x0100_0000);
    assert!(cpu.secure && !cpu.handler);
    assert_eq!(
        cpu.banked,
        vec![("msp_s", 0x3000_0800), ("msp_ns", 0x2009_3000)]
    );
    let dis = fake.disasm(0x100, 3);
    assert_eq!(dis.len(), 3);
    assert_eq!(dis[2].addr, 0x104);
    assert_eq!(
        (dis[2].len, dis[2].raw, dis[2].text.as_str()),
        (2, 0xBF00, "nop")
    );
    let un = fake.unmapped();
    assert_eq!(un.len(), 1);
    assert_eq!(
        (
            un[0].addr,
            un[0].width,
            un[0].kind,
            un[0].value,
            un[0].pc,
            un[0].secure,
            un[0].count
        ),
        (0x5020_8014, 4, "read", None, FAKE_PC, true, 1)
    );
}
