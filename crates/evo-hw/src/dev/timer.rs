//! TIMER block at `0x4010_1000` (`docs/hardware/timer.md`): three 32-bit down-counters whose
//! current value is computed from the cycle count, so the block needs no scheduler events.
//!
//! Per channel n: VALUE at +0x10n, LOAD at +0x10n+4. Common: CTRL +0x30 (bit 3n enable, bit
//! 3n+1 selects the 32,768 Hz clock, else 39,000 Hz), INT_STAT +0x34 (W1C), INT_EN +0x38.
//! Only aligned 32-bit accesses reach registers; anything else reads 0, ignores writes and is
//! logged as unhandled. Interrupts are not modeled: no status bit is ever set and IRQs 1-3 are
//! never asserted (no firmware path enables them).
//!
//! The block keeps its state across a System reset (`SYSRESETREQ`): the firmware
//! re-initializes it on every boot. A PowerOn reset clears every register.

use serde::{Deserialize, Serialize};

use crate::{CPU_HZ, DevCtx, Device, RegView, ResetKind, StateError, load_serde, save_serde};

/// Number of counter channels.
const CHANNELS: usize = 3;
/// Clock rate with CTRL bit 3n+1 set.
const HZ_32K: u64 = 32_768;
/// Clock rate with CTRL bit 3n+1 clear (the firmware's /39000 conversion).
const HZ_39K: u64 = 39_000;

const CTRL: u32 = 0x30;
const INT_STAT: u32 = 0x34;
const INT_EN: u32 = 0x38;

/// One counter: the value it held at cycle `since`, and its reload value.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
struct Channel {
    value: u32,
    load: u32,
    since: u64,
}

/// Everything a save state carries.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Regs {
    ch: [Channel; CHANNELS],
    ctrl: u32,
    ists: u32,
    ien: u32,
}

/// The 3-channel timer. VALUE reads are computed from `ctx.now`; they never change state.
#[derive(Clone, Debug, Default)]
pub struct Timer {
    regs: Regs,
    /// The "timer interrupts not modeled" warning was logged (host-side only, not saved).
    warned: bool,
}

impl Timer {
    /// The timer in its power-on state (every register 0, every channel disabled).
    pub fn new() -> Self {
        Self::default()
    }

    fn enabled(&self, n: usize) -> bool {
        self.regs.ctrl >> (3 * n) & 1 != 0
    }

    fn hz(&self, n: usize) -> u64 {
        if self.regs.ctrl >> (3 * n + 1) & 1 != 0 {
            HZ_32K
        } else {
            HZ_39K
        }
    }

    /// Whole ticks of channel `n` between `since` and `now` (0 if `now` is before `since`).
    ///
    /// The tick clocks are slower than the CPU, so the result is at most `now - since`. In
    /// 64 bits while `elapsed * hz` fits (2^48 cycles, weeks of guest time), which spares
    /// every counter read a 128-bit division; exact in 128 bits beyond.
    fn ticks(&self, n: usize, now: u64) -> u64 {
        let elapsed = now.saturating_sub(self.regs.ch[n].since);
        let hz = self.hz(n);
        match elapsed.checked_mul(hz) {
            Some(p) => p / CPU_HZ,
            None => {
                let t = u128::from(elapsed) * u128::from(hz) / u128::from(CPU_HZ);
                u64::try_from(t).unwrap_or(u64::MAX)
            }
        }
    }

    /// The count channel `n` shows at `now`.
    fn cur(&self, n: usize, now: u64) -> u32 {
        let c = &self.regs.ch[n];
        if !self.enabled(n) {
            return c.value;
        }
        let ticks = self.ticks(n, now);
        let value = u64::from(c.value);
        if ticks <= value {
            // `ticks <= value <= u32::MAX`.
            (value - ticks) as u32
        } else {
            // Underflow reloads from LOAD: the period after the first underflow is LOAD + 1.
            let period = u64::from(c.load) + 1;
            let into = ((ticks - value - 1) % period) as u32;
            c.load - into
        }
    }

    /// Fold the elapsed whole ticks of channel `n` into `value`, carrying the part-tick
    /// remainder in `since`. A disabled channel restarts its time base at `now`.
    fn latch(&mut self, n: usize, now: u64) {
        if !self.enabled(n) {
            self.regs.ch[n].since = now;
            return;
        }
        let ticks = self.ticks(n, now);
        let value = self.cur(n, now);
        let hz = self.hz(n);
        // The cycles those whole ticks took (`ticks * CPU_HZ <= elapsed * hz`, so 64 bits
        // suffice whenever `ticks` was computed in 64 bits).
        let used = match ticks.checked_mul(CPU_HZ) {
            Some(p) => p.div_ceil(hz),
            None => {
                let u = (u128::from(ticks) * u128::from(CPU_HZ)).div_ceil(u128::from(hz));
                u64::try_from(u).unwrap_or(u64::MAX)
            }
        };
        let c = &mut self.regs.ch[n];
        // `used <= now - since` because `ticks` was rounded down; clamp anyway.
        let room = now.saturating_sub(c.since);
        c.since += used.min(room);
        c.value = value;
    }

    /// The value a guest word read of `off` returns at `now`, or `None` if not a register.
    fn reg(&self, off: u32, now: u64) -> Option<u32> {
        match off {
            0x00 | 0x10 | 0x20 => Some(self.cur((off >> 4) as usize, now)),
            0x04 | 0x14 | 0x24 => Some(self.regs.ch[(off >> 4) as usize].load),
            CTRL => Some(self.regs.ctrl),
            INT_STAT => Some(self.regs.ists),
            INT_EN => Some(self.regs.ien),
            _ => None,
        }
    }
}

impl Device for Timer {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        match self.reg(off, ctx.now) {
            Some(v) if width == 4 => v,
            _ => {
                ctx.unhandled(off, width, None);
                0
            }
        }
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        if width != 4 || self.reg(off, ctx.now).is_none() {
            ctx.unhandled(off, width, Some(v));
            return;
        }
        let now = ctx.now;
        match off {
            0x00 | 0x10 | 0x20 => {
                let n = (off >> 4) as usize;
                self.latch(n, now);
                let c = &mut self.regs.ch[n];
                c.value = v;
                c.since = now;
            }
            0x04 | 0x14 | 0x24 => self.regs.ch[(off >> 4) as usize].load = v,
            CTRL => {
                for n in 0..CHANNELS {
                    self.latch(n, now);
                }
                self.regs.ctrl = v;
            }
            INT_STAT => self.regs.ists &= !v,
            INT_EN => {
                if v != 0 && !self.warned {
                    self.warned = true;
                    log::warn!("timer interrupts not modeled (INT_EN = {v:#x})");
                }
                self.regs.ien = v;
            }
            _ => {}
        }
    }

    fn reset(&mut self, kind: ResetKind, _ctx: &mut DevCtx<'_>) {
        match kind {
            // The firmware re-initializes the block on every boot (timer.md, "Model").
            ResetKind::System => {}
            ResetKind::PowerOn => self.regs = Regs::default(),
        }
    }

    fn save(&self) -> Vec<u8> {
        save_serde(&self.regs)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        load_serde(&mut self.regs, state)
    }

    fn inspect(&self, now: u64) -> Vec<RegView> {
        let mut rows = Vec::new();
        for n in 0..CHANNELS as u32 {
            for (name, off) in [("value", 0x10 * n), ("load", 0x10 * n + 4)] {
                // Every offset listed here is a register.
                let v = self.reg(off, now).unwrap_or(0);
                rows.push(RegView::new(format!("ch{n}_{name}"), off, v));
            }
        }
        for (name, off) in [("ctrl", CTRL), ("int_stat", INT_STAT), ("int_en", INT_EN)] {
            rows.push(RegView::new(name, off, self.reg(off, now).unwrap_or(0)));
        }
        rows
    }
}
