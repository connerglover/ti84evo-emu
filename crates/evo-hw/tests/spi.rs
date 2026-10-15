//! SPI master at 0x4010_7000 and its ST7789 panel responder (`docs/hardware/spi-panel.md`
//! "Minimal model specification").

use armv8m::{Access, AccessKind, Bus};
use evo_hw::dev::spi::{DEFAULT_PANEL_ID, SPI_FIFO_DEPTH, SPI_RX_CAP, Spi, St7789};
use evo_hw::{DevHarness, Device, MemoryMap, ResetKind};

const CTRL0: u32 = 0x00;
const CTRL1: u32 = 0x04;
const CTRL2: u32 = 0x08;
const STATUS: u32 = 0x0C;
const DATA: u32 = 0x18;
const INTEN: u32 = 0x1C;
const PARAM: u32 = 0x64;

/// 9-bit frames, divider 19: the firmware's CTRL1.
const CTRL1_9BIT: u32 = 0x0008_0012;

fn spi() -> Spi {
    Spi::new(St7789::new(DEFAULT_PANEL_ID))
}

/// A 9-bit, enabled SPI master.
fn enabled() -> (DevHarness, Spi) {
    let mut h = DevHarness::new();
    let mut s = spi();
    h.write(&mut s, CTRL1, 4, CTRL1_9BIT);
    h.write(&mut s, CTRL2, 4, 1);
    (h, s)
}

fn rx_level(h: &mut DevHarness, s: &mut Spi) -> u32 {
    (h.read(s, STATUS, 4) >> 4) & 0x3F
}

fn send(h: &mut DevHarness, s: &mut Spi, frames: &[u32]) {
    for &f in frames {
        h.write(s, DATA, 4, f);
    }
}

fn pop(h: &mut DevHarness, s: &mut Spi, n: usize) -> Vec<u32> {
    (0..n).map(|_| h.read(s, DATA, 4)).collect()
}

#[test]
fn status_at_reset() {
    let mut h = DevHarness::new();
    let mut s = spi();
    assert_eq!(h.read(&mut s, STATUS, 4), 0x2);
    assert_eq!(h.read(&mut s, PARAM, 4), 0x0007_0000);
    assert_eq!(SPI_FIFO_DEPTH, 8);
    assert_eq!(SPI_RX_CAP, 16);
    assert!(h.log.is_empty());
}

#[test]
fn control_registers_store() {
    let mut h = DevHarness::new();
    let mut s = spi();
    h.write(&mut s, CTRL0, 4, 0x0003_100F);
    h.write(&mut s, CTRL1, 4, CTRL1_9BIT);
    h.write(&mut s, INTEN, 4, 0x3FF);
    h.write(&mut s, CTRL2, 4, 0x18E);
    assert_eq!(h.read(&mut s, CTRL0, 4), 0x0003_100F);
    assert_eq!(h.read(&mut s, CTRL1, 4), CTRL1_9BIT);
    assert_eq!(s.ctrl1(), CTRL1_9BIT);
    assert_eq!(h.read(&mut s, INTEN, 4), 0x3FF);
    assert_eq!(h.read(&mut s, CTRL2, 4), 0x18E);
    assert!(h.log.is_empty());
    // Unmodeled offsets and narrow accesses read 0 and are logged.
    h.write(&mut s, 0x10, 4, 5);
    assert_eq!(h.read(&mut s, 0x10, 4), 0);
    assert_eq!(h.read(&mut s, CTRL1, 2), 0);
    assert_eq!(h.read(&mut s, 0xFFF, 1), 0);
    assert_eq!(h.log.len(), 4);
    assert_eq!(h.irq, 0);
}

#[test]
fn rddid_frames() {
    let mut h = DevHarness::new();
    let mut s = spi();
    h.write(&mut s, CTRL1, 4, CTRL1_9BIT);
    h.write(&mut s, CTRL2, 4, 0x183);
    send(&mut h, &mut s, &[0x004, 0, 0, 0]);
    assert_eq!(h.read(&mut s, STATUS, 4), 0x42);
    assert_eq!(pop(&mut h, &mut s, 4), [0x000, 0x085, 0x10A, 0x148]);
    assert_eq!(h.read(&mut s, STATUS, 4), 0x2);
    // The firmware's decode gives the ID back.
    let rx = [0x000u32, 0x085, 0x10A, 0x148];
    let id1 = rx[1] & 0xFF;
    let id2 = rx[2] >> 1;
    let id3 = ((rx[2] << 7) | (rx[3] >> 2)) & 0xFF;
    assert_eq!([id1, id2, id3], DEFAULT_PANEL_ID.map(u32::from));
}

#[test]
fn frames_wait_for_enable() {
    let mut h = DevHarness::new();
    let mut s = spi();
    h.write(&mut s, CTRL1, 4, CTRL1_9BIT);
    h.write(&mut s, CTRL2, 4, 0);
    send(&mut h, &mut s, &[0x004, 0]);
    assert_eq!(rx_level(&mut h, &mut s), 0);
    h.write(&mut s, CTRL2, 4, 1);
    assert_eq!(rx_level(&mut h, &mut s), 2);
    assert_eq!(pop(&mut h, &mut s, 2), [0x000, 0x085]);
}

#[test]
fn tx_fifo_full_drops_and_clears_not_full() {
    let mut h = DevHarness::new();
    let mut s = spi();
    h.write(&mut s, CTRL1, 4, CTRL1_9BIT);
    send(&mut h, &mut s, &[0; SPI_FIFO_DEPTH]);
    assert_eq!(h.read(&mut s, STATUS, 4), 0);
    send(&mut h, &mut s, &[0x0DA]);
    h.write(&mut s, CTRL2, 4, 1);
    assert_eq!(rx_level(&mut h, &mut s), SPI_FIFO_DEPTH as u32);
}

#[test]
fn flush_bit6() {
    let (mut h, mut s) = enabled();
    send(&mut h, &mut s, &[0x004, 0, 0]);
    assert_eq!(rx_level(&mut h, &mut s), 3);
    h.write(&mut s, CTRL2, 4, 0x40);
    assert_eq!(h.read(&mut s, STATUS, 4), 0x2);
    // Queued TX frames are flushed too.
    send(&mut h, &mut s, &[0x004, 0]);
    h.write(&mut s, CTRL2, 4, 0x40);
    h.write(&mut s, CTRL2, 4, 0x1);
    assert_eq!(h.read(&mut s, STATUS, 4), 0x2);
}

/// The firmware's transfer (0x100b4928..0x100b49ba): disable (`&~1, |0x40, &~0x180, &~2`),
/// queue the frames, then enable (`|0x180, |2, |1`) with read-modify-writes. Bit 6 is
/// self-clearing, so the enable writes do not flush the queued frames, and each MOSI frame
/// yields its MISO frame (the loop at 0x100b49c8 waits for all of them, without a timeout).
#[test]
fn queued_frames_survive_enable_sequence() {
    let (mut h, mut s) = enabled();
    let rmw = |h: &mut DevHarness, s: &mut Spi, clear: u32, set: u32| {
        let v = h.read(s, CTRL2, 4);
        h.write(s, CTRL2, 4, (v & !clear) | set);
    };
    rmw(&mut h, &mut s, 1, 0);
    rmw(&mut h, &mut s, 0, 0x40);
    assert_eq!(h.read(&mut s, CTRL2, 4) & 0x40, 0, "bit 6 reads 0");
    rmw(&mut h, &mut s, 0x180, 0);
    rmw(&mut h, &mut s, 2, 0);
    send(&mut h, &mut s, &[0x004, 0, 0, 0]);
    assert_eq!(rx_level(&mut h, &mut s), 0);
    rmw(&mut h, &mut s, 0, 0x180);
    rmw(&mut h, &mut s, 0, 2);
    assert_eq!(rx_level(&mut h, &mut s), 0, "still queued");
    rmw(&mut h, &mut s, 0, 1);
    assert_eq!(rx_level(&mut h, &mut s), 4);
    assert_eq!(pop(&mut h, &mut s, 4), [0x000, 0x085, 0x10A, 0x148]);
}

#[test]
fn rx_caps_at_16() {
    let (mut h, mut s) = enabled();
    send(&mut h, &mut s, &[0x0DA]);
    send(&mut h, &mut s, &[0x000; 19]);
    assert_eq!(rx_level(&mut h, &mut s), 16);
    // Frames 1..=4 (0x000, 0x10A, 0, 0) were dropped; the first left is the 5th frame's.
    assert_eq!(h.read(&mut s, DATA, 4), 0x000);
    assert_eq!(rx_level(&mut h, &mut s), 15);
}

#[test]
fn rdid_bytes_no_dummy() {
    let (mut h, mut s) = enabled();
    send(&mut h, &mut s, &[0x0DA, 0x000]);
    assert_eq!(pop(&mut h, &mut s, 2), [0x000, 0x10A]);
    send(&mut h, &mut s, &[0x0DC, 0x000]);
    assert_eq!(pop(&mut h, &mut s, 2), [0x000, 0x52 << 1]);
}

#[test]
fn diag_read_echoes_params() {
    let (mut h, mut s) = enabled();
    send(&mut h, &mut s, &[0x0C2, 0x101, 0x0C2, 0x000, 0x000]);
    assert_eq!(pop(&mut h, &mut s, 5), [0x000, 0x000, 0x000, 0x001, 0x000]);
    // New parameter frames replace the stored ones.
    send(&mut h, &mut s, &[0x0BB, 0x1AB, 0x1CD, 0x0BB, 0x000, 0x000]);
    assert_eq!(pop(&mut h, &mut s, 6), [0, 0, 0, 0, 0x0AB, 0xCD << 1]);
    send(&mut h, &mut s, &[0x0BB, 0x112, 0x0BB, 0x000, 0x000]);
    assert_eq!(pop(&mut h, &mut s, 5), [0, 0, 0, 0x012, 0]);
}

#[test]
fn status_reads() {
    let (mut h, mut s) = enabled();
    // RDDST: dummy + 4 zero bytes; 0x0A reports sleeping and display off at reset.
    send(&mut h, &mut s, &[0x009, 0, 0, 0, 0]);
    assert_eq!(pop(&mut h, &mut s, 5), [0; 5]);
    send(&mut h, &mut s, &[0x00A, 0]);
    assert_eq!(pop(&mut h, &mut s, 2), [0, 0]);
    send(&mut h, &mut s, &[0x011, 0x029, 0x00A, 0]);
    assert_eq!(pop(&mut h, &mut s, 4), [0, 0, 0, 0x14 << 1]);
}

#[test]
fn panel_tracks_commands() {
    let (mut h, mut s) = enabled();
    assert!(s.panel().sleeping());
    assert!(!s.panel().display_on());
    send(&mut h, &mut s, &[0x011, 0x029, 0x036, 0x128, 0x03A, 0x166]);
    let p = s.panel();
    assert!(!p.sleeping());
    assert!(p.display_on());
    assert_eq!(p.madctl(), 0x28);
    assert_eq!(p.colmod(), 0x66);
    send(&mut h, &mut s, &[0x028, 0x010]);
    assert!(s.panel().sleeping());
    assert!(!s.panel().display_on());
    // SWRESET restores the power-on state but keeps the ID.
    send(&mut h, &mut s, &[0x011, 0x029, 0x001]);
    let p = s.panel();
    assert!(p.sleeping());
    assert!(!p.display_on());
    assert_eq!(p.madctl(), 0);
    let _ = pop(&mut h, &mut s, SPI_RX_CAP);
    send(&mut h, &mut s, &[0x0DB, 0]);
    assert_eq!(pop(&mut h, &mut s, 2), [0, 0x85 << 1]);
}

#[test]
fn empty_read_is_zero() {
    let mut h = DevHarness::new();
    let mut s = spi();
    assert_eq!(h.read(&mut s, DATA, 4), 0);
    let (mut h, mut s) = enabled();
    assert_eq!(h.read(&mut s, DATA, 4), 0);
    assert_eq!(h.read(&mut s, STATUS, 4), 0x2);
    assert!(h.log.is_empty());
}

#[test]
fn nine_bit_mask() {
    let (mut h, mut s) = enabled();
    h.write(&mut s, DATA, 4, 0xFFFF);
    assert_eq!(s.panel().last_frame(), 0x1FF);
    // 8-bit frames mask to 0xFF.
    h.write(&mut s, CTRL1, 4, 0x0007_0012);
    h.write(&mut s, DATA, 4, 0xFFFF);
    assert_eq!(s.panel().last_frame(), 0xFF);
    // Absurd frame sizes do not panic.
    h.write(&mut s, CTRL1, 4, 0x007F_0000);
    h.write(&mut s, DATA, 4, 0xFFFF_FFFF);
    assert_eq!(s.panel().last_frame(), 0xFFFF);
}

#[test]
fn panel_clock_direct() {
    let (a, b, c) = (0x12u16, 0x34u16, 0xD6u16);
    let mut p = St7789::new([a as u8, b as u8, c as u8]);
    assert_eq!(p.clock(0x004, 9), 0);
    let f = [p.clock(0, 9), p.clock(0, 9), p.clock(0, 9)];
    assert_eq!(f, [a, (b << 1) | (c >> 7), (c << 2) & 0x1FF]);
    assert_eq!(f, [0x012, 0x069, 0x158]);
    assert_eq!(p.clock(0, 9), 0);
}

#[test]
fn reset_kinds() {
    let (mut h, mut s) = enabled();
    send(&mut h, &mut s, &[0x011, 0x029, 0x004]);
    h.write(&mut s, CTRL0, 4, 0x0003_100F);
    h.reset(&mut s, ResetKind::System);
    assert_eq!(h.read(&mut s, CTRL0, 4), 0);
    assert_eq!(s.ctrl1(), 0);
    assert_eq!(h.read(&mut s, CTRL2, 4), 0);
    assert_eq!(h.read(&mut s, STATUS, 4), 0x2);
    // The panel keeps its state across a System reset.
    assert!(!s.panel().sleeping());
    assert!(s.panel().display_on());
    h.reset(&mut s, ResetKind::PowerOn);
    assert!(s.panel().sleeping());
    assert!(!s.panel().display_on());
}

#[test]
fn state_roundtrip() {
    let (mut h, mut s) = enabled();
    send(&mut h, &mut s, &[0x011, 0x029, 0x036, 0x160, 0x0C5, 0x177]);
    // A read in progress: RDDID with one frame clocked out.
    send(&mut h, &mut s, &[0x004, 0x000]);
    h.write(&mut s, CTRL2, 4, 0);
    send(&mut h, &mut s, &[0x000]); // queued in TX while disabled
    let st = s.save();

    let mut fresh = Spi::new(St7789::new([0, 0, 0]));
    fresh.load(&st).unwrap();
    assert_eq!(fresh.ctrl1(), CTRL1_9BIT);
    assert_eq!(fresh.panel().madctl(), 0x60);
    assert!(fresh.panel().display_on());
    assert!(!fresh.panel().sleeping());
    assert_eq!(rx_level(&mut h, &mut fresh), 8);
    let _ = pop(&mut h, &mut fresh, 8);
    // The queued TX frame continues the pending RDDID.
    h.write(&mut fresh, CTRL2, 4, 1);
    assert_eq!(pop(&mut h, &mut fresh, 1), [0x10A]);
    send(&mut h, &mut fresh, &[0x0C5, 0, 0]);
    assert_eq!(pop(&mut h, &mut fresh, 3), [0, 0x077, 0]);

    // Garbage is refused and leaves the device unchanged.
    assert!(s.load(&[0xFF; 3]).is_err());
    assert_eq!(s.ctrl1(), CTRL1_9BIT);
    assert_eq!(s.panel().madctl(), 0x60);
}

#[test]
fn soc_attaches_spi() {
    let mut map = MemoryMap::new();
    evo_hw::soc::attach(&mut map);
    assert!(map.device::<Spi>("spi").is_some());
    let r = Access {
        kind: AccessKind::Read,
        secure: false,
        privileged: true,
    };
    let w = Access {
        kind: AccessKind::Write,
        ..r
    };
    assert_eq!(map.read32(0x4010_7064, r).ok(), Some(0x0007_0000));
    map.write32(0x4010_7004, CTRL1_9BIT, w).unwrap();
    map.write32(0x4010_7008, 0x183, w).unwrap();
    for f in [0x004, 0, 0, 0] {
        map.write32(0x4010_7018, f, w).unwrap();
    }
    assert_eq!(map.read32(0x4010_700C, r).ok(), Some(0x42));
    let got: Vec<u32> = (0..4)
        .map(|_| map.read32(0x4010_7018, r).unwrap())
        .collect();
    assert_eq!(got, [0x000, 0x085, 0x10A, 0x148]);
    assert!(map.log.is_empty());
}
