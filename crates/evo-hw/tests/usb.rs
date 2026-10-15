//! USB OTG minimum and USB Type-C stub (`docs/hardware/usb.md`).

use evo_hw::dev::usb::{USB_CONFIG, USB_ID, Usb, UsbTypeC};
use evo_hw::{DevHarness, Device, MemoryMap, ResetKind, StateError};

const CTRL: u32 = 0x000;
const ID: u32 = 0xE0;
const CONFIG: u32 = 0xE4;
const TC_CC: u32 = 0x1C;

#[test]
fn id_and_config() {
    let mut h = DevHarness::new();
    let mut u = Usb::new();
    assert_eq!(h.read(&mut u, ID, 4), USB_ID);
    assert_eq!(h.read(&mut u, CONFIG, 4), USB_CONFIG);
    assert_eq!(USB_ID, 0x0001_0000);
    assert_eq!(USB_CONFIG, 0x0000_0880);
    // Read-only: writes are dropped.
    h.write(&mut u, ID, 4, 0xDEAD_BEEF);
    h.write(&mut u, CONFIG, 4, 0xDEAD_BEEF);
    assert_eq!(h.read(&mut u, ID, 4), USB_ID);
    assert_eq!(h.read(&mut u, CONFIG, 4), USB_CONFIG);
    // Narrow reads see the constant's lanes.
    assert_eq!(h.read(&mut u, ID + 2, 2), 0x0001);
    assert_eq!(h.read(&mut u, CONFIG + 1, 1), 0x08);
    assert_eq!(h.irq, 0, "IRQ 16 is never asserted");
    assert!(h.log.is_empty());
}

#[test]
fn soft_reset_self_clears() {
    let mut h = DevHarness::new();
    let mut u = Usb::new();
    h.write(&mut u, CTRL, 4, 0x6);
    assert_eq!(h.read(&mut u, CTRL, 4), 0x4);
    h.write(&mut u, CTRL, 1, 0x2);
    assert_eq!(h.read(&mut u, CTRL, 1), 0);
    assert!(h.log.is_empty());
}

#[test]
fn storage_elsewhere() {
    let mut h = DevHarness::new();
    let mut u = Usb::new();
    h.write(&mut u, 0x100, 4, 0x220);
    assert_eq!(h.read(&mut u, 0x100, 4), 0x220);
    h.write(&mut u, 0x1B0, 4, 0x1000);
    assert_eq!(h.read(&mut u, 0x1B0, 4), 0x1000);
    h.write(&mut u, 0xFFC, 4, 0x1234_5678);
    assert_eq!(h.read(&mut u, 0xFFE, 2), 0x1234);
    assert!(h.log.is_empty());
    // Past the end of the window: unhandled, reads 0, never panics.
    assert_eq!(h.read(&mut u, 0xFFE, 4), 0);
    h.write(&mut u, 0x1000, 4, 1);
    assert_eq!(h.log.len(), 2);
}

#[test]
fn typec_byte_storage() {
    let mut h = DevHarness::new();
    let mut t = UsbTypeC::new();
    h.write(&mut t, 0x1A4, 1, 0x95);
    assert_eq!(h.read(&mut t, 0x1A4, 4), 0x95);
    assert_eq!(h.read(&mut t, TC_CC, 4), 0);
    h.write(&mut t, 0x22, 2, 0xBEEF);
    assert_eq!(h.read(&mut t, 0x20, 4), 0xBEEF_0000);
    assert_eq!(h.irq, 0, "IRQ 28 is never asserted");
    assert!(h.log.is_empty());
}

#[test]
fn typec_cc_is_host_controlled() {
    let mut h = DevHarness::new();
    let mut t = UsbTypeC::new();
    h.write(&mut t, TC_CC, 4, 0xFFFF_FFFF);
    assert_eq!(h.read(&mut t, TC_CC, 4), 0xFFFF_F0FF);
    t.set_cc(1, 2);
    assert_eq!(h.read(&mut t, TC_CC, 4), 0xFFFF_F9FF);
    assert_eq!(t.cc(), (1, 2));
    // Byte and halfword reads covering bits 11:8 see the CC state too.
    assert_eq!(h.read(&mut t, TC_CC + 1, 1), 0xF9);
    assert_eq!(h.read(&mut t, TC_CC, 2), 0xF9FF);
    // A byte write to +0x1D drops bits 11:8.
    h.write(&mut t, TC_CC + 1, 1, 0x00);
    assert_eq!(h.read(&mut t, TC_CC, 4), 0xFFFF_09FF);
    // Values are masked to 2 bits each.
    t.set_cc(0xFF, 0xFD);
    assert_eq!(t.cc(), (3, 1));
    assert_eq!(h.read(&mut t, TC_CC, 4) & 0xF00, 0x700);
    assert!(h.log.is_empty());
}

#[test]
fn reset_keeps_host_cc() {
    let mut h = DevHarness::new();
    let mut t = UsbTypeC::new();
    h.write(&mut t, 0x84, 4, 0x55);
    t.set_cc(2, 1);
    h.reset(&mut t, ResetKind::System);
    assert_eq!(h.read(&mut t, 0x84, 4), 0);
    assert_eq!(t.cc(), (2, 1));
    assert_eq!(h.read(&mut t, TC_CC, 4), 0x600);
    h.reset(&mut t, ResetKind::PowerOn);
    assert_eq!(t.cc(), (0, 0));

    let mut u = Usb::new();
    h.write(&mut u, 0x100, 4, 0x220);
    h.reset(&mut u, ResetKind::System);
    assert_eq!(h.read(&mut u, 0x100, 4), 0);
    assert_eq!(h.read(&mut u, ID, 4), USB_ID);
}

#[test]
fn state_roundtrip() {
    let mut h = DevHarness::new();

    let mut u = Usb::new();
    h.write(&mut u, 0x100, 4, 0x220);
    let st = u.save();
    let mut u2 = Usb::new();
    u2.load(&st).unwrap();
    assert_eq!(h.read(&mut u2, 0x100, 4), 0x220);
    assert!(u2.load(&[0xFF; 3]).is_err());
    assert!(matches!(
        u2.load(&[st.clone(), vec![0]].concat()),
        Err(StateError::Mismatch(_))
    ));
    assert_eq!(h.read(&mut u2, 0x100, 4), 0x220, "failed load leaves state");

    let mut t = UsbTypeC::new();
    h.write(&mut t, 0x1A4, 1, 0x95);
    t.set_cc(3, 1);
    let ts = t.save();
    let mut t2 = UsbTypeC::new();
    t2.load(&ts).unwrap();
    assert_eq!(h.read(&mut t2, 0x1A4, 4), 0x95);
    assert_eq!(t2.cc(), (3, 1));
    assert!(t2.load(&[0xFF; 3]).is_err());
    assert_eq!(t2.cc(), (3, 1));
}

#[test]
fn soc_attaches_usb_blocks() {
    let mut map = MemoryMap::new();
    evo_hw::soc::attach(&mut map);
    assert!(map.device::<Usb>("usb").is_some());
    assert!(map.device::<UsbTypeC>("usbtc").is_some());
}
