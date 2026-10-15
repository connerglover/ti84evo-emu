//! GPIO controller at `0x4010_5000` (`docs/hardware/gpio.md` "Minimal model"): 13 pins, one
//! level-sensitive IRQ line (24) for every pin.
//!
//! Only aligned 32-bit accesses reach registers; any other width or alignment, and the
//! offsets the model leaves out, read 0, ignore writes and are reported as unhandled.
//!
//! A pin's level is its output bit when it is an output (DIR = 1); for an input, the host
//! level if the host drives it ([`Gpio::set_input`]), else its pull (PULL_SEL bit when
//! PULL_EN is set), else 0. After every register write and every host input change, each
//! pin's interrupt latch is updated from its old and new level: level mode (INT_TYPE = 1)
//! latches while level == INT_POL; edge mode latches on a rising edge if INT_POL or INT_BOTH,
//! on a falling edge if !INT_POL or INT_BOTH. IRQ 24 = `latch & INT_EN != 0`.
//!
//! Pin 1 is the power hold: when it is an output going from high to low, the device logs
//! `power-hold released` and [`Gpio::take_power_off`] reports it once.
//!
//! A System reset returns every register (and the latch) to 0 but keeps the host input
//! levels (what is wired to the pins does not change); PowerOn clears those too. A pending
//! power-off report is kept across a System reset.

use serde::{Deserialize, Serialize};

use crate::{DevCtx, Device, ResetKind, StateError, load_serde, save_serde};

/// Number of pins.
pub const GPIO_PINS: u8 = 13;
/// The controller's IRQ line.
pub const GPIO_IRQ: u16 = 24;

/// Bits of the implemented pins.
const PIN_MASK: u32 = (1 << GPIO_PINS) - 1;
/// The power-hold pin (BSP Shipmode drives it low).
const POWER_PIN: u32 = 1 << 1;
const WINDOW: u32 = 0x1000;

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

/// The GPIO controller. Every register bit field holds bit `p` for pin `p`, 13 bits used.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Gpio {
    out: u32,
    dir: u32,
    pull_en: u32,
    pull_sel: u32,
    ien: u32,
    /// The interrupt latch (INT_STATUS reads `ists & ien`).
    ists: u32,
    itype: u32,
    iboth: u32,
    ipol: u32,
    /// Pins the host drives, and their levels.
    ext_driven: u32,
    ext_level: u32,
    /// Pin 1 was released (output high → low) and the host has not been told yet.
    power_off: bool,
}

impl Gpio {
    /// The controller in its power-on state: every register 0, no host inputs.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drive input `pin` from the host: `Some(level)`, or `None` = undriven (the pull, else
    /// 0). Pins >= [`GPIO_PINS`] are ignored. An output pin keeps its output level, but the
    /// host level applies once it becomes an input.
    pub fn set_input(&mut self, pin: u8, level: Option<bool>, ctx: &mut DevCtx<'_>) {
        if pin >= GPIO_PINS {
            return;
        }
        let old = self.levels();
        let b = 1 << pin;
        match level {
            Some(l) => {
                self.ext_driven |= b;
                self.ext_level = (self.ext_level & !b) | if l { b } else { 0 };
            }
            None => {
                self.ext_driven &= !b;
                self.ext_level &= !b;
            }
        }
        self.update(old, ctx);
    }

    /// The level of `pin` now (false for pins >= [`GPIO_PINS`]).
    pub fn pin_level(&self, pin: u8) -> bool {
        pin < GPIO_PINS && self.levels() & (1 << pin) != 0
    }

    /// Pin 1 is an output driven high: the unit holds its own power on.
    pub fn power_hold(&self) -> bool {
        self.dir & self.out & POWER_PIN != 0
    }

    /// True once after pin 1, an output, went from high to low; the report is drained.
    pub fn take_power_off(&mut self) -> bool {
        std::mem::take(&mut self.power_off)
    }

    /// Every pin's level, bit `p` for pin `p`.
    fn levels(&self) -> u32 {
        let pull = self.pull_en & self.pull_sel & !self.ext_driven;
        let input = (self.ext_level & self.ext_driven) | pull;
        ((self.out & self.dir) | (input & !self.dir)) & PIN_MASK
    }

    /// Latch interrupts for the change from the `old` levels to now, then drive IRQ 24.
    fn update(&mut self, old: u32, ctx: &mut DevCtx<'_>) {
        let new = self.levels();
        let rising = !old & new;
        let falling = old & !new;
        let level = self.itype & !(new ^ self.ipol);
        let edge = !self.itype
            & ((rising & (self.ipol | self.iboth)) | (falling & (!self.ipol | self.iboth)));
        self.ists |= (level | edge) & PIN_MASK;
        ctx.set_irq(GPIO_IRQ, self.ists & self.ien != 0);
    }

    fn reg(&self, off: u32) -> Option<u32> {
        Some(match off {
            DATA_OUT => self.out,
            DATA_IN => self.levels(),
            DIR => self.dir,
            PULL_EN => self.pull_en,
            PULL_SEL => self.pull_sel,
            INT_EN => self.ien,
            INT_STATUS => self.ists & self.ien,
            INT_TYPE => self.itype,
            INT_BOTH => self.iboth,
            INT_POL => self.ipol,
            _ => return None,
        })
    }
}

/// An aligned word access inside the window.
fn word_access(off: u32, width: u8) -> bool {
    width == 4 && off.is_multiple_of(4) && off < WINDOW
}

impl Device for Gpio {
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
        if !word_access(off, width) {
            ctx.unhandled(off, width, Some(v));
            return;
        }
        let old = self.levels();
        let old_out = self.dir & self.out;
        let b = v & PIN_MASK;
        match off {
            DATA_OUT => self.out = b,
            DIR => self.dir = b,
            SET => self.out |= b,
            CLR => self.out &= !b,
            PULL_EN => self.pull_en = b,
            PULL_SEL => self.pull_sel = b,
            INT_EN => self.ien = b,
            INT_CLR => self.ists &= !b,
            INT_TYPE => self.itype = b,
            INT_BOTH => self.iboth = b,
            INT_POL => self.ipol = b,
            _ => {
                ctx.unhandled(off, width, Some(v));
                return;
            }
        }
        // Pin 1 an output before and after, high before, low now.
        if old_out & POWER_PIN != 0 && self.dir & POWER_PIN != 0 && self.out & POWER_PIN == 0 {
            log::info!("power-hold released");
            self.power_off = true;
        }
        self.update(old, ctx);
    }

    fn reset(&mut self, kind: ResetKind, ctx: &mut DevCtx<'_>) {
        let (ext_driven, ext_level, power_off) = match kind {
            ResetKind::System => (self.ext_driven, self.ext_level, self.power_off),
            ResetKind::PowerOn => (0, 0, false),
        };
        *self = Gpio {
            ext_driven,
            ext_level,
            power_off,
            ..Gpio::new()
        };
        ctx.set_irq(GPIO_IRQ, false);
    }

    fn save(&self) -> Vec<u8> {
        save_serde(self)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut s = self.clone();
        load_serde(&mut s, state)?;
        for r in [
            &mut s.out,
            &mut s.dir,
            &mut s.pull_en,
            &mut s.pull_sel,
            &mut s.ien,
            &mut s.ists,
            &mut s.itype,
            &mut s.iboth,
            &mut s.ipol,
            &mut s.ext_driven,
        ] {
            *r &= PIN_MASK;
        }
        s.ext_level &= s.ext_driven;
        *self = s;
        Ok(())
    }
}
