//! PL111 LCD controller at `0x4000_4000` and backlight PWM at `0x4010_4000`
//! (`docs/hardware/lcdc.md`).

use evo_hw::dev::lcdc::{FRAME_CYCLES, Frame, LCDC_IRQ, Lcdc, Pwm};
use evo_hw::{DevHarness, Device, MemoryMap, ResetKind, StateError};

const TIM0: u32 = 0x00;
const TIM1: u32 = 0x04;
const TIM2: u32 = 0x08;
const TIM3: u32 = 0x0C;
const UPBASE: u32 = 0x10;
const LPBASE: u32 = 0x14;
const CONTROL: u32 = 0x18;
const IMSC: u32 = 0x1C;
const RIS: u32 = 0x20;
const MIS: u32 = 0x24;
const ICR: u32 = 0x28;
const UPCURR: u32 = 0x2C;
const LPCURR: u32 = 0x30;

const IRQ_BIT: u64 = 1 << 14;
/// LcdEn, RGB565, TFT, BGR, VComp = front porch: the firmware's first CONTROL value.
const CTRL_ON: u32 = 0x312D;
/// LcdPwr.
const PWR: u32 = 0x800;

const SRAM_BASE: u32 = 0x2000_0000;
const SRAM_LEN: usize = 0x18_0000;
const FB: u32 = 0x2004_8000;

#[test]
fn constants() {
    assert_eq!(FRAME_CYCLES, 2_600_000);
    assert_eq!(LCDC_IRQ, 14);
}

#[test]
fn registers_store() {
    let mut h = DevHarness::new();
    let mut l = Lcdc::new();
    h.write(&mut l, TIM0, 4, 0x0A0A_0A38);
    h.write(&mut l, TIM1, 4, 0x042D_093F);
    h.write(&mut l, TIM2, 4, 0x00EF_7801);
    h.write(&mut l, TIM3, 4, 0x1234);
    h.write(&mut l, UPBASE, 4, FB);
    h.write(&mut l, LPBASE, 4, 0x2006_D800);
    h.write(&mut l, CONTROL, 4, CTRL_ON);
    h.write(&mut l, IMSC, 4, 0xFF);
    assert_eq!(h.read(&mut l, TIM0, 4), 0x0A0A_0A38);
    assert_eq!(h.read(&mut l, TIM1, 4), 0x042D_093F);
    assert_eq!(h.read(&mut l, TIM2, 4), 0x00EF_7801);
    assert_eq!(h.read(&mut l, TIM3, 4), 0x1234);
    assert_eq!(h.read(&mut l, UPBASE, 4), FB);
    assert_eq!(h.read(&mut l, LPBASE, 4), 0x2006_D800);
    assert_eq!(h.read(&mut l, CONTROL, 4), CTRL_ON);
    assert_eq!(h.read(&mut l, IMSC, 4), 0x1E);
    assert_eq!(h.read(&mut l, UPCURR, 4), FB);
    assert_eq!(h.read(&mut l, LPCURR, 4), 0x2006_D800);
    assert_eq!(h.read(&mut l, ICR, 4), 0, "ICR reads 0");
    // RIS and MIS are read-only.
    h.write(&mut l, RIS, 4, 0xFF);
    h.write(&mut l, MIS, 4, 0xFF);
    assert_eq!(h.read(&mut l, RIS, 4), 0);
    assert_eq!(h.read(&mut l, MIS, 4), 0);
    // PrimeCell IDs.
    assert_eq!(h.read(&mut l, 0xFE0, 4), 0x11);
    assert_eq!(h.read(&mut l, 0xFE8, 4), 0x24);
    assert_eq!(h.read(&mut l, 0xFFC, 4), 0xB1);
    assert!(h.log.is_empty());
    // Unmodeled offsets and non-word accesses are reported.
    assert_eq!(h.read(&mut l, 0x100, 4), 0);
    assert_eq!(h.read(&mut l, CONTROL, 2), 0);
    h.write(&mut l, CONTROL, 1, 0);
    assert_eq!(h.read(&mut l, CONTROL, 4), CTRL_ON);
    assert_eq!(h.log.len(), 3);
}

#[test]
fn vcomp_each_frame() {
    let mut h = DevHarness::new();
    let mut l = Lcdc::new();
    h.write(&mut l, CONTROL, 4, CTRL_ON);
    assert_eq!(h.event, Some(Some(2_600_000)));
    assert_eq!(h.read(&mut l, RIS, 4), 0);
    assert!(h.fire(&mut l));
    assert_eq!(h.now, 2_600_000);
    assert_eq!(h.read(&mut l, RIS, 4) & 8, 8);
    assert_eq!(h.read(&mut l, MIS, 4), 0);
    assert_eq!(h.irq, 0, "VComp masked");
    assert_eq!(h.event, Some(Some(5_200_000)), "next frame scheduled");
    h.write(&mut l, IMSC, 4, 8);
    assert_eq!(h.read(&mut l, MIS, 4), 8);
    assert_eq!(h.irq, IRQ_BIT);
    h.write(&mut l, ICR, 4, 8);
    assert_eq!(h.read(&mut l, RIS, 4), 0);
    assert_eq!(h.irq, 0);
    // The next deadline is the previous deadline plus one frame, even when the event fires late.
    h.now = 5_200_123;
    assert!(h.fire(&mut l));
    assert_eq!(h.irq, IRQ_BIT);
    assert_eq!(h.event, Some(Some(7_800_000)));
    // Rewriting CONTROL with LcdEn still set keeps the frame timing.
    h.event = None;
    h.write(&mut l, CONTROL, 4, CTRL_ON | PWR);
    assert_eq!(h.event, None);
}

#[test]
fn disable_cancels() {
    let mut h = DevHarness::new();
    let mut l = Lcdc::new();
    h.write(&mut l, CONTROL, 4, CTRL_ON);
    assert_eq!(h.event, Some(Some(2_600_000)));
    h.write(&mut l, CONTROL, 4, 0);
    assert_eq!(h.event, Some(None));
    // A stale event after disabling sets nothing.
    h.event = Some(Some(2_600_000));
    h.fire(&mut l);
    assert_eq!(h.read(&mut l, RIS, 4), 0);
    assert_eq!(h.event, None);
}

#[test]
fn system_reset_is_power_on_state() {
    let mut h = DevHarness::new();
    let mut l = Lcdc::new();
    h.write(&mut l, CONTROL, 4, CTRL_ON);
    h.write(&mut l, IMSC, 4, 8);
    h.fire(&mut l);
    assert_eq!(h.irq, IRQ_BIT);
    h.reset(&mut l, ResetKind::System);
    assert_eq!(h.event, Some(None));
    assert_eq!(h.irq, 0);
    for off in [TIM0, UPBASE, CONTROL, IMSC, RIS] {
        assert_eq!(h.read(&mut l, off, 4), 0);
    }
    let mut p = Pwm::new();
    h.write(&mut p, 0, 4, 0xA0);
    h.write(&mut p, 4, 4, 0xFF);
    h.reset(&mut p, ResetKind::System);
    assert_eq!(p.brightness(), 0.0);
    assert_eq!(h.read(&mut p, 0, 4), 0);
}

/// An LCDC set up like the firmware (240x320 portrait, RGB565, BGR) with `upbase`.
fn lcd_on(h: &mut DevHarness, upbase: u32) -> Lcdc {
    let mut l = Lcdc::new();
    h.write(&mut l, TIM0, 4, 0x0A0A_0A38);
    h.write(&mut l, TIM1, 4, 0x042D_093F);
    h.write(&mut l, UPBASE, 4, upbase);
    h.write(&mut l, CONTROL, 4, CTRL_ON | PWR);
    l
}

fn put(sram: &mut [u8], line: usize, px: usize, v: u16) {
    let o = (FB - SRAM_BASE) as usize + (line * 240 + px) * 2;
    sram[o..o + 2].copy_from_slice(&v.to_le_bytes());
}

fn rgb(f: &Frame, x: u32, y: u32) -> (u8, u8, u8) {
    let o = ((y * f.width + x) * 3) as usize;
    (f.rgb[o], f.rgb[o + 1], f.rgb[o + 2])
}

#[test]
fn frame_decode() {
    let mut sram = vec![0u8; SRAM_LEN];
    put(&mut sram, 0, 0, 0xF800);
    put(&mut sram, 319, 239, 0x001F);
    put(&mut sram, 5, 7, 0x07E0);
    put(&mut sram, 6, 8, 0x8410);
    let mut h = DevHarness::new();
    let l = lcd_on(&mut h, FB);
    let f = l.frame(&sram, SRAM_BASE).expect("frame");
    assert_eq!((f.width, f.height), (320, 240));
    assert_eq!(f.rgb.len(), 320 * 240 * 3);
    assert_eq!(rgb(&f, 0, 0), (0, 0, 255), "BGR: the r5 field is blue");
    assert_eq!(rgb(&f, 319, 239), (255, 0, 0));
    assert_eq!(rgb(&f, 5, 7), (0, 255, 0));
    // r5 = 16, g6 = 32, b5 = 16: (16 << 3) | (16 >> 2) = 132, (32 << 2) | (32 >> 4) = 130.
    assert_eq!(rgb(&f, 6, 8), (132, 130, 132));
    assert_eq!(rgb(&f, 1, 0), (0, 0, 0));
    // The Secure alias of SRAM shows the same frame.
    let mut h = DevHarness::new();
    let ls = lcd_on(&mut h, FB | 0x1000_0000);
    assert_eq!(ls.frame(&sram, SRAM_BASE), Some(f.clone()));
    // Without BGR the fields are in RGB order.
    let mut l = lcd_on(&mut h, FB);
    h.write(&mut l, CONTROL, 4, (CTRL_ON | PWR) & !0x100);
    let g = l.frame(&sram, SRAM_BASE).expect("frame");
    assert_eq!(rgb(&g, 0, 0), (255, 0, 0));
    assert_eq!(rgb(&g, 319, 239), (0, 0, 255));
}

#[test]
fn frame_none_when_off() {
    let sram = vec![0u8; SRAM_LEN];
    let mut h = DevHarness::new();
    let l = Lcdc::new();
    assert_eq!(l.frame(&sram, SRAM_BASE), None, "power-on state");
    let mut l = lcd_on(&mut h, FB);
    assert!(l.frame(&sram, SRAM_BASE).is_some());
    h.write(&mut l, CONTROL, 4, CTRL_ON);
    assert_eq!(l.frame(&sram, SRAM_BASE), None, "LcdPwr clear");
    h.write(&mut l, CONTROL, 4, (CTRL_ON | PWR) & !1);
    assert_eq!(l.frame(&sram, SRAM_BASE), None, "LcdEn clear");
}

#[test]
fn frame_outside_sram_is_none() {
    let sram = vec![0u8; SRAM_LEN];
    let mut h = DevHarness::new();
    for base in [
        0xFFFF_FF00,
        0x2017_FF00,
        0x1FFF_FF00,
        0x0000_0000,
        0x4000_4000,
    ] {
        let l = lcd_on(&mut h, base);
        assert_eq!(l.frame(&sram, SRAM_BASE), None, "UPBASE {base:#010x}");
    }
    // The last base that still fits.
    let last = SRAM_BASE + SRAM_LEN as u32 - 240 * 320 * 2;
    assert!(lcd_on(&mut h, last).frame(&sram, SRAM_BASE).is_some());
    assert_eq!(lcd_on(&mut h, last + 2).frame(&sram, SRAM_BASE), None);
    // An SRAM slice shorter than the frame.
    assert_eq!(lcd_on(&mut h, FB).frame(&sram[..0x1000], SRAM_BASE), None);
    // Another pixel format: bpp 5 (24 bpp).
    let mut l = lcd_on(&mut h, FB);
    h.write(&mut l, CONTROL, 4, ((CTRL_ON | PWR) & !0xE) | (5 << 1));
    assert_eq!(l.frame(&sram, SRAM_BASE), None);
    // The largest geometry with a base near the top of the address space does not overflow.
    let mut l = lcd_on(&mut h, 0xFFFF_FFFE);
    h.write(&mut l, TIM0, 4, 0xFC);
    h.write(&mut l, TIM1, 4, 0x3FF);
    assert_eq!(l.frame(&sram, SRAM_BASE), None);
    assert_eq!(l.frame(&sram, 0xFFFF_0000), None);
}

#[test]
fn pwm_brightness() {
    let mut h = DevHarness::new();
    let mut p = Pwm::new();
    assert_eq!(p.brightness(), 0.0);
    h.write(&mut p, 0, 4, 0xA0);
    h.write(&mut p, 4, 4, 0xFF);
    assert_eq!(h.read(&mut p, 0, 4), 0xA0);
    assert_eq!(h.read(&mut p, 4, 4), 0xFF);
    assert!((p.brightness() - 160.0 / 255.0).abs() < 1e-6);
    h.write(&mut p, 4, 4, 0);
    assert_eq!(p.brightness(), 0.0);
    h.write(&mut p, 0, 4, 0x200);
    h.write(&mut p, 4, 4, 0xFF);
    assert_eq!(p.brightness(), 1.0);
    h.write(&mut p, 0, 4, 0x000F_FFFF);
    h.write(&mut p, 4, 4, 0x000F_FFFF);
    assert_eq!(p.brightness(), 1.0);
    assert!(h.log.is_empty());
    assert_eq!(h.read(&mut p, 8, 4), 0);
    h.write(&mut p, 0, 2, 1);
    assert_eq!(h.log.len(), 2);
}

#[test]
fn state_roundtrip() {
    let mut h = DevHarness::new();
    let mut l = lcd_on(&mut h, FB);
    h.write(&mut l, IMSC, 4, 8);
    h.fire(&mut l);
    let st = l.save();
    let mut l2 = Lcdc::new();
    l2.load(&st).unwrap();
    for off in [TIM0, TIM1, UPBASE, CONTROL, IMSC, RIS, MIS] {
        assert_eq!(h.read(&mut l2, off, 4), h.read(&mut l, off, 4), "{off:#x}");
    }
    // The frame deadline survives: the event due at 5_200_000 schedules 7_800_000.
    assert_eq!(h.event, Some(Some(5_200_000)));
    h.write(&mut l2, ICR, 4, 8);
    h.fire(&mut l2);
    assert_eq!(h.read(&mut l2, RIS, 4), 8);
    assert_eq!(h.event, Some(Some(7_800_000)));
    assert!(l2.load(&[0xFF; 3]).is_err());
    assert!(matches!(
        l2.load(&[st.clone(), vec![0]].concat()),
        Err(StateError::Mismatch(_))
    ));
    assert_eq!(h.read(&mut l2, CONTROL, 4), CTRL_ON | PWR, "failed load");

    let mut p = Pwm::new();
    h.write(&mut p, 0, 4, 0xA0);
    h.write(&mut p, 4, 4, 0xFF);
    let st = p.save();
    let mut p2 = Pwm::new();
    p2.load(&st).unwrap();
    assert_eq!(p2.brightness(), p.brightness());
    assert!(p2.load(&[st.clone(), vec![0]].concat()).is_err());
    assert_eq!(p2.brightness(), p.brightness());
}

#[test]
fn soc_attaches_lcdc_and_pwm() {
    let mut map = MemoryMap::new();
    evo_hw::soc::attach(&mut map);
    assert!(map.device::<Lcdc>("lcdc").is_some());
    assert!(map.device::<Pwm>("pwm").is_some());
}
