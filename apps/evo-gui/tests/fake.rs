mod common;

use std::collections::HashSet;

use common::TempDir;
use evo_gui::backend::fake::{FAKE_BUS_CAP, FAKE_PC, FAKE_WATCH_ADDR, FakeBackend, FakeProbe};
use evo_gui::backend::placeholder::Placeholder;
use evo_gui::backend::{
    AccessDir, BackendError, BusRecord, DeviceView, EmuBackend, Frame, Goal, HardwareView,
    HwChange, KeyEvent, MemRegion, RegRow, RunStatus, UartId, WatchKinds, Watchpoint,
};
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

fn write_watch(start: u32, len: u32) -> Watchpoint {
    Watchpoint {
        start,
        len,
        kinds: WatchKinds {
            write: true,
            ..WatchKinds::default()
        },
    }
}

#[test]
fn poke_writes_overlay_and_peek_sees_it() {
    let mut fake = FakeBackend::new();
    let probe = fake.probe();
    fake.poke(0x2000_0010, &[1, 2]).unwrap();
    assert_eq!(fake.peek(0x2000_0010, 2), vec![Some(1), Some(2)]);
    assert_eq!(probe.pokes(), vec![(0x2000_0010, vec![1, 2])]);
    // Reset clears the overlay.
    fake.reset();
    assert_eq!(fake.peek(0x2000_0010, 1), vec![Some(0x10)]);
}

#[test]
fn poke_outside_is_all_or_nothing() {
    let mut fake = FakeBackend::new();
    let probe = fake.probe();
    let before = fake.peek(0x2000_FFFF, 1);
    assert!(matches!(
        fake.poke(0x2000_FFFF, &[9, 9]),
        Err(BackendError::Unwritable(0x2001_0000))
    ));
    assert_eq!(fake.peek(0x2000_FFFF, 1), before);
    assert!(probe.pokes().is_empty());
    assert_eq!(
        BackendError::Unwritable(0x2001_0000).to_string(),
        "address 0x20010000 is not writable"
    );
}

#[test]
fn memory_regions_and_peripherals_match_the_spec() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    assert_eq!(
        fake.memory_regions(),
        vec![MemRegion {
            name: "fake RAM".into(),
            start: 0x2000_0000,
            len: 0x1_0000
        }]
    );
    let row = |name: &str, offset, value, note: Option<&str>| RegRow {
        name: name.into(),
        offset,
        value,
        note: note.map(Into::into),
    };
    assert_eq!(
        fake.peripherals(),
        vec![
            DeviceView {
                name: "timer".into(),
                base: 0x4010_1000,
                regs: vec![
                    row("ctrl", 0x30, 0x1, None),
                    row("int_stat", 0x34, 0x0, None)
                ],
            },
            DeviceView {
                name: "nvic".into(),
                base: 0xE000_E100,
                regs: vec![row("irq13", 13, 0x1, Some("enabled"))],
            },
        ]
    );
    assert!(fake.bus_records().is_empty());
    assert_eq!(probe.inspect_calls(), 2);
}

#[test]
fn register_overrides() {
    let mut fake = FakeBackend::new();
    fake.set_core_reg(13, 0x2000_1234).unwrap();
    fake.set_core_reg(16, 0x6100_0000).unwrap();
    fake.set_banked_reg("msp_ns", 0x2009_0000).unwrap();
    let cpu = fake.cpu();
    assert_eq!(cpu.core[13], 0x2000_1234);
    assert_eq!(cpu.xpsr, 0x6100_0000);
    assert_eq!(
        cpu.banked,
        vec![("msp_s", 0x3000_0800), ("msp_ns", 0x2009_0000)]
    );
    assert!(matches!(
        fake.set_core_reg(17, 0),
        Err(BackendError::BadRegister(r)) if r == "#17"
    ));
    assert!(matches!(
        fake.set_banked_reg("r99", 0),
        Err(BackendError::BadRegister(r)) if r == "r99"
    ));
    fake.reset();
    assert_eq!(fake.cpu().core[13], 0x2000_8000);
    assert_eq!(fake.cpu().xpsr, 0x0100_0000);
}

#[test]
fn goals_arm_and_finish() {
    let mut fake = FakeBackend::new();
    let probe = fake.probe();
    assert!(!fake.begin_goal(Goal::StepOver).unwrap());
    assert_eq!(fake.cycles(), 1);
    assert!(fake.begin_goal(Goal::RunTo(0x1000_0203)).unwrap());
    assert!(probe.goal_armed());
    assert_eq!(fake.run_for(5000), RunStatus::Goal { pc: 0x1000_0202 });
    assert!(RunStatus::Goal { pc: 0 }.is_stop());
    assert_eq!(fake.cycles(), 1001);
    assert!(!probe.goal_armed());
    assert_eq!(fake.run_for(5000), RunStatus::Running);
    assert!(fake.begin_goal(Goal::StepOut).unwrap());
    assert_eq!(fake.run_for(10), RunStatus::Goal { pc: FAKE_PC + 0x40 });
    assert_eq!(fake.cycles(), 6011);
    assert_eq!(
        probe.goals(),
        vec![Goal::StepOver, Goal::RunTo(0x1000_0203), Goal::StepOut]
    );
}

#[test]
fn cancel_goal_disarms() {
    let mut fake = FakeBackend::new();
    let probe = fake.probe();
    assert!(fake.begin_goal(Goal::RunTo(0x1000_0300)).unwrap());
    fake.cancel_goal();
    assert_eq!(probe.goal_cancels(), 1);
    assert!(!probe.goal_armed());
    assert_eq!(fake.run_for(5000), RunStatus::Running);
    assert_eq!(fake.cycles(), 5000);
}

#[test]
fn never_reached_goal_stays_armed() {
    let mut fake = FakeBackend::new().goal_never_reached();
    let probe = fake.probe();
    assert!(fake.begin_goal(Goal::RunTo(0x1000_0300)).unwrap());
    for _ in 0..3 {
        assert_eq!(fake.run_for(5000), RunStatus::Running);
    }
    assert_eq!(fake.cycles(), 15_000);
    assert!(probe.goal_armed());
    fake.reset();
    assert!(!probe.goal_armed());
}

#[test]
fn load_state_disarms_goal_and_clears_overrides() {
    let mut fake = FakeBackend::new();
    let probe = fake.probe();
    let state = fake.save_state().unwrap();
    fake.poke(0x2000_0000, &[0xEE]).unwrap();
    fake.set_core_reg(0, 0x55).unwrap();
    assert!(fake.begin_goal(Goal::StepOut).unwrap());
    fake.load_state(&state).unwrap();
    assert!(!probe.goal_armed());
    assert_eq!(fake.peek(0x2000_0000, 1), vec![Some(0)]);
    assert_eq!(fake.cpu().core[0], 0);
}

#[test]
fn watchpoint_fires_once_per_arm() {
    let mut fake = FakeBackend::new();
    fake.run_for(100);
    let w = write_watch(FAKE_WATCH_ADDR, 4);
    fake.set_watchpoints(vec![w]).unwrap();
    assert_eq!(fake.watchpoints(), vec![w]);
    assert_eq!(
        fake.run_for(1000),
        RunStatus::Watchpoint {
            addr: FAKE_WATCH_ADDR,
            dir: AccessDir::Write,
            value: 0x2A,
            pc: FAKE_PC
        }
    );
    assert!(
        RunStatus::Watchpoint {
            addr: 0,
            dir: AccessDir::Read,
            value: 0,
            pc: 0
        }
        .is_stop()
    );
    assert_eq!(fake.cycles(), 100);
    assert_eq!(fake.run_for(1000), RunStatus::Running);
    assert_eq!(fake.cycles(), 1100);
    let bad = Watchpoint { len: 0, ..w };
    assert!(matches!(
        fake.set_watchpoints(vec![write_watch(0x2000_0000, 4), bad]),
        Err(BackendError::Invalid(m)) if m == "invalid watchpoint"
    ));
    assert_eq!(fake.watchpoints(), vec![w]);
    // A watchpoint that does not cover the address never fires.
    fake.set_watchpoints(vec![write_watch(0x2000_0000, 4)])
        .unwrap();
    assert_eq!(fake.run_for(10), RunStatus::Running);
}

fn watch_all() -> Watchpoint {
    Watchpoint {
        start: 0x2000_0000,
        len: 8,
        kinds: WatchKinds {
            read: true,
            write: true,
            exec: true,
        },
    }
}

#[test]
fn watchpoint_validity_and_names() {
    assert!(write_watch(0, 1).is_valid());
    assert!(write_watch(u32::MAX, 1).is_valid());
    assert!(!write_watch(u32::MAX, 2).is_valid());
    assert!(!write_watch(0, 0).is_valid());
    assert!(
        !Watchpoint {
            start: 0,
            len: 4,
            kinds: WatchKinds::default()
        }
        .is_valid()
    );
    assert_eq!(write_watch(0x100, 4).last(), 0x103);
    assert_eq!(write_watch(u32::MAX, 2).last(), u32::MAX);
    assert_eq!(write_watch(5, 0).last(), 5);
    assert_eq!(AccessDir::Read.as_str(), "read");
    assert_eq!(AccessDir::Write.as_str(), "write");
    assert_eq!(AccessDir::Exec.as_str(), "exec");
    let json = serde_json::to_string(&watch_all()).unwrap();
    assert_eq!(
        serde_json::from_str::<Watchpoint>(&json).unwrap(),
        watch_all()
    );
}

#[test]
fn bus_monitor_ring_is_bounded() {
    let mut fake = FakeBackend::new();
    fake.set_bus_monitor(Some((FAKE_WATCH_ADDR, 4)));
    for _ in 0..FAKE_BUS_CAP + 10 {
        fake.run_for(1);
    }
    let recs = fake.bus_records();
    assert_eq!(recs.len(), FAKE_BUS_CAP);
    assert_eq!(
        recs[0],
        BusRecord {
            addr: FAKE_WATCH_ADDR,
            width: 4,
            dir: AccessDir::Write,
            value: 0x2A,
            pc: FAKE_PC,
            cycle: 10
        }
    );
    assert_eq!(recs[FAKE_BUS_CAP - 1].cycle, (FAKE_BUS_CAP + 9) as u64);
    // Stopping keeps the records; a range that misses the address records nothing.
    fake.set_bus_monitor(None);
    fake.run_for(1);
    assert_eq!(fake.bus_records().len(), FAKE_BUS_CAP);
    fake.clear_bus_records();
    assert!(fake.bus_records().is_empty());
    fake.set_bus_monitor(Some((0x2000_0000, 0x10)));
    fake.run_for(1);
    assert!(fake.bus_records().is_empty());
}

#[test]
fn uart_send_echoes_per_port() {
    let mut fake = FakeBackend::new();
    let probe = fake.probe();
    fake.send_uart(UartId::Uart0, b"a");
    fake.send_uart(UartId::Uart2, b"bc");
    assert_eq!(fake.take_uart(), b"a".to_vec());
    assert_eq!(fake.take_uart2(), b"bc".to_vec());
    assert!(fake.take_uart().is_empty());
    assert!(fake.take_uart2().is_empty());
    assert_eq!(
        probe.uart_sent(),
        vec![
            (UartId::Uart0, b"a".to_vec()),
            (UartId::Uart2, b"bc".to_vec())
        ]
    );
}

#[test]
fn key_events_carry_cycles() {
    let mut fake = FakeBackend::new();
    let probe = fake.probe();
    fake.run_for(500);
    fake.set_key(KeyId::Enter, true);
    let ev = KeyEvent {
        key: KeyId::Enter,
        pressed: true,
        cycle: 500,
    };
    assert_eq!(fake.take_key_events(), vec![ev]);
    assert!(fake.take_key_events().is_empty());
    assert_eq!(probe.key_events(), vec![ev]);
}

#[test]
fn state_of_other_backend_is_refused() {
    let mut fake = FakeBackend::new();
    fake.run_for(500);
    assert!(matches!(
        fake.load_state(b"EVOSTATE\x01\x00binary"),
        Err(BackendError::BadState(_))
    ));
    assert_eq!(fake.cycles(), 500);
}

#[test]
fn hardware_changes() {
    let mut fake = FakeBackend::new();
    let probe = fake.probe();
    assert_eq!(
        fake.hardware(),
        HardwareView {
            backlight: 1.0,
            charger_line: true,
            battery_ok: true,
            usb_attached: false,
            power_hold: true,
            deep_sleep: false,
            rtc_seconds: 0
        }
    );
    let changes = [
        HwChange::ChargerLine(false),
        HwChange::BatteryOk(false),
        HwChange::UsbAttached(true),
        HwChange::RtcSeconds(86_399),
    ];
    for c in changes {
        fake.set_hardware(c).unwrap();
    }
    let hw = fake.hardware();
    assert!(!hw.charger_line && !hw.battery_ok && hw.usb_attached);
    assert_eq!(hw.rtc_seconds, 86_399);
    assert!(matches!(
        fake.set_hardware(HwChange::RtcSeconds(86_400)),
        Err(BackendError::Invalid(m)) if m == "RTC time must be below 24 h"
    ));
    assert_eq!(fake.hardware().rtc_seconds, 86_399);
    assert_eq!(probe.hw_changes(), changes.to_vec());
}

#[test]
fn ram_image_roundtrip_and_garbage() {
    let mut fake = FakeBackend::new();
    fake.poke(0x2000_0040, &[7, 8]).unwrap();
    let image = fake.export_ram().unwrap();
    assert!(image.starts_with(b"FAKERAM1"));
    fake.reset();
    assert_eq!(fake.peek(0x2000_0040, 1), vec![Some(0x40)]);
    fake.import_ram(&image).unwrap();
    assert_eq!(fake.peek(0x2000_0040, 2), vec![Some(7), Some(8)]);
    let garbage: [&[u8]; 4] = [
        b"",
        b"garbage",
        b"FAKERAM1{not json",
        b"FAKERAM1[[4294967295,1]]",
    ];
    for bad in garbage {
        assert!(
            matches!(fake.import_ram(bad), Err(BackendError::BadState(m)) if m == "not a RAM image"),
            "{bad:?}"
        );
        assert_eq!(fake.peek(0x2000_0040, 2), vec![Some(7), Some(8)]);
    }
}

#[test]
fn flash_file_roundtrip_and_garbage() {
    let dir = TempDir::new("evo-gui-tests-fake-flash", true);
    let mut fake = FakeBackend::new();
    let probe = fake.probe();
    let path = dir.join("calc.flash");
    fake.export_flash(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"FAKEFLASH1");
    fake.run_for(500);
    fake.import_flash(&path).unwrap();
    assert_eq!(fake.cycles(), 0);
    assert_eq!(probe.flash_imports(), 1);

    fake.run_for(500);
    let bad = dir.join("bad.flash");
    std::fs::write(&bad, b"FAKEFLASH1 and more").unwrap();
    assert!(matches!(
        fake.import_flash(&bad),
        Err(BackendError::BadState(m)) if m == "not a flash image"
    ));
    assert!(matches!(
        fake.import_flash(&dir.join("missing.flash")),
        Err(BackendError::Io(_))
    ));
    assert!(matches!(
        fake.import_flash(&dir),
        Err(BackendError::Io(_) | BackendError::BadState(_))
    ));
    assert!(matches!(
        fake.export_flash(&dir.join("no-such-dir").join("x.flash")),
        Err(BackendError::Io(_))
    ));
    assert_eq!(fake.cycles(), 500);
    assert_eq!(probe.flash_imports(), 1);
}

#[test]
fn set_breakpoints_sorts_and_dedups() {
    let mut fake = FakeBackend::new();
    fake.toggle_breakpoint(0x10);
    fake.set_breakpoints(vec![0x30, 0x20, 0x30, 0x10]);
    assert_eq!(fake.breakpoints(), vec![0x10, 0x20, 0x30]);
    fake.set_breakpoints(vec![]);
    assert!(fake.breakpoints().is_empty());
}

fn unsupported<T>(r: Result<T, BackendError>) -> bool {
    matches!(r, Err(BackendError::Unsupported(_)))
}

#[test]
fn placeholder_is_inert() {
    let mut p = Placeholder::new("calc.84b2", "machine:1");
    assert_eq!(p.name(), "calc.84b2");
    assert_eq!(p.state_tag(), "machine:1");
    let none = Placeholder::no_rom();
    assert_eq!(none.name(), "No ROM");
    assert_eq!(none.state_tag(), "none");
    assert!(p.framebuffer().is_none());
    assert_eq!(p.backlight(), 0.0);
    assert_eq!(p.run_for(1000), RunStatus::Idle);
    assert_eq!(p.step(), RunStatus::Idle);
    assert_eq!(p.cycles(), 0);
    assert!(!p.needs_provision());
    assert!(p.memory_regions().is_empty());
    assert!(p.peripherals().is_empty());
    assert!(p.bus_records().is_empty());
    assert!(p.watchpoints().is_empty());
    assert!(p.take_key_events().is_empty());
    assert!(p.take_uart2().is_empty());
    assert_eq!(p.peek(0, 2), vec![None, None]);
    let x = std::path::Path::new("x");
    assert!(unsupported(p.save_state()));
    assert!(unsupported(p.load_state(b"{}")));
    assert!(unsupported(p.export_ram()));
    assert!(unsupported(p.import_ram(b"x")));
    assert!(unsupported(p.export_flash(x)));
    assert!(unsupported(p.import_flash(x)));
    assert!(unsupported(p.poke(0x2000_0000, &[1])));
    assert!(unsupported(p.set_core_reg(0, 1)));
    assert!(unsupported(p.set_banked_reg("msp_s", 1)));
    assert!(unsupported(p.begin_goal(Goal::StepOut)));
    assert!(unsupported(p.set_watchpoints(vec![])));
    assert!(unsupported(p.set_hardware(HwChange::BatteryOk(false))));
    assert!(unsupported(p.provision()));
}
