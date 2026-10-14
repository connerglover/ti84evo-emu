//! Memory Protection Unit registers (one instance per security state). The registers are
//! stored so firmware can program and read them back; the protection itself is not enforced.

use serde::{Deserialize, Serialize};

/// Number of MPU regions.
pub const REGIONS: usize = 8;
/// `MPU_TYPE`: DREGION = 8, unified.
pub const TYPE: u32 = (REGIONS as u32) << 8;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mpu {
    /// `MPU_CTRL`: ENABLE, HFNMIENA, PRIVDEFENA.
    pub ctrl: u32,
    /// `MPU_RNR`: selected region.
    pub rnr: u32,
    pub rbar: [u32; REGIONS],
    pub rlar: [u32; REGIONS],
    pub mair: [u32; 2],
}

impl Mpu {
    /// Region addressed by an RBAR/RLAR alias: `RNR` for alias 0, `RNR[7:2]:n` for alias n.
    fn region(&self, alias: u32) -> Option<usize> {
        let r = if alias == 0 {
            self.rnr
        } else {
            (self.rnr & !3) + alias
        } as usize;
        (r < REGIONS).then_some(r)
    }

    /// Read the register at offset `off` from `MPU_TYPE` (0xE000_ED90); `None` if unimplemented.
    pub fn read(&self, off: u32) -> Option<u32> {
        Some(match off {
            0x00 => TYPE,
            0x04 => self.ctrl,
            0x08 => self.rnr,
            0x0C..=0x28 => {
                let alias = (off - 0x0C) / 8;
                let Some(r) = self.region(alias) else {
                    return Some(0);
                };
                if (off - 0x0C) & 4 == 0 {
                    self.rbar[r]
                } else {
                    self.rlar[r]
                }
            }
            0x30 => self.mair[0],
            0x34 => self.mair[1],
            _ => return None,
        })
    }

    /// Write the register at offset `off`; returns false if unimplemented.
    pub fn write(&mut self, off: u32, v: u32) -> bool {
        match off {
            0x00 => {}
            0x04 => {
                self.ctrl = v & 0b111;
                if v & 1 != 0 {
                    log::warn!("MPU enabled; MPU regions are stored but not enforced");
                }
            }
            0x08 => self.rnr = v & 0xFF,
            0x0C..=0x28 => {
                let alias = (off - 0x0C) / 8;
                if let Some(r) = self.region(alias) {
                    if (off - 0x0C) & 4 == 0 {
                        self.rbar[r] = v;
                    } else {
                        self.rlar[r] = v & !0x10;
                    }
                }
            }
            0x30 => self.mair[0] = v,
            0x34 => self.mair[1] = v,
            _ => return false,
        }
        true
    }
}
