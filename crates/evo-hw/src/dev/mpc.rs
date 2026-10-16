//! Arm SIE-200 Memory Protection Controller (`docs/hardware/security.md` §1).
//!
//! A register model only: the block LUT is stored and read back, but no access is checked
//! against it, so `INT_STAT` stays 0 and IRQ 7 (`MPC_Handler`, fatal in the firmware) is never
//! asserted. Only aligned 32-bit accesses reach registers; any other width, and any offset not
//! listed below (PIDR/CIDR included), reads 0, ignores writes and is logged as unhandled.
//!
//! A System reset keeps every register (lock-down included); a PowerOn reset clears them.

use serde::{Deserialize, Serialize};

use crate::{DevCtx, Device, RegView, ResetKind, StateError, load_serde, save_serde};

/// `BLK_CFG` reported by every instance: block size `1 << (7 + 5)` = 4 KiB.
pub const MPC_BLK_CFG: u32 = 7;

const CTRL: u32 = 0x00;
const BLK_MAX: u32 = 0x10;
const BLK_CFG: u32 = 0x14;
const BLK_IDX: u32 = 0x18;
const BLK_LUT: u32 = 0x1C;
const INT_STAT: u32 = 0x20;
const INT_CLEAR: u32 = 0x24;
const INT_EN: u32 = 0x28;
const INT_INFO1: u32 = 0x2C;
const INT_INFO2: u32 = 0x30;
const INT_SET: u32 = 0x34;

/// CTRL bit 8: BLK_IDX advances after each BLK_LUT access.
const CTRL_AUTOINC: u32 = 1 << 8;
/// CTRL bit 31: sticky; afterwards CTRL and BLK_LUT writes are ignored.
const CTRL_LOCK: u32 = 1 << 31;

/// One SIE-200 MPC with `blk_max + 1` LUT words.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mpc {
    blk_max: u32,
    ctrl: u32,
    idx: u32,
    lut: Vec<u32>,
    int_en: u32,
    int_stat: u32,
}

impl Mpc {
    /// An MPC whose `BLK_MAX` reads `blk_max` (LUT words − 1), in its power-on state.
    pub fn new(blk_max: u32) -> Self {
        Mpc {
            blk_max,
            ctrl: 0,
            idx: 0,
            lut: vec![0; blk_max as usize + 1],
            int_en: 0,
            int_stat: 0,
        }
    }

    /// The block LUT: bit `b % 32` of word `b / 32` set means block `b` is Non-secure.
    pub fn lut(&self) -> &[u32] {
        &self.lut
    }

    /// Whether CTRL.SEC_LOCK_DOWN (bit 31) is set.
    pub fn locked(&self) -> bool {
        self.ctrl & CTRL_LOCK != 0
    }

    /// After a BLK_LUT access: advance the index if AUTOINC is on, saturating at BLK_MAX.
    fn lut_accessed(&mut self) {
        if self.ctrl & CTRL_AUTOINC != 0 {
            self.idx = (self.idx + 1).min(self.blk_max);
        }
    }
}

impl Device for Mpc {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        if width != 4 {
            ctx.unhandled(off, width, None);
            return 0;
        }
        match off {
            CTRL => self.ctrl,
            BLK_MAX => self.blk_max,
            BLK_CFG => MPC_BLK_CFG,
            BLK_IDX => self.idx,
            BLK_LUT => {
                let v = self.lut[self.idx as usize];
                self.lut_accessed();
                v
            }
            INT_STAT => self.int_stat,
            INT_EN => self.int_en,
            // Write-only (INT_CLEAR, INT_SET) and the unused fault info: read 0.
            INT_CLEAR | INT_SET | INT_INFO1 | INT_INFO2 => 0,
            _ => {
                ctx.unhandled(off, width, None);
                0
            }
        }
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        if width != 4 {
            ctx.unhandled(off, width, Some(v));
            return;
        }
        match off {
            CTRL => {
                if !self.locked() {
                    self.ctrl = v;
                }
            }
            BLK_IDX => self.idx = v.min(self.blk_max),
            BLK_LUT => {
                if !self.locked() {
                    self.lut[self.idx as usize] = v;
                }
                self.lut_accessed();
            }
            INT_CLEAR => {
                if v & 1 != 0 {
                    self.int_stat &= !1;
                }
            }
            INT_EN => self.int_en = v & 1,
            INT_SET => {
                if v & 1 != 0 {
                    self.int_stat |= 1;
                }
            }
            // Read-only registers: writes are ignored.
            BLK_MAX | BLK_CFG | INT_STAT | INT_INFO1 | INT_INFO2 => {}
            _ => ctx.unhandled(off, width, Some(v)),
        }
    }

    fn reset(&mut self, kind: ResetKind, _ctx: &mut DevCtx<'_>) {
        match kind {
            // The configuration (and the lock) survives SYSRESETREQ.
            ResetKind::System => {}
            ResetKind::PowerOn => *self = Mpc::new(self.blk_max),
        }
    }

    fn save(&self) -> Vec<u8> {
        save_serde(self)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut s = self.clone();
        load_serde(&mut s, state)?;
        if s.blk_max != self.blk_max || s.lut.len() != self.lut.len() || s.idx > s.blk_max {
            return Err(StateError::Mismatch(format!(
                "MPC state with BLK_MAX {} and {} LUT words (index {}), expected BLK_MAX {}",
                s.blk_max,
                s.lut.len(),
                s.idx,
                self.blk_max
            )));
        }
        *self = s;
        Ok(())
    }

    /// Every register a read decodes; `blk_lut` shows the LUT word at the current index
    /// without advancing it. Write-only INT_CLEAR / INT_SET and the fault info read 0.
    fn inspect(&self, _now: u64) -> Vec<RegView> {
        let lut = self.lut.get(self.idx as usize).copied().unwrap_or(0);
        let ctrl = RegView::new("ctrl", CTRL, self.ctrl);
        let ctrl = match (self.ctrl & CTRL_AUTOINC != 0, self.locked()) {
            (false, false) => ctrl,
            (true, false) => ctrl.with_note("autoinc"),
            (false, true) => ctrl.with_note("locked"),
            (true, true) => ctrl.with_note("autoinc locked"),
        };
        vec![
            ctrl,
            RegView::new("blk_max", BLK_MAX, self.blk_max),
            RegView::new("blk_cfg", BLK_CFG, MPC_BLK_CFG),
            RegView::new("blk_idx", BLK_IDX, self.idx),
            RegView::new("blk_lut", BLK_LUT, lut),
            RegView::new("int_stat", INT_STAT, self.int_stat),
            RegView::new("int_clear", INT_CLEAR, 0),
            RegView::new("int_en", INT_EN, self.int_en),
            RegView::new("int_info1", INT_INFO1, 0),
            RegView::new("int_info2", INT_INFO2, 0),
            RegView::new("int_set", INT_SET, 0),
        ]
    }
}
