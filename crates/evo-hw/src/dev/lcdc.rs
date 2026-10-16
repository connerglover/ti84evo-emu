//! Arm PL111 CLCD at `0x4000_4000` (IRQ 14) and the backlight PWM at `0x4010_4000`
//! (`docs/hardware/lcdc.md`).
//!
//! LCDC registers: LCDTiming0-3 +0x00..+0x0C, UPBASE +0x10, LPBASE +0x14, CONTROL +0x18,
//! IMSC +0x1C (stores `v & 0x1E`), RIS +0x20, MIS +0x24 (`RIS & IMSC`), ICR +0x28 (W1C on RIS,
//! reads 0), +0x2C/+0x30 the current UPBASE/LPBASE, +0xFE0..+0xFFC the PrimeCell IDs. RIS,
//! MIS, the current-base and ID registers are read-only (writes are ignored). Only aligned
//! 32-bit accesses reach registers; any other width or alignment, and every other offset,
//! reads 0, ignores writes and is reported as unhandled.
//!
//! While CONTROL bit 0 (LcdEn) is set, a scheduler event every [`FRAME_CYCLES`] latches RIS
//! bit 3 (VComp). The first deadline is one frame after LcdEn goes from 0 to 1; each next one
//! is the previous deadline plus one frame (an event that fires late does not drift the frame
//! rate). Clearing LcdEn cancels the event. IRQ 14 (level) = `RIS & IMSC != 0`.
//!
//! The pixels are not part of the device: [`Lcdc::frame`] decodes the framebuffer from the
//! SRAM bytes the host passes in, so a frame costs nothing until someone looks at it.
//!
//! A System reset returns the LCDC and the PWM to their power-on state (event cancelled).

use serde::{Deserialize, Serialize};

use crate::{CPU_HZ, DevCtx, Device, RegView, ResetKind, StateError, load_serde, save_serde};

/// Cycles per LCD frame: 60 Hz at [`CPU_HZ`] (the real pixel clock is unknown; `lcdc.md`).
pub const FRAME_CYCLES: u64 = CPU_HZ / 60;
/// The LCDC's IRQ line.
pub const LCDC_IRQ: u16 = 14;

const WINDOW: u32 = 0x1000;

const TIM0: u32 = 0x00;
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
/// PrimeCell peripheral and cell IDs at +0xFE0, +0xFE4, .. +0xFFC.
const IDS_AT: u32 = 0xFE0;
const IDS: [u32; 8] = [0x11, 0x11, 0x24, 0x00, 0x0D, 0xF0, 0x05, 0xB1];

/// The interrupt bits PL111 implements (FUF, LNBU, VComp, MBERROR).
const INT_MASK: u32 = 0x1E;
/// RIS bit 3: vertical compare.
const VCOMP: u32 = 1 << 3;
/// CONTROL bits.
const LCD_EN: u32 = 1 << 0;
const LCD_BGR: u32 = 1 << 8;
const LCD_PWR: u32 = 1 << 11;
/// CONTROL LcdBpp (bits 3:1) for 16 bpp RGB565.
const BPP_565: u32 = 6;
/// Bit 28 selects the Secure alias of SRAM (0x3xxx_xxxx over the same bytes as 0x2xxx_xxxx).
const SECURE_ALIAS: u32 = 0x1000_0000;

/// One decoded screen image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// RGB888, row-major, `width * height * 3` bytes.
    pub rgb: Vec<u8>,
}

/// The PL111 LCD controller.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Lcdc {
    tim: [u32; 4],
    upbase: u32,
    lpbase: u32,
    control: u32,
    imsc: u32,
    ris: u32,
    /// The pending frame deadline while LcdEn is set.
    next: u64,
}

impl Lcdc {
    /// The controller in its power-on state: every register 0, LCD off, no frame event.
    pub fn new() -> Self {
        Self::default()
    }

    /// The image the panel shows, decoded from `sram` (the SRAM bytes, which start at
    /// address `sram_base`). `None` unless LcdEn and LcdPwr are set, the format is RGB565 and
    /// the whole framebuffer at UPBASE (Secure alias folded onto the Non-secure one) lies
    /// inside `sram`. The portrait buffer (PPL pixels per line, LPP lines) is shown landscape:
    /// `width = LPP`, `height = PPL`, output pixel (x, y) = buffer pixel (line x, pixel y).
    /// Colour order is PL111's: red is bits 4:0 and blue bits 15:11 of a pixel, swapped when
    /// CONTROL bit 8 (BGR) is set, as the firmware sets it (`lcdc.md` "Colour order").
    pub fn frame(&self, sram: &[u8], sram_base: u32) -> Option<Frame> {
        let c = self.control;
        if c & LCD_EN == 0 || c & LCD_PWR == 0 || (c >> 1) & 7 != BPP_565 {
            return None;
        }
        let ppl = (((self.tim[0] >> 2) & 0x3F) + 1) * 16;
        let lpp = (self.tim[1] & 0x3FF) + 1;
        let len = u64::from(ppl) * u64::from(lpp) * 2;
        let base = self.upbase & !SECURE_ALIAS;
        let start = u64::from(base.checked_sub(sram_base)?);
        if start + len > sram.len() as u64 {
            return None;
        }
        // In range: `start + len <= sram.len()`, so both fit in usize.
        let fb = &sram[start as usize..(start + len) as usize];
        let bgr = c & LCD_BGR != 0;
        let (w, h) = (lpp as usize, ppl as usize);
        let mut rgb = vec![0u8; w * h * 3];
        for (line, row) in fb.chunks_exact(h * 2).enumerate() {
            for (px, p) in row.as_chunks::<2>().0.iter().enumerate() {
                let p = u16::from_le_bytes(*p);
                let (hi5, g6, lo5) = ((p >> 11) as u8, ((p >> 5) & 63) as u8, (p & 31) as u8);
                // PL111 takes red from bits 4:0 unless BGR swaps it to bits 15:11.
                let (r5, b5) = if bgr { (hi5, lo5) } else { (lo5, hi5) };
                let o = (px * w + line) * 3;
                rgb[o] = (r5 << 3) | (r5 >> 2);
                rgb[o + 1] = (g6 << 2) | (g6 >> 4);
                rgb[o + 2] = (b5 << 3) | (b5 >> 2);
            }
        }
        Some(Frame {
            width: lpp,
            height: ppl,
            rgb,
        })
    }

    fn reg(&self, off: u32) -> Option<u32> {
        Some(match off {
            TIM0..=TIM3 => self.tim[(off / 4) as usize],
            UPBASE | UPCURR => self.upbase,
            LPBASE | LPCURR => self.lpbase,
            CONTROL => self.control,
            IMSC => self.imsc,
            RIS => self.ris,
            MIS => self.ris & self.imsc,
            ICR => 0,
            IDS_AT.. if off < WINDOW => IDS[((off - IDS_AT) / 4) as usize],
            _ => return None,
        })
    }

    fn drive_irq(&self, ctx: &mut DevCtx<'_>) {
        ctx.set_irq(LCDC_IRQ, self.ris & self.imsc != 0);
    }
}

/// An aligned word access inside the window.
fn word_access(off: u32, width: u8) -> bool {
    width == 4 && off.is_multiple_of(4) && off < WINDOW
}

impl Device for Lcdc {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        match self.reg(off).filter(|_| word_access(off, width)) {
            Some(v) => v,
            None => {
                ctx.unhandled(off, width, None);
                0
            }
        }
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        if !word_access(off, width) || self.reg(off).is_none() {
            ctx.unhandled(off, width, Some(v));
            return;
        }
        match off {
            TIM0..=TIM3 => self.tim[(off / 4) as usize] = v,
            UPBASE => self.upbase = v,
            LPBASE => self.lpbase = v,
            CONTROL => {
                let was_on = self.control & LCD_EN != 0;
                self.control = v;
                match (was_on, v & LCD_EN != 0) {
                    (false, true) => {
                        self.next = ctx.now.saturating_add(FRAME_CYCLES);
                        ctx.schedule_at(self.next);
                    }
                    (true, false) => ctx.cancel_event(),
                    _ => {}
                }
            }
            IMSC => self.imsc = v & INT_MASK,
            ICR => self.ris &= !v,
            // RIS, MIS, the current bases and the IDs are read-only.
            _ => {}
        }
        self.drive_irq(ctx);
    }

    fn event(&mut self, ctx: &mut DevCtx<'_>) {
        if self.control & LCD_EN == 0 {
            return;
        }
        self.ris |= VCOMP;
        // The next deadline after `now`, on the frame grid of the previous one.
        let late = ctx.now.saturating_sub(self.next) / FRAME_CYCLES;
        self.next = self
            .next
            .saturating_add((late + 1).saturating_mul(FRAME_CYCLES));
        ctx.schedule_at(self.next);
        self.drive_irq(ctx);
    }

    fn reset(&mut self, _kind: ResetKind, ctx: &mut DevCtx<'_>) {
        *self = Lcdc::new();
        ctx.cancel_event();
        self.drive_irq(ctx);
    }

    fn save(&self) -> Vec<u8> {
        save_serde(self)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut s = self.clone();
        load_serde(&mut s, state)?;
        s.imsc &= INT_MASK;
        s.ris &= INT_MASK;
        *self = s;
        Ok(())
    }

    fn inspect(&self, _now: u64) -> Vec<RegView> {
        let mut named: Vec<(String, u32)> = vec![
            ("tim0".into(), TIM0),
            ("tim1".into(), TIM0 + 4),
            ("tim2".into(), TIM0 + 8),
            ("tim3".into(), TIM3),
            ("upbase".into(), UPBASE),
            ("lpbase".into(), LPBASE),
            ("control".into(), CONTROL),
            ("imsc".into(), IMSC),
            ("ris".into(), RIS),
            ("mis".into(), MIS),
            ("icr".into(), ICR),
            ("upcurr".into(), UPCURR),
            ("lpcurr".into(), LPCURR),
        ];
        named.extend((0..IDS.len() as u32).map(|i| (format!("id{i}"), IDS_AT + 4 * i)));
        named
            .into_iter()
            .map(|(name, off)| {
                // Every offset listed is a register.
                let row = RegView::new(name, off, self.reg(off).unwrap_or(0));
                if off == CONTROL {
                    row.with_note(control_note(self.control))
                } else {
                    row
                }
            })
            .collect()
    }
}

/// CONTROL decoded: `"enabled, power on, rgb565"`, `"disabled, power off, bpp 2"`, ...
fn control_note(c: u32) -> String {
    let mut parts = vec![
        if c & LCD_EN != 0 {
            "enabled"
        } else {
            "disabled"
        }
        .to_owned(),
        if c & LCD_PWR != 0 {
            "power on"
        } else {
            "power off"
        }
        .to_owned(),
    ];
    let bpp = (c >> 1) & 7;
    parts.push(if bpp == BPP_565 {
        "rgb565".to_owned()
    } else {
        format!("bpp {bpp}")
    });
    if c & LCD_BGR != 0 {
        parts.push("bgr".to_owned());
    }
    parts.join(", ")
}

/// The backlight PWM: duty at +0x00, period at +0x04, plain storage (no IRQ).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Pwm {
    duty: u32,
    period: u32,
}

impl Pwm {
    /// The PWM in its power-on state (both registers 0: backlight off).
    pub fn new() -> Self {
        Self::default()
    }

    /// Backlight brightness: duty / period, clamped to `0.0..=1.0`; 0 if the period is 0.
    pub fn brightness(&self) -> f32 {
        if self.period == 0 {
            return 0.0;
        }
        (f64::from(self.duty) / f64::from(self.period)).min(1.0) as f32
    }
}

impl Device for Pwm {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        match (off, width) {
            (0, 4) => self.duty,
            (4, 4) => self.period,
            _ => {
                ctx.unhandled(off, width, None);
                0
            }
        }
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        match (off, width) {
            (0, 4) => self.duty = v,
            (4, 4) => self.period = v,
            _ => ctx.unhandled(off, width, Some(v)),
        }
    }

    fn reset(&mut self, _kind: ResetKind, _ctx: &mut DevCtx<'_>) {
        *self = Pwm::new();
    }

    fn save(&self) -> Vec<u8> {
        save_serde(self)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        load_serde(self, state)
    }

    fn inspect(&self, _now: u64) -> Vec<RegView> {
        vec![
            RegView::new("duty", 0x00, self.duty),
            RegView::new("period", 0x04, self.period),
        ]
    }
}
