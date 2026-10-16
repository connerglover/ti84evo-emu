//! USB controller packets, DMA, interrupt masks and Type-C registers.

use evo_hw::dev::usb::{USB_CONFIG, USB_ID, Usb, UsbError, UsbTypeC};
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
    assert_eq!(h.read(&mut u, 0x1B0, 4), 0, "FIFO reset completes");
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

fn connected() -> (DevHarness, Usb) {
    let mut h = DevHarness::new().with_buffer("sram", vec![0; 4096]);
    let mut u = Usb::new();
    h.with_ctx(|ctx| u.attach(true, ctx));
    h.write(&mut u, 0x100, 4, 0x24);
    (h, u)
}

#[test]
fn reset_and_setup_use_masks_and_write_one_acknowledgement() {
    let (mut h, mut u) = connected();
    h.write(&mut u, 0x13C, 4, 1);
    h.with_ctx(|ctx| u.bus_reset(ctx)).unwrap();
    assert_eq!(h.irq, 0);
    assert_eq!(h.read(&mut u, 0x14C, 4), 1);
    h.write(&mut u, 0x13C, 4, 0);
    assert_eq!(h.irq, 1 << 16);
    h.write(&mut u, 0x14C, 1, 1);
    assert_eq!(h.irq, 0);
    h.with_ctx(|ctx| u.host_setup([0x80, 6, 0, 1, 0, 0, 18, 0], ctx))
        .unwrap();
    assert_eq!(h.irq, 1 << 16);
    assert_eq!(h.read(&mut u, 0x1D0, 4), 0x01000680);
    assert_eq!(h.read(&mut u, 0x1D0, 4), 18 << 16);
    assert_eq!(h.irq, 0);
}

#[test]
fn setup_and_response_traverse_virtual_dma() {
    let (mut h, mut u) = connected();
    let setup = [0x80, 6, 0, 1, 0, 0, 18, 0];
    h.with_ctx(|ctx| u.host_setup(setup, ctx)).unwrap();
    h.write(&mut u, 0x304, 4, 0x20000010);
    h.write(&mut u, 0x300, 4, (8 << 8) | 1);
    assert_eq!(&h.buffer("sram")[16..24], &setup);
    assert_eq!(h.read(&mut u, 0x328, 4), 1);
    h.write(&mut u, 0x328, 4, 1);
    h.write(&mut u, 0x304, 4, 0x30000010);
    h.write(&mut u, 0x300, 4, (8 << 8) | 3);
    assert_eq!(h.with_ctx(|ctx| u.host_in(0, ctx)).unwrap(), setup);
    assert_eq!(h.read(&mut u, 0x300, 4) & 1, 0);
}

#[test]
fn bulk_out_dma_waits_for_packets_and_ends_on_short_packet() {
    let (mut h, mut u) = connected();
    // Endpoint 2 OUT -> FIFO1; FIFO1's virtual DMA is +0x310.
    h.write(&mut u, 0x184, 4, 64);
    h.write(&mut u, 0x1A0, 4, 0x1000);
    h.write(&mut u, 0x1A8, 4, 0x0200);
    h.write(&mut u, 0x1AC, 4, 0x2200);
    h.write(&mut u, 0x314, 4, 0x20000100);
    h.write(&mut u, 0x310, 4, (128 << 8) | 1);
    assert_eq!(h.read(&mut u, 0x328, 4), 0, "no fabricated completion");
    h.with_ctx(|ctx| u.host_out(2, &[0x55; 64], ctx)).unwrap();
    assert_ne!(h.read(&mut u, 0x310, 4) & 1, 0);
    h.with_ctx(|ctx| u.host_out(2, &[0x66; 3], ctx)).unwrap();
    assert_eq!(&h.buffer("sram")[0x100..0x140], &[0x55; 64]);
    assert_eq!(&h.buffer("sram")[0x140..0x143], &[0x66; 3]);
    assert_eq!(h.read(&mut u, 0x310, 4), 61 << 8);
    assert_eq!(h.read(&mut u, 0x328, 4), 4);
}

#[test]
fn dma_errors_do_not_write_outside_sram_and_pending_packets_restore() {
    let (mut h, mut u) = connected();
    let setup = [0x80, 6, 0, 1, 0, 0, 8, 0];
    h.with_ctx(|ctx| u.host_setup(setup, ctx)).unwrap();
    h.write(&mut u, 0x304, 4, 0x20000FFF);
    h.write(&mut u, 0x300, 4, (8 << 8) | 1);
    assert_eq!(h.read(&mut u, 0x328, 4), 1 << 16);
    assert!(h.buffer("sram").iter().all(|b| *b == 0));
    let state = u.save();
    let mut restored = Usb::new();
    restored.load(&state).unwrap();
    assert_eq!(h.read(&mut restored, 0x1D0, 4), 0x01000680);
    assert!(restored.load(&[state, vec![0]].concat()).is_err());
    assert_eq!(h.read(&mut restored, 0x1D0, 4), 8 << 16);
}

#[test]
fn packet_errors_stall_recovery_and_zero_length_status() {
    let (mut h, mut u) = connected();
    assert_eq!(
        h.with_ctx(|ctx| u.host_in(9, ctx)),
        Err(UsbError::InvalidEndpoint)
    );
    h.write(&mut u, 0x120, 4, 4);
    assert_eq!(h.with_ctx(|ctx| u.host_in(0, ctx)), Err(UsbError::Stall));
    h.with_ctx(|ctx| u.host_setup([0, 5, 1, 0, 0, 0, 0, 0], ctx))
        .unwrap();
    assert_eq!(h.with_ctx(|ctx| u.host_in(0, ctx)), Err(UsbError::Nak));
    h.write(&mut u, 0x120, 4, 1);
    assert_eq!(h.with_ctx(|ctx| u.host_in(0, ctx)), Ok(vec![]));
    h.with_ctx(|ctx| u.attach(false, ctx));
    assert_eq!(
        h.with_ctx(|ctx| u.host_in(0, ctx)),
        Err(UsbError::Disconnected)
    );
}

#[test]
fn bus_reset_cancels_dma_and_narrow_status_acknowledges_only_its_lane() {
    let (mut h, mut u) = connected();
    h.with_ctx(|ctx| u.host_setup([0, 0x20, 0, 0, 0, 0, 7, 0], ctx))
        .unwrap();
    // Invalid DMA address raises the CX error at bit 16. Low-byte W1C cannot clear it.
    h.write(&mut u, 0x304, 4, 0xF000_0010);
    h.write(&mut u, 0x300, 4, (8 << 8) | 1);
    assert_eq!(h.read(&mut u, 0x328, 4), 1 << 16);
    h.write(&mut u, 0x328, 1, 0xFF);
    assert_eq!(h.read(&mut u, 0x328, 4), 1 << 16);
    h.write(&mut u, 0x32A, 1, 1);
    assert_eq!(h.read(&mut u, 0x328, 4), 0);
    h.write(&mut u, 0x314, 4, 0x2000_0100);
    h.write(&mut u, 0x310, 4, (128 << 8) | 1);
    assert_eq!(h.read(&mut u, 0x310, 4) & 1, 1);
    h.with_ctx(|ctx| u.bus_reset(ctx)).unwrap();
    assert_eq!(h.read(&mut u, 0x310, 4), 0);
    assert_eq!(h.read(&mut u, 0x144, 4), 0);
    assert_eq!(h.read(&mut u, 0x14C, 4), 1);
    assert_eq!(h.read(&mut u, 0x1D0, 4), 0);
}
