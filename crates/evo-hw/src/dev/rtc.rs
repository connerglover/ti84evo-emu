//! RTC at `0x4010_2000` (`docs/hardware/rtc.md` "Model (first cut)"): time of day, one
//! level-sensitive IRQ line (15).
//!
//! Only aligned 32-bit accesses reach registers; any other width or alignment, and the
//! offsets the model leaves out, read 0, ignore writes and are reported as unhandled.
//!
//! - +0x10 / +0x14 / +0x18 (alarm fields) and +0x20 (control): storage.
//! - +0x34 status: bits 0-5, write 1 to clear; only [`Rtc::raise_status`] sets them.
//! - +0x40 busy: reads [`RTC_BUSY`] (read-only).
//! - +0x44 time: `sec | min << 6 | hour << 12` of the seconds of day
//!   `(base + (now - set_at) / CPU_HZ) % 86_400`. The count starts at 00:00:00 at power-on
//!   (no host clock: the machine is deterministic). A write decodes the same fields into
//!   `base` and restarts the count from now.
//! - IRQ 15 = `status & 0x3F != 0`.
//!
//! The RTC is an always-on block: a System reset keeps every register and the running time
//! (and re-drives IRQ 15 from the kept status). PowerOn returns to 00:00:00 counted from the
//! reset time.

use serde::{Deserialize, Serialize};

use crate::{CPU_HZ, DevCtx, Device, ResetKind, StateError, load_serde, save_serde};

/// The RTC's IRQ line.
pub const RTC_IRQ: u16 = 15;
/// The value +0x40 reads: no busy bit set. The BSP polls bit 3 (0x1542B0) and bit 8
/// (0x1543E4) and both wait loops exit at once with 0 (`rtc.md` "Busy polarity").
pub const RTC_BUSY: u32 = 0;

/// Implemented status bits.
const STATUS_MASK: u32 = 0x3F;
const DAY: u64 = 86_400;
const WINDOW: u32 = 0x1000;

const ALARM0: u32 = 0x10;
const ALARM1: u32 = 0x14;
const ALARM2: u32 = 0x18;
const CTRL: u32 = 0x20;
const STATUS: u32 = 0x34;
const BUSY: u32 = 0x40;
const TIME: u32 = 0x44;

/// The RTC.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Rtc {
    alarm: [u32; 3],
    ctrl: u32,
    status: u32,
    /// Seconds of day at `set_at`.
    base: u32,
    /// Cycle the time was last set (or the power-on reset).
    set_at: u64,
}

impl Rtc {
    /// The RTC at power-on: 00:00:00 counted from cycle 0, every register 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// The seconds of day (0..86_400) at cycle `now`. A `now` before the last set reads as
    /// the set time.
    pub fn seconds_of_day(&self, now: u64) -> u32 {
        let elapsed = now.saturating_sub(self.set_at) / CPU_HZ;
        // `% DAY` first: `base + elapsed` cannot overflow and the result fits u32.
        ((u64::from(self.base) + elapsed % DAY) % DAY) as u32
    }

    /// Set status bits (host/test): `status |= bits & 0x3F`, then drive IRQ 15.
    pub fn raise_status(&mut self, bits: u32, ctx: &mut DevCtx<'_>) {
        self.status |= bits & STATUS_MASK;
        self.drive_irq(ctx);
    }

    fn drive_irq(&self, ctx: &mut DevCtx<'_>) {
        ctx.set_irq(RTC_IRQ, self.status & STATUS_MASK != 0);
    }

    fn time_reg(&self, now: u64) -> u32 {
        let s = self.seconds_of_day(now);
        (s % 60) | ((s / 60) % 60) << 6 | (s / 3600) << 12
    }

    fn reg(&self, off: u32, now: u64) -> Option<u32> {
        Some(match off {
            ALARM0 => self.alarm[0],
            ALARM1 => self.alarm[1],
            ALARM2 => self.alarm[2],
            CTRL => self.ctrl,
            STATUS => self.status,
            BUSY => RTC_BUSY,
            TIME => self.time_reg(now),
            _ => return None,
        })
    }
}

/// An aligned word access inside the window.
fn word_access(off: u32, width: u8) -> bool {
    width == 4 && off.is_multiple_of(4) && off < WINDOW
}

impl Device for Rtc {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        match self.reg(off, ctx.now).filter(|_| word_access(off, width)) {
            Some(v) => v,
            None => {
                ctx.unhandled(off, width, None);
                0
            }
        }
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        if !word_access(off, width) {
            ctx.unhandled(off, width, Some(v));
            return;
        }
        match off {
            ALARM0 => self.alarm[0] = v,
            ALARM1 => self.alarm[1] = v,
            ALARM2 => self.alarm[2] = v,
            CTRL => self.ctrl = v,
            STATUS => {
                self.status &= !v;
                self.drive_irq(ctx);
            }
            TIME => {
                let (sec, min, hour) = (v & 0x3F, (v >> 6) & 0x3F, (v >> 12) & 0x1F);
                self.base = ((hour * 3600 + min * 60 + sec) as u64 % DAY) as u32;
                self.set_at = ctx.now;
            }
            _ => ctx.unhandled(off, width, Some(v)),
        }
    }

    fn reset(&mut self, kind: ResetKind, ctx: &mut DevCtx<'_>) {
        if kind == ResetKind::PowerOn {
            *self = Rtc {
                set_at: ctx.now,
                ..Rtc::new()
            };
        }
        self.drive_irq(ctx);
    }

    fn save(&self) -> Vec<u8> {
        save_serde(self)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut s = self.clone();
        load_serde(&mut s, state)?;
        s.status &= STATUS_MASK;
        s.base = (u64::from(s.base) % DAY) as u32;
        *self = s;
        Ok(())
    }
}
