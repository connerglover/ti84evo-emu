//! The device runtime: IRQ lines, event requests, time, buffers, reset and device state.

use armv8m::{Access, AccessKind, Bus};
use evo_hw::{
    DevCtx, DevHarness, Device, MemoryMap, ResetKind, StateError, UnmappedAccess, UnmappedKind,
    load_serde, save_serde,
};
use serde::{Deserialize, Serialize};

fn write_acc(secure: bool) -> Access {
    Access {
        kind: AccessKind::Write,
        secure,
        privileged: true,
    }
}

/// Register map of the test device (offsets):
/// +0x0 write: v = irq number to assert; +0x4 write: irq number to deassert;
/// +0x8 write: schedule_at(v); +0xC write: cancel_event; +0x10 write: unhandled(0x3C, 4, v);
/// +0x14 write: zero byte v of buffer "flash"; +0x18 write: pulse_irq(v).
/// Reads return the access counter.
#[derive(Default, Serialize, Deserialize)]
struct Counter {
    accesses: u32,
    events: u32,
    last_now: u64,
    resets: Vec<ResetKind>,
}

impl Device for Counter {
    fn read(&mut self, _off: u32, _width: u8, _ctx: &mut DevCtx<'_>) -> u32 {
        self.accesses += 1;
        self.accesses
    }
    fn write(&mut self, off: u32, _width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        self.accesses += 1;
        match off {
            0x0 => ctx.set_irq(v as u16, true),
            0x4 => ctx.set_irq(v as u16, false),
            0x8 => ctx.schedule_at(u64::from(v)),
            0xC => ctx.cancel_event(),
            0x10 => ctx.unhandled(0x3C, 4, Some(v)),
            0x14 => {
                if let Some(b) = ctx.buffer_mut("flash") {
                    b[v as usize] = 0;
                }
            }
            0x18 => ctx.pulse_irq(v as u16),
            _ => ctx.unhandled(off, 4, Some(v)),
        }
    }
    fn event(&mut self, ctx: &mut DevCtx<'_>) {
        self.events += 1;
        self.last_now = ctx.now;
    }
    fn reset(&mut self, kind: ResetKind, _ctx: &mut DevCtx<'_>) {
        self.resets.push(kind);
    }
    fn save(&self) -> Vec<u8> {
        save_serde(self)
    }
    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        load_serde(self, state)
    }
}

/// A second device type, for downcast tests.
#[derive(Default)]
struct Other;

impl Device for Other {
    fn read(&mut self, _off: u32, _width: u8, _ctx: &mut DevCtx<'_>) -> u32 {
        0
    }
    fn write(&mut self, _off: u32, _width: u8, _v: u32, _ctx: &mut DevCtx<'_>) {}
    fn reset(&mut self, _kind: ResetKind, _ctx: &mut DevCtx<'_>) {}
    fn save(&self) -> Vec<u8> {
        Vec::new()
    }
    fn load(&mut self, _state: &[u8]) -> Result<(), StateError> {
        Ok(())
    }
}

const A: u32 = 0x4000_0000;
const B: u32 = 0x4000_1000;

fn two_counters() -> MemoryMap {
    let mut m = MemoryMap::new();
    assert_eq!(m.add_device("a", A, 0x100, Box::new(Counter::default())), 0);
    assert_eq!(m.add_device("b", B, 0x100, Box::new(Counter::default())), 1);
    m
}

#[test]
fn irq_lines_or_across_devices() {
    let mut m = two_counters();
    m.write32(A, 5, write_acc(true)).unwrap();
    m.write32(B, 5, write_acc(true)).unwrap();
    assert_eq!(m.irq_lines(), 1 << 5);
    m.write32(A + 4, 5, write_acc(true)).unwrap();
    assert_eq!(m.irq_lines(), 1 << 5, "B still holds the line");
    m.write32(B + 4, 5, write_acc(true)).unwrap();
    assert_eq!(m.irq_lines(), 0);
    // Out-of-range IRQ numbers are ignored, not a panic.
    m.write32(A, 64, write_acc(true)).unwrap();
    m.write32(A + 0x18, 200, write_acc(true)).unwrap();
    assert_eq!(m.irq_lines(), 0);
    assert_eq!(m.take_pulses(), Vec::<u16>::new());
}

#[test]
fn pulses_drain() {
    let mut m = two_counters();
    m.write32(A + 0x18, 0, write_acc(true)).unwrap();
    m.write32(B + 0x18, 3, write_acc(true)).unwrap();
    assert_eq!(m.take_pulses(), vec![0, 3]);
    assert_eq!(m.take_pulses(), Vec::<u16>::new());
    assert_eq!(m.irq_lines(), 0, "a pulse is not a level");
}

#[test]
fn event_requests_drain_in_order() {
    let mut m = two_counters();
    m.write32(A + 8, 100, write_acc(true)).unwrap();
    m.write32(B + 8, 50, write_acc(true)).unwrap();
    m.write32(A + 0xC, 0, write_acc(true)).unwrap();
    assert_eq!(
        m.take_event_requests(),
        vec![(0, Some(100)), (1, Some(50)), (0, None)]
    );
    assert_eq!(m.take_event_requests(), vec![]);
}

#[test]
fn device_event_sees_now() {
    let mut m = two_counters();
    m.now = 777;
    m.device_event(0);
    let a = m.device::<Counter>("a").unwrap();
    assert_eq!(a.events, 1);
    assert_eq!(a.last_now, 777);
    assert_eq!(m.device::<Counter>("b").unwrap().events, 0);
    // Unknown id: ignored.
    m.device_event(99);
}

#[test]
fn with_device_downcasts() {
    let mut m = two_counters();
    m.add_device("other", 0x4000_2000, 0x100, Box::new(Other));
    m.now = 42;
    let r = m.with_device("a", |c: &mut Counter, ctx| {
        ctx.set_irq(7, true);
        c.accesses += 10;
        ctx.now
    });
    assert_eq!(r, Some(42));
    assert_eq!(m.device::<Counter>("a").unwrap().accesses, 10);
    assert_eq!(m.irq_lines(), 1 << 7);
    assert!(m.with_device("other", |_: &mut Counter, _| ()).is_none());
    assert!(m.with_device("nope", |_: &mut Counter, _| ()).is_none());
    assert!(m.with_device("other", |_: &mut Other, _| ()).is_some());
    assert!(m.device::<Other>("a").is_none());
    assert!(m.device::<Counter>("nope").is_none());
}

#[test]
fn unhandled_goes_to_log() {
    let mut m = MemoryMap::new();
    m.add_device("a", 0x4010_1000, 0x100, Box::new(Counter::default()));
    m.pc_hint = 0x1234;
    m.write32(0x4010_1010, 9, write_acc(true)).unwrap();
    let log: Vec<_> = m.log.entries().map(|(a, c)| (a.clone(), c)).collect();
    assert_eq!(
        log,
        vec![(
            UnmappedAccess {
                addr: 0x4010_103C,
                width: 4,
                write: Some(9),
                kind: UnmappedKind::Write,
                pc: 0x1234,
                secure: true,
            },
            1
        )]
    );
}

#[test]
fn buffer_mut_reaches_rom() {
    let mut m = MemoryMap::new();
    m.add_rom("flash", vec![0xFF; 16], &[0x0000_0000]);
    m.add_device("a", A, 0x100, Box::new(Counter::default()));
    m.write32(A + 0x14, 3, write_acc(true)).unwrap();
    assert_eq!(m.peek(3, 1), Some(0));
    assert_eq!(m.peek(2, 1), Some(0xFF));
    assert_eq!(m.buffer_mut("flash").unwrap()[3], 0);
    assert!(m.buffer_mut("nope").is_none());
    // A guest write to the ROM is still dropped and logged.
    m.write8(4, 0, write_acc(true)).unwrap();
    assert_eq!(m.peek(4, 1), Some(0xFF));
    assert_eq!(m.log.len(), 1);
}

#[test]
fn reset_devices_reaches_each() {
    let mut m = two_counters();
    m.reset_devices(ResetKind::System);
    for name in ["a", "b"] {
        assert_eq!(
            m.device::<Counter>(name).unwrap().resets,
            vec![ResetKind::System]
        );
    }
}

#[test]
fn device_states_roundtrip_and_mismatch() {
    let mut m = two_counters();
    m.write32(A, 3, write_acc(true)).unwrap();
    m.write32(B + 0x10, 1, write_acc(true)).unwrap();
    m.write32(B + 0x10, 1, write_acc(true)).unwrap();
    let states = m.device_states();
    assert_eq!(states.len(), 2);
    assert_eq!(states[0].name, "a");
    assert_eq!(states[0].irq_out, 1 << 3);

    let mut back = two_counters();
    back.load_device_states(&states).unwrap();
    assert_eq!(back.device::<Counter>("a").unwrap().accesses, 1);
    assert_eq!(back.device::<Counter>("b").unwrap().accesses, 2);
    assert_eq!(back.irq_lines(), 1 << 3);

    let mut other = MemoryMap::new();
    other.add_device("a", A, 0x100, Box::new(Counter::default()));
    other.add_device("c", B, 0x100, Box::new(Counter::default()));
    assert!(matches!(
        other.load_device_states(&states),
        Err(StateError::Mismatch(_))
    ));
    let mut short = MemoryMap::new();
    short.add_device("a", A, 0x100, Box::new(Counter::default()));
    assert!(matches!(
        short.load_device_states(&states),
        Err(StateError::Mismatch(_))
    ));
}

#[test]
fn bad_device_state_leaves_devices_unchanged() {
    let mut m = two_counters();
    m.write32(A, 3, write_acc(true)).unwrap();
    let mut states = m.device_states();
    states[1].state = vec![0xFF; 3]; // not a valid Counter
    let mut back = two_counters();
    back.write32(B, 9, write_acc(true)).unwrap();
    assert!(matches!(
        back.load_device_states(&states),
        Err(StateError::Decode(_))
    ));
    assert_eq!(back.device::<Counter>("a").unwrap().accesses, 0);
    assert_eq!(back.device::<Counter>("b").unwrap().accesses, 1);
    assert_eq!(back.irq_lines(), 1 << 9);
}

#[test]
fn load_serde_rejects_garbage() {
    let mut c = Counter::default();
    assert!(load_serde(&mut c, &[0xFF, 0xFF]).is_err());
    let mut bytes = save_serde(&Counter {
        accesses: 5,
        ..Counter::default()
    });
    load_serde(&mut c, &bytes).unwrap();
    assert_eq!(c.accesses, 5);
    bytes.push(0);
    assert!(matches!(
        load_serde(&mut c, &bytes),
        Err(StateError::Mismatch(_))
    ));
}

#[test]
fn harness_fire() {
    let mut h = DevHarness::new();
    let mut d = Counter::default();
    h.write(&mut d, 0x8, 4, 10);
    assert_eq!(h.event, Some(Some(10)));
    assert!(h.fire(&mut d));
    assert_eq!(h.now, 10);
    assert_eq!(h.event, None);
    assert_eq!(d.events, 1);
    assert_eq!(d.last_now, 10);
    assert!(!h.fire(&mut d));
    assert_eq!(d.events, 1);
}

#[test]
fn harness_lines_buffers_and_log() {
    let mut h = DevHarness::new().with_buffer("flash", vec![0xFF; 8]);
    let mut d = Counter::default();
    h.write(&mut d, 0x0, 4, 2);
    h.write(&mut d, 0x18, 4, 0);
    assert_eq!(h.irq, 1 << 2);
    assert_eq!(h.pulses, vec![0]);
    h.write(&mut d, 0x14, 4, 1);
    assert_eq!(
        h.buffer("flash"),
        &[0xFF, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
    );
    h.write(&mut d, 0x10, 4, 7);
    assert_eq!(h.log.len(), 1);
    assert_eq!(h.read(&mut d, 0, 1), 5, "fifth access");
    h.reset(&mut d, ResetKind::PowerOn);
    assert_eq!(d.resets, vec![ResetKind::PowerOn]);
    // A cancel leaves nothing to fire.
    h.write(&mut d, 0x8, 4, 5);
    h.write(&mut d, 0xC, 4, 0);
    assert_eq!(h.event, Some(None));
    assert!(!h.fire(&mut d));
}
