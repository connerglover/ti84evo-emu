//! System-control blocks: the Secure Privilege Control block SPCTRL (`docs/hardware/security.md`
//! §2) and the clock/reset unit CRU (§3 "Minimal CRU model").
//!
//! Both are word storage over a 0x1000 window with a few fixed or read-only registers. Only
//! aligned 32-bit accesses reach registers; any other width or alignment reads 0, ignores
//! writes and is logged as unhandled. Neither block drives an IRQ: SPCTRL's IRQ 8 would be
//! `(+0x20 & +0x28) != 0`, and +0x20 stays 0 because PPC enforcement is not modeled; the CRU
//! has none.
//!
//! Both keep every register across a System reset (`SYSRESETREQ`); a PowerOn reset restores
//! the reset values. The firmware re-applies its read-modify-writes idempotently, and the CRU
//! clock state BL2 left (+0x04, +0x1C) has to survive for the clock getters.

use serde::{Deserialize, Serialize};

use super::storage::Storage;
use crate::{DevCtx, Device, ResetKind, StateError, save_serde};

/// Size of each block's register window.
const WINDOW: u32 = 0x1000;

/// `true` for an aligned 32-bit access inside the window.
fn word_access(off: u32, width: u8) -> bool {
    width == 4 && off.is_multiple_of(4) && off < WINDOW
}

// --- SPCTRL ---------------------------------------------------------------------------------

/// SECPPCINTSTAT: PPC interrupt status, read-only (0 while PPC enforcement is not modeled).
const SPCTRL_INTSTAT: u32 = 0x20;
/// SECPPCINTCLR: write-1-to-clear on +0x20; reads 0.
const SPCTRL_INTCLR: u32 = 0x24;
/// Lifecycle/provisioning status: read-only 0 (the unprovisioned branch of every consumer).
const SPCTRL_LCS: u32 = 0xC0;

/// Secure Privilege Control at `0x5020_1000`: PPC attribute words and control bits as plain
/// storage, +0x20 read-only, +0x24 W1C on +0x20, +0xC0 read-only 0.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Spctrl {
    regs: Storage,
}

impl Default for Spctrl {
    fn default() -> Self {
        Self::new()
    }
}

impl Spctrl {
    /// SPCTRL in its power-on state (every register 0).
    pub fn new() -> Self {
        Spctrl {
            regs: Storage::new(WINDOW),
        }
    }

    /// The value a guest word read of `off` returns.
    pub fn reg(&self, off: u32) -> u32 {
        match off {
            SPCTRL_INTCLR | SPCTRL_LCS => 0,
            _ => self.regs.word(off),
        }
    }
}

impl Device for Spctrl {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        if !word_access(off, width) {
            ctx.unhandled(off, width, None);
            return 0;
        }
        self.reg(off)
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        if !word_access(off, width) {
            ctx.unhandled(off, width, Some(v));
            return;
        }
        match off {
            SPCTRL_INTSTAT | SPCTRL_LCS => {}
            SPCTRL_INTCLR => {
                let stat = self.regs.word(SPCTRL_INTSTAT) & !v;
                self.regs.set(SPCTRL_INTSTAT, 4, stat);
            }
            _ => {
                self.regs.set(off, 4, v);
            }
        }
    }

    fn reset(&mut self, kind: ResetKind, _ctx: &mut DevCtx<'_>) {
        match kind {
            // The PPC configuration survives SYSRESETREQ (security.md, open question 8).
            ResetKind::System => {}
            ResetKind::PowerOn => self.regs.clear(),
        }
    }

    fn save(&self) -> Vec<u8> {
        save_serde(&self.regs)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        self.regs.load_checked(state)
    }
}

// --- CRU ------------------------------------------------------------------------------------

/// +0xFC ID, read-only: `[31:16] == 0x0011` selects the 24 MHz reference clock.
pub const CRU_ID: u32 = 0x0011_0000;
/// +0x04 CLKSEL reset value: sysclk from the PLL, PCLK = sysclk >> 2 (the post-BL2 state).
pub const CRU_CLKSEL_RESET: u32 = 0x0002_0001;
/// +0x1C PLLCTRL reset value: N = 52, ÷4, enabled and locked (156 MHz).
pub const CRU_PLL_RESET: u32 = 0x0000_3433;
/// +0x0C UID_LO, read-only ("EVO1").
pub const CRU_UID_LO: u32 = 0x4556_4F31;
/// +0x10 CHIPINFO, read-only: variant bit 30 clear, strap 0, UID_HI 0.
pub const CRU_CHIPINFO: u32 = 0;

const CRU_CLKSEL: u32 = 0x04;
const CRU_UID: u32 = 0x0C;
const CRU_INFO: u32 = 0x10;
const CRU_PLLCTRL: u32 = 0x1C;
const CRU_IDREG: u32 = 0xFC;

/// PLLCTRL bit 0: enable.
const PLL_EN: u32 = 1 << 0;
/// PLLCTRL bit 1: lock, polled by `cru_pll_enable` with no timeout.
const PLL_LOCK: u32 = 1 << 1;

/// Clock/reset unit at `0x5020_0000`: word storage with the reset values above, the ID, UID
/// and chip-info words read-only, and PLLCTRL's LOCK bit reading as its ENABLE bit at once.
/// Clock gates (+0x14/+0x18) and peripheral resets (+0x20) are stored but have no effect.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cru {
    regs: Storage,
}

impl Default for Cru {
    fn default() -> Self {
        Self::new()
    }
}

impl Cru {
    /// The CRU in its power-on state.
    pub fn new() -> Self {
        let mut regs = Storage::new(WINDOW);
        regs.set(CRU_CLKSEL, 4, CRU_CLKSEL_RESET);
        regs.set(CRU_PLLCTRL, 4, CRU_PLL_RESET);
        Cru { regs }
    }

    /// The value a guest word read of `off` returns.
    pub fn reg(&self, off: u32) -> u32 {
        match off {
            CRU_IDREG => CRU_ID,
            CRU_UID => CRU_UID_LO,
            CRU_INFO => CRU_CHIPINFO,
            CRU_PLLCTRL => {
                let v = self.regs.word(off);
                (v & !PLL_LOCK) | ((v & PLL_EN) << 1)
            }
            _ => self.regs.word(off),
        }
    }
}

impl Device for Cru {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        if !word_access(off, width) {
            ctx.unhandled(off, width, None);
            return 0;
        }
        self.reg(off)
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        if !word_access(off, width) {
            ctx.unhandled(off, width, Some(v));
            return;
        }
        match off {
            CRU_IDREG | CRU_UID | CRU_INFO => {}
            _ => {
                self.regs.set(off, 4, v);
            }
        }
    }

    fn reset(&mut self, kind: ResetKind, _ctx: &mut DevCtx<'_>) {
        match kind {
            // Clock gates, resets and the PLL state survive SYSRESETREQ (security.md, open
            // question 8).
            ResetKind::System => {}
            ResetKind::PowerOn => *self = Cru::new(),
        }
    }

    fn save(&self) -> Vec<u8> {
        save_serde(&self.regs)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        self.regs.load_checked(state)
    }
}
