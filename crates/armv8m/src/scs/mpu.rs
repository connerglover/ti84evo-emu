//! Memory Protection Unit registers (one instance per security state). The registers are
//! stored so firmware can program and read them back, and `TT` reports the permissions they
//! describe; the protection itself is not enforced on accesses.

use serde::{Deserialize, Serialize};

/// Number of MPU regions.
pub const REGIONS: usize = 8;
/// `MPU_TYPE`: DREGION = 8, unified.
pub const TYPE: u32 = (REGIONS as u32) << 8;

// MPU_CTRL fields.
const CTRL_ENABLE: u32 = 1 << 0;
const CTRL_HFNMIENA: u32 = 1 << 1;
const CTRL_PRIVDEFENA: u32 = 1 << 2;
/// Region addresses have 32-byte granularity.
const GRANULE: u32 = 0x1F;

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

    /// Data access permissions at `addr` as `TT` reports them: `(read, write, region)`.
    /// `negative_priority` is true while executing at a priority below 0 (HardFault, NMI), where
    /// the MPU is bypassed unless `HFNMIENA` is set. The default memory map grants everything
    /// except unprivileged access to the PPB.
    pub fn permissions(
        &self,
        addr: u32,
        privileged: bool,
        negative_priority: bool,
    ) -> (bool, bool, Option<u8>) {
        // All of the PPB, the external PPB (0xE004_0000..) included, stays privileged-only, even
        // though only the internal PPB is handled by the CPU itself (see `scs::is_ppb`). Data
        // accesses enforce this in `exec::mem`: `ppb_read`/`ppb_write`, and `scs::is_eppb`.
        let ppb = (0xE000_0000..=0xE00F_FFFF).contains(&addr);
        if self.ctrl & CTRL_ENABLE == 0
            || ppb
            || (negative_priority && self.ctrl & CTRL_HFNMIENA == 0)
        {
            let ok = privileged || !ppb;
            return (ok, ok, None);
        }
        let mut hits = (0..REGIONS).filter(|&r| {
            let rlar = self.rlar[r];
            rlar & 1 != 0 && self.rbar[r] & !GRANULE <= addr && addr <= rlar | GRANULE
        });
        match (hits.next(), hits.next()) {
            (Some(r), None) => {
                // RBAR.AP: 00 RW privileged, 01 RW any, 10 RO privileged, 11 RO any.
                let (read, write) = match (self.rbar[r] >> 1) & 3 {
                    0b00 => (privileged, privileged),
                    0b01 => (true, true),
                    0b10 => (privileged, false),
                    _ => (true, false),
                };
                (read, write, Some(r as u8))
            }
            // Overlapping regions: no access.
            (Some(_), Some(_)) => (false, false, None),
            (None, _) => {
                let background = privileged && self.ctrl & CTRL_PRIVDEFENA != 0;
                (background, background, None)
            }
        }
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
