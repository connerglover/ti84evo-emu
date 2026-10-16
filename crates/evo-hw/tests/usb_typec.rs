//! TCPCI attach events observed by the Evo BSP, distinct from USB endpoint traffic.

use evo_hw::dev::usb::UsbTypeC;
use evo_hw::{DevHarness, Device, ResetKind};

#[test]
fn drp_search_stays_unsettled_until_cable_arrives() {
    let mut h = DevHarness::new();
    let mut t = UsbTypeC::new();
    h.write(&mut t, 0x1A, 1, 0x4A);
    h.write(&mut t, 0x23, 1, 0x99);
    assert_eq!(h.read(&mut t, 0x1D, 1), 0x20);
    h.with_ctx(|ctx| t.attach_host(true, ctx));
    assert_eq!(h.read(&mut t, 0x1D, 1), 0x11);
}

#[test]
fn host_attach_reports_sink_cc_vbus_and_masked_alerts() {
    let mut h = DevHarness::new();
    let mut t = UsbTypeC::new();
    h.write(&mut t, 0x14, 1, 0x45);
    h.with_ctx(|ctx| t.attach_host(true, ctx));
    assert_eq!(h.read(&mut t, 0x1D, 1), 0x11);
    assert_eq!(h.read(&mut t, 0x1E, 1) & 4, 4);
    assert_eq!(h.read(&mut t, 0x10, 2), 3);
    assert_eq!(h.irq, 0, "alerts latch while masked");
    h.write(&mut t, 0x12, 2, 3);
    assert_eq!(h.irq, 1 << 28);
    h.write(&mut t, 0x10, 1, 1);
    assert_eq!(h.read(&mut t, 0x10, 2), 2);
    assert_eq!(h.irq, 1 << 28, "power alert remains");
    h.write(&mut t, 0x10, 2, 2);
    assert_eq!(h.irq, 0);
    h.with_ctx(|ctx| t.attach_host(true, ctx));
    assert_eq!(
        h.read(&mut t, 0x10, 2),
        0,
        "unchanged cable creates no event"
    );
    h.with_ctx(|ctx| t.attach_host(false, ctx));
    assert_eq!(h.read(&mut t, 0x1D, 1), 0);
    assert_eq!(h.read(&mut t, 0x1E, 1) & 5, 0);
    assert_eq!(h.read(&mut t, 0x10, 2), 3);
    assert_eq!(h.irq, 1 << 28);
}

#[test]
fn alert_mask_and_power_status_mask_are_independent() {
    let mut h = DevHarness::new();
    let mut t = UsbTypeC::new();
    h.write(&mut t, 0x12, 2, 3);
    h.with_ctx(|ctx| t.attach_host(true, ctx));
    assert_eq!(h.read(&mut t, 0x10, 2), 1, "power status mask is zero");
    h.write(&mut t, 0x12, 1, 0);
    assert_eq!(
        h.irq, 0,
        "masking deasserts the interrupt without acknowledging"
    );
    h.write(&mut t, 0x12, 1, 1);
    assert_eq!(h.irq, 1 << 28);
    h.write(&mut t, 0x10, 4, 0x0003_0001);
    assert_eq!(h.read(&mut t, 0x10, 2), 0);
    assert_eq!(
        h.read(&mut t, 0x12, 2),
        3,
        "word write preserves mask semantics"
    );
    assert_eq!(h.irq, 0);
}

#[test]
fn host_cable_survives_reset_and_state_restore() {
    let mut h = DevHarness::new();
    let mut t = UsbTypeC::new();
    h.write(&mut t, 0x12, 2, 3);
    h.with_ctx(|ctx| t.attach_host(true, ctx));
    h.write(&mut t, 0x23, 1, 0x33);
    h.write(&mut t, 0x23, 1, 0x55);
    assert_eq!(h.read(&mut t, 0x1E, 1), 13);
    let mut restored = UsbTypeC::new();
    restored.load(&t.save()).unwrap();
    assert_eq!(h.read(&mut restored, 0x1D, 1), 0x11);
    assert_eq!(h.read(&mut restored, 0x1E, 1), 13);
    h.reset(&mut restored, ResetKind::System);
    assert_eq!(h.read(&mut restored, 0x1D, 1), 0x11);
    assert_eq!(h.read(&mut restored, 0x1E, 1), 4);
    assert_eq!(h.irq, 0, "reset masks interrupts");
    h.write(&mut restored, 0x12, 2, 1);
    assert_eq!(
        h.irq,
        1 << 28,
        "firmware can rediscover the connected cable"
    );
    h.reset(&mut restored, ResetKind::PowerOn);
    assert_eq!(h.read(&mut restored, 0x1D, 1), 0);
    assert_eq!(h.read(&mut restored, 0x1E, 1), 0);
    assert_eq!(h.read(&mut restored, 0x10, 2), 0);
    assert_eq!(h.irq, 0);
}
