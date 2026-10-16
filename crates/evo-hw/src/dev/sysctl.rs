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
use crate::{DevCtx, Device, RegView, ResetKind, StateError, save_serde};

/// Size of each block's register window.
const WINDOW: u32 = 0x1000;

/// `true` for an aligned 32-bit access inside the window.
fn word_access(off: u32, width: u8) -> bool {
    width == 4 && off.is_multiple_of(4) && off < WINDOW
}

/// Inspection rows for a word-storage block: every `named` register, plus every other word
/// that holds a non-zero value (named `w` and its offset in hex, e.g. `w014`), in offset
/// order. `reg` is the value a guest read of an offset returns.
fn storage_rows(named: &[(&str, u32)], regs: &Storage, reg: impl Fn(u32) -> u32) -> Vec<RegView> {
    (0..WINDOW)
        .step_by(4)
        .filter_map(|off| match named.iter().find(|&&(_, o)| o == off) {
            Some(&(name, _)) => Some(RegView::new(name, off, reg(off))),
            None => {
                (regs.word(off) != 0).then(|| RegView::new(format!("w{off:03x}"), off, reg(off)))
            }
        })
        .collect()
}

// --- SPCTRL ---------------------------------------------------------------------------------

/// SECPPCINTSTAT: PPC interrupt status, read-only (0 while PPC enforcement is not modeled).
const SPCTRL_INTSTAT: u32 = 0x20;
/// SECPPCINTCLR: write-1-to-clear on +0x20; reads 0.
const SPCTRL_INTCLR: u32 = 0x24;
/// Lifecycle/provisioning status: read-only 0 (the unprovisioned branch of every consumer).
const SPCTRL_LCS: u32 = 0xC0;
/// Deep-sleep control (`docs/hardware/security.md` §2): W `0x10` before the deep-sleep WFI,
/// W `0x100` after the wake.
const SPCTRL_DEEP_SLEEP: u32 = 0xD4;
/// +0xD4 bit 4: deep sleep armed.
const DEEP_SLEEP_ARMED: u32 = 0x10;

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

    /// True while the guest has armed deep sleep: +0xD4 bit 4, which the TF-M Power partition
    /// sets before its deep-sleep WFI and clears after the wake.
    pub fn deep_sleep_armed(&self) -> bool {
        self.regs.word(SPCTRL_DEEP_SLEEP) & DEEP_SLEEP_ARMED != 0
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

    fn inspect(&self, _now: u64) -> Vec<RegView> {
        let named = [
            ("intstat", SPCTRL_INTSTAT),
            ("intclr", SPCTRL_INTCLR),
            ("lcs", SPCTRL_LCS),
            ("deep_sleep", SPCTRL_DEEP_SLEEP),
        ];
        let mut rows = storage_rows(&named, &self.regs, |off| self.reg(off));
        if self.deep_sleep_armed()
            && let Some(row) = rows.iter_mut().find(|r| r.offset == SPCTRL_DEEP_SLEEP)
        {
            row.note = Some("armed".to_owned());
        }
        rows
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
/// +0x10 CHIPINFO, read-only: variant bit 30 clear, strap [29:25] = 1, UID_HI 0. The secure
/// image reads the strap as the hardware product ID (`cru_get_strap` 0x100b5cec, when OTP item
/// 50 is 0) and the OS service accepts image 1 only if it equals the image's product-ID TLV
/// 0xA0, which is 1 in OS 7.1 (`docs/hardware/security.md`).
pub const CRU_CHIPINFO: u32 = 1 << 25;

/// +0x08 WAKE_STATUS bit the ON key sets when it wakes the unit from deep sleep. Which bit
/// is the ON key is inferred: OS 7.1 resumes from deep sleep with any of bits 0, 1, 3 or 6
/// set (`docs/hardware/security.md` §3, "Wake from deep sleep"), and bit 0 is reason 0.
pub const CRU_WAKE_ON_KEY: u32 = 1 << 0;

const CRU_CLKSEL: u32 = 0x04;
const CRU_WAKE_STATUS: u32 = 0x08;
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

    /// A wake source fired: ORs `bits` into +0x08 WAKE_STATUS (the firmware clears it by
    /// writing 0).
    pub fn set_wake(&mut self, bits: u32) {
        let v = self.regs.word(CRU_WAKE_STATUS) | bits;
        self.regs.set(CRU_WAKE_STATUS, 4, v);
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

    fn inspect(&self, _now: u64) -> Vec<RegView> {
        let named = [
            ("clksel", CRU_CLKSEL),
            ("wake_status", CRU_WAKE_STATUS),
            ("uid", CRU_UID),
            ("info", CRU_INFO),
            ("pllctrl", CRU_PLLCTRL),
            ("idreg", CRU_IDREG),
        ];
        storage_rows(&named, &self.regs, |off| self.reg(off))
    }
}
