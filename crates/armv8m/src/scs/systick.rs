//! SysTick timer (one instance per security state).

use serde::{Deserialize, Serialize};

/// `SYST_CALIB`: no reference clock (NOREF), TENMS not exact (SKEW), TENMS unknown.
pub const CALIB: u32 = 0xC000_0000;

const ENABLE: u32 = 1 << 0;
const TICKINT: u32 = 1 << 1;
/// ENABLE, TICKINT and CLKSOURCE. CLKSOURCE is stored but ignored: the counter always counts
/// processor cycles.
const CSR_MASK: u32 = 0b111;
const COUNTFLAG: u32 = 1 << 16;
const COUNTER_MASK: u32 = 0x00FF_FFFF;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SysTick {
    /// `SYST_CSR` control bits (ENABLE, TICKINT, CLKSOURCE).
    pub csr: u32,
    pub countflag: bool,
    /// `SYST_RVR` reload value (24 bits).
    pub rvr: u32,
    /// `SYST_CVR` current value (24 bits).
    pub cvr: u32,
}

impl SysTick {
    /// Read the register at `off` (0x0 CSR, 0x4 RVR, 0x8 CVR, 0xC CALIB). Reading CSR clears
    /// COUNTFLAG.
    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x0 => {
                let v = self.csr | if self.countflag { COUNTFLAG } else { 0 };
                self.countflag = false;
                v
            }
            0x4 => self.rvr,
            0x8 => self.cvr,
            _ => CALIB,
        }
    }

    /// Write the register at `off`. Any write to CVR clears it and COUNTFLAG.
    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x0 => self.csr = v & CSR_MASK,
            0x4 => self.rvr = v & COUNTER_MASK,
            0x8 => {
                self.cvr = 0;
                self.countflag = false;
            }
            _ => {}
        }
    }

    /// Advance the counter by `cycles` processor cycles. Returns true when it reached zero at
    /// least once with TICKINT set (the SysTick exception must be pended).
    ///
    /// The counter loads RVR on the cycle after it reads zero (or on the first cycle when
    /// enabled at zero) and signals on each 1 -> 0 transition; RVR = 0 stops it at zero.
    pub fn tick(&mut self, mut cycles: u32) -> bool {
        if self.csr & ENABLE == 0 {
            return false;
        }
        let mut wrapped = false;
        while cycles > 0 {
            if self.cvr == 0 {
                self.cvr = self.rvr;
                cycles -= 1;
                if self.rvr == 0 {
                    break;
                }
                // Skip whole periods (RVR decrements plus the reload cycle) at once.
                let period = self.rvr + 1;
                if cycles >= period {
                    cycles %= period;
                    wrapped = true;
                }
                continue;
            }
            let n = cycles.min(self.cvr);
            self.cvr -= n;
            cycles -= n;
            if self.cvr == 0 {
                wrapped = true;
            }
        }
        if wrapped {
            self.countflag = true;
        }
        wrapped && self.csr & TICKINT != 0
    }

    /// The fewest cycles after which `tick` returns true (pends the exception), or `None` if it
    /// never will without a register write: the counter is disabled, TICKINT is clear, or it is
    /// stopped at zero by RVR = 0.
    pub fn cycles_to_interrupt(&self) -> Option<u32> {
        if self.csr & ENABLE == 0 || self.csr & TICKINT == 0 {
            return None;
        }
        match (self.cvr, self.rvr) {
            (0, 0) => None,
            // One cycle reloads RVR, then RVR more count it down to zero.
            (0, rvr) => Some(1 + rvr),
            (cvr, _) => Some(cvr),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_to_interrupt_agrees_with_tick() {
        for rvr in [0, 1, 2, 5, 99, COUNTER_MASK] {
            for cvr in [0, 1, 3, rvr] {
                let st = SysTick {
                    csr: ENABLE | TICKINT,
                    countflag: false,
                    rvr,
                    cvr,
                };
                let state = format!("rvr {rvr} cvr {cvr}");
                match st.cycles_to_interrupt() {
                    Some(n) => {
                        assert!(n > 0, "{state}");
                        let mut early = st.clone();
                        assert!(!early.tick(n - 1), "{state}: fired before {n}");
                        assert!(early.tick(1), "{state}: did not fire at {n}");
                        assert!(st.clone().tick(n), "{state}: one tick of {n}");
                    }
                    None => assert!(!st.clone().tick(u32::MAX), "{state}"),
                }
            }
        }
        let quiet = SysTick {
            csr: ENABLE,
            countflag: false,
            rvr: 10,
            cvr: 5,
        };
        assert_eq!(quiet.cycles_to_interrupt(), None, "TICKINT clear");
    }
}
