//! GPIO controller at `0x4010_5000` (`docs/hardware/gpio.md` "Minimal model").

use evo_hw::dev::gpio::{GPIO_IRQ, GPIO_PINS, Gpio};
use evo_hw::{DevHarness, Device, MemoryMap, ResetKind, StateError};

const DATA_OUT: u32 = 0x00;
const DATA_IN: u32 = 0x04;
const DIR: u32 = 0x08;
const SET: u32 = 0x10;
const CLR: u32 = 0x14;
const PULL_EN: u32 = 0x18;
const PULL_SEL: u32 = 0x1C;
const INT_EN: u32 = 0x20;
const INT_STATUS: u32 = 0x28;
const INT_CLR: u32 = 0x30;
const INT_TYPE: u32 = 0x34;
const INT_BOTH: u32 = 0x38;
const INT_POL: u32 = 0x3C;

const IRQ_BIT: u64 = 1 << 24;

fn set_input(h: &mut DevHarness, g: &mut Gpio, pin: u8, level: Option<bool>) {
    h.with_ctx(|ctx| g.set_input(pin, level, ctx));
}

#[test]
fn constants() {
    assert_eq!(GPIO_PINS, 13);
    assert_eq!(GPIO_IRQ, 24);
}

#[test]
fn set_clr_and_readback() {
    let mut h = DevHarness::new();
    let mut g = Gpio::new();
    h.write(&mut g, DIR, 4, 2);
    assert_eq!(h.read(&mut g, DIR, 4), 2);
    assert!(!g.power_hold());
    h.write(&mut g, SET, 4, 2);
    assert_eq!(h.read(&mut g, DATA_OUT, 4), 2);
    assert_eq!(h.read(&mut g, DATA_IN, 4) & 2, 2);
    assert!(g.pin_level(1));
    assert!(g.power_hold());
    h.write(&mut g, CLR, 4, 2);
    assert_eq!(h.read(&mut g, DATA_OUT, 4), 0);
    assert_eq!(h.read(&mut g, DATA_IN, 4) & 2, 0);
    assert!(!g.power_hold());
    // DATA_OUT is also written directly.
    h.write(&mut g, DATA_OUT, 4, 0x1003);
    assert_eq!(h.read(&mut g, DATA_OUT, 4), 0x1003);
    assert!(g.power_hold());
    assert!(h.log.is_empty());
}

#[test]
fn pull_up_input() {
    let mut h = DevHarness::new();
    let mut g = Gpio::new();
    assert_eq!(h.read(&mut g, DATA_IN, 4), 0, "undriven inputs read 0");
    h.write(&mut g, PULL_EN, 4, 1 << 3);
    h.write(&mut g, PULL_SEL, 4, 1 << 3);
    assert_eq!(h.read(&mut g, PULL_EN, 4), 1 << 3);
    assert_eq!(h.read(&mut g, PULL_SEL, 4), 1 << 3);
    assert_eq!(h.read(&mut g, DATA_IN, 4), 1 << 3);
    assert!(g.pin_level(3));
    // PULL_SEL without PULL_EN does nothing.
    h.write(&mut g, PULL_EN, 4, 0);
    assert_eq!(h.read(&mut g, DATA_IN, 4), 0);
}

#[test]
fn host_input_overrides_pull() {
    let mut h = DevHarness::new();
    let mut g = Gpio::new();
    h.write(&mut g, PULL_EN, 4, 1 << 4);
    h.write(&mut g, PULL_SEL, 4, 1 << 4);
    set_input(&mut h, &mut g, 4, Some(false));
    assert_eq!(h.read(&mut g, DATA_IN, 4), 0);
    set_input(&mut h, &mut g, 4, Some(true));
    assert_eq!(h.read(&mut g, DATA_IN, 4), 1 << 4);
    // Undriven again: the pull-up decides.
    h.write(&mut g, PULL_SEL, 4, 0);
    set_input(&mut h, &mut g, 4, None);
    assert_eq!(h.read(&mut g, DATA_IN, 4), 0);
    // An output ignores the host input.
    set_input(&mut h, &mut g, 4, Some(true));
    h.write(&mut g, DIR, 4, 1 << 4);
    assert_eq!(h.read(&mut g, DATA_IN, 4), 0);
    assert!(!g.pin_level(4));
}

#[test]
fn both_edges_irq() {
    let mut h = DevHarness::new();
    let mut g = Gpio::new();
    h.write(&mut g, INT_BOTH, 4, 1 << 9);
    h.write(&mut g, INT_EN, 4, 1 << 9);
    assert_eq!(h.read(&mut g, INT_BOTH, 4), 1 << 9);
    assert_eq!(h.read(&mut g, INT_EN, 4), 1 << 9);
    assert_eq!(h.irq, 0);
    set_input(&mut h, &mut g, 9, Some(true));
    assert_eq!(h.read(&mut g, INT_STATUS, 4), 1 << 9);
    assert_eq!(h.irq, IRQ_BIT);
    h.write(&mut g, INT_CLR, 4, 1 << 9);
    assert_eq!(h.read(&mut g, INT_STATUS, 4), 0);
    assert_eq!(h.irq, 0);
    // The same level again is no edge.
    set_input(&mut h, &mut g, 9, Some(true));
    assert_eq!(h.irq, 0);
    set_input(&mut h, &mut g, 9, Some(false));
    assert_eq!(h.read(&mut g, INT_STATUS, 4), 1 << 9);
    assert_eq!(h.irq, IRQ_BIT);
    // Disabling the pin masks the status and drops the line.
    h.write(&mut g, INT_EN, 4, 0);
    assert_eq!(h.read(&mut g, INT_STATUS, 4), 0);
    assert_eq!(h.irq, 0);
}

#[test]
fn level_irq_holds() {
    let mut h = DevHarness::new();
    let mut g = Gpio::new();
    h.write(&mut g, INT_TYPE, 4, 1 << 11);
    h.write(&mut g, INT_POL, 4, 1 << 11);
    // TYPE = 1 with POL still 0 latched the low level; the driver clears before enabling.
    h.write(&mut g, INT_CLR, 4, 1 << 11);
    h.write(&mut g, INT_EN, 4, 1 << 11);
    assert_eq!(h.read(&mut g, INT_TYPE, 4), 1 << 11);
    assert_eq!(h.read(&mut g, INT_POL, 4), 1 << 11);
    assert_eq!(h.irq, 0);
    set_input(&mut h, &mut g, 11, Some(true));
    assert_eq!(h.read(&mut g, INT_STATUS, 4), 1 << 11);
    assert_eq!(h.irq, IRQ_BIT);
    h.write(&mut g, INT_CLR, 4, 1 << 11);
    assert_eq!(
        h.read(&mut g, INT_STATUS, 4),
        1 << 11,
        "W1C does not clear while the level holds"
    );
    assert_eq!(h.irq, IRQ_BIT);
    set_input(&mut h, &mut g, 11, Some(false));
    h.write(&mut g, INT_CLR, 4, 1 << 11);
    assert_eq!(h.read(&mut g, INT_STATUS, 4), 0);
    assert_eq!(h.irq, 0);
}

#[test]
fn falling_only() {
    let mut h = DevHarness::new();
    let mut g = Gpio::new();
    h.write(&mut g, INT_EN, 4, 1 << 5);
    set_input(&mut h, &mut g, 5, Some(true));
    assert_eq!(h.read(&mut g, INT_STATUS, 4), 0, "rising edge ignored");
    assert_eq!(h.irq, 0);
    set_input(&mut h, &mut g, 5, Some(false));
    assert_eq!(h.read(&mut g, INT_STATUS, 4), 1 << 5);
    assert_eq!(h.irq, IRQ_BIT);
}

#[test]
fn rising_only() {
    let mut h = DevHarness::new();
    let mut g = Gpio::new();
    h.write(&mut g, INT_POL, 4, 1 << 6);
    h.write(&mut g, INT_EN, 4, 1 << 6);
    set_input(&mut h, &mut g, 6, Some(true));
    assert_eq!(h.read(&mut g, INT_STATUS, 4), 1 << 6);
    h.write(&mut g, INT_CLR, 4, 1 << 6);
    set_input(&mut h, &mut g, 6, Some(false));
    assert_eq!(h.read(&mut g, INT_STATUS, 4), 0, "falling edge ignored");
}

#[test]
fn power_off_once() {
    let mut h = DevHarness::new();
    let mut g = Gpio::new();
    h.write(&mut g, DIR, 4, 2);
    h.write(&mut g, SET, 4, 2);
    assert!(!g.take_power_off());
    h.write(&mut g, CLR, 4, 2);
    assert!(g.take_power_off());
    assert!(!g.take_power_off(), "drained");
    // Another release reports again.
    h.write(&mut g, SET, 4, 2);
    h.write(&mut g, DATA_OUT, 4, 0);
    assert!(g.take_power_off());
    // Pin 1 as an input going low is not a power-off.
    let mut g = Gpio::new();
    set_input(&mut h, &mut g, 1, Some(true));
    set_input(&mut h, &mut g, 1, Some(false));
    assert!(!g.take_power_off());
}

#[test]
fn out_of_range_pin_ignored() {
    let mut h = DevHarness::new();
    let mut g = Gpio::new();
    h.write(&mut g, INT_BOTH, 4, 0xFFFF_FFFF);
    h.write(&mut g, INT_EN, 4, 0xFFFF_FFFF);
    h.write(&mut g, INT_CLR, 4, 0xFFFF_FFFF);
    assert_eq!(h.read(&mut g, INT_EN, 4), 0x1FFF, "13 bits implemented");
    for pin in [13, 31, 32, 255] {
        set_input(&mut h, &mut g, pin, Some(true));
        assert!(!g.pin_level(pin));
    }
    assert_eq!(h.read(&mut g, DATA_IN, 4), 0);
    assert_eq!(h.read(&mut g, INT_STATUS, 4), 0);
    assert_eq!(h.irq, 0);
    // Registers outside the model, narrow and unaligned accesses: 0, logged, no panic.
    assert_eq!(h.read(&mut g, 0x0C, 4), 0);
    assert_eq!(h.read(&mut g, 0x40, 4), 0);
    assert_eq!(h.read(&mut g, DIR, 1), 0);
    h.write(&mut g, DIR, 2, 0xFFFF);
    h.write(&mut g, 0x0A, 4, 1);
    h.write(&mut g, INT_STATUS, 4, 1);
    h.write(&mut g, 0xFFC, 4, 1);
    assert_eq!(h.read(&mut g, DIR, 4), 0);
    assert_eq!(h.log.len(), 7);
}

#[test]
fn system_reset_keeps_host_inputs() {
    let mut h = DevHarness::new();
    let mut g = Gpio::new();
    set_input(&mut h, &mut g, 9, Some(true));
    h.write(&mut g, INT_TYPE, 4, 1 << 9);
    h.write(&mut g, INT_POL, 4, 1 << 9);
    h.write(&mut g, INT_EN, 4, 1 << 9);
    h.write(&mut g, DIR, 4, 2);
    h.write(&mut g, SET, 4, 2);
    assert_eq!(h.irq, IRQ_BIT);
    h.reset(&mut g, ResetKind::System);
    assert_eq!(h.irq, 0);
    for off in [DATA_OUT, DIR, INT_EN, INT_TYPE, INT_POL, INT_STATUS] {
        assert_eq!(h.read(&mut g, off, 4), 0, "{off:#x}");
    }
    assert!(!g.power_hold());
    assert!(!g.take_power_off(), "a reset is not a release");
    assert_eq!(h.read(&mut g, DATA_IN, 4), 1 << 9, "host input kept");
    h.reset(&mut g, ResetKind::PowerOn);
    assert_eq!(h.read(&mut g, DATA_IN, 4), 0, "power-on drops host inputs");
}

#[test]
fn state_roundtrip() {
    let mut h = DevHarness::new();
    let mut g = Gpio::new();
    h.write(&mut g, DIR, 4, 2);
    h.write(&mut g, SET, 4, 2);
    h.write(&mut g, INT_BOTH, 4, 1 << 9);
    h.write(&mut g, INT_EN, 4, 1 << 9);
    set_input(&mut h, &mut g, 9, Some(true));
    let st = g.save();
    let mut g2 = Gpio::new();
    g2.load(&st).unwrap();
    assert!(g2.power_hold());
    assert_eq!(h.read(&mut g2, INT_STATUS, 4), 1 << 9);
    assert_eq!(h.read(&mut g2, DATA_IN, 4), (1 << 9) | 2);
    assert!(g2.load(&[0xFF; 3]).is_err());
    assert!(matches!(
        g2.load(&[st.clone(), vec![0]].concat()),
        Err(StateError::Mismatch(_))
    ));
    assert_eq!(
        h.read(&mut g2, DATA_IN, 4),
        (1 << 9) | 2,
        "failed load leaves state"
    );
}

#[test]
fn soc_attaches_gpio() {
    let mut map = MemoryMap::new();
    evo_hw::soc::attach(&mut map);
    assert!(map.device::<Gpio>("gpio").is_some());
}
