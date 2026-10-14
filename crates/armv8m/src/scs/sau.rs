//! Security attribution: the Security Attribution Unit (SAU) registers, the hook for the SoC's
//! Implementation Defined Attribution Unit (IDAU), and their combination as in the ARMv8-M
//! `SecurityCheck()` pseudocode. The SecureFault status registers (SFSR, SFAR) live here too.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Number of SAU regions (`SAU_TYPE.SREGION`).
pub const REGIONS: usize = 8;

// SAU_CTRL fields.
pub const CTRL_ENABLE: u32 = 1 << 0;
pub const CTRL_ALLNS: u32 = 1 << 1;
// SAU_RLAR fields.
const RLAR_ENABLE: u32 = 1 << 0;
const RLAR_NSC: u32 = 1 << 1;
/// SAU regions have 32-byte granularity.
const GRANULE: u32 = 0x1F;

// SFSR fields.
pub const SFSR_INVEP: u32 = 1 << 0;
pub const SFSR_INVIS: u32 = 1 << 1;
pub const SFSR_INVER: u32 = 1 << 2;
pub const SFSR_AUVIOL: u32 = 1 << 3;
pub const SFSR_INVTRAN: u32 = 1 << 4;
pub const SFSR_SFARVALID: u32 = 1 << 6;

/// The system space (PPB and vendor system region) is exempt from security attribution: it is
/// accessed in the current security state (the PPB has its own Secure/Non-secure views).
const EXEMPT_START: u32 = 0xE000_0000;

/// Security attribution of an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Attr {
    Secure,
    /// Secure memory in which Non-secure code may enter Secure state at an `SG` instruction.
    NonSecureCallable,
    NonSecure,
    /// Exempt from attribution: accessed in the current security state.
    Exempt,
}

/// Implementation Defined Attribution Unit: the SoC's fixed security map. It is always combined
/// with the SAU, most secure wins: with the SAU disabled and `ALLNS` clear everything is Secure
/// anyway, with `ALLNS` set the IDAU decides. An `Exempt` result and region numbers apply
/// whether or not the SAU is enabled.
pub trait Idau: Send + Sync {
    /// Attribution of `addr` and the number of the IDAU region holding it (reported by `TT` as
    /// IREGION). `None` means the IDAU does not cover `addr`: it imposes nothing and the SAU
    /// decides. `NonSecureCallable` marks Secure memory in which the SAU may define Non-secure
    /// callable regions (`IDAUNSC`); `Secure` memory can never become Non-secure callable.
    fn attr(&self, addr: u32) -> Option<(Attr, Option<u8>)>;
}

/// The default IDAU, following the Arm convention: address bit 28 set is Secure, clear is
/// Non-secure. As in Arm's reference subsystems the Secure half is reported as NSC-capable, so
/// the SAU's NSC regions (the CMSE veneers in Secure flash) take effect; where the SAU says
/// Secure that wins, and where it says Non-secure (an NS region, or `ALLNS`) the result is
/// NSC. There are no region numbers.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultIdau;

impl Idau for DefaultIdau {
    fn attr(&self, addr: u32) -> Option<(Attr, Option<u8>)> {
        let attr = if addr & 0x1000_0000 != 0 {
            Attr::NonSecureCallable
        } else {
            Attr::NonSecure
        };
        Some((attr, None))
    }
}

/// The installed IDAU. It is configuration rather than state: it is not serialized (a restored
/// `Cpu` gets `DefaultIdau` until the machine installs its own again) and `PartialEq` ignores it.
#[derive(Clone)]
pub(crate) struct IdauHandle(Arc<dyn Idau>);

impl IdauHandle {
    pub fn new(idau: Box<dyn Idau>) -> Self {
        Self(Arc::from(idau))
    }

    pub fn get(&self) -> &dyn Idau {
        &*self.0
    }
}

impl Default for IdauHandle {
    fn default() -> Self {
        Self(Arc::new(DefaultIdau))
    }
}

impl fmt::Debug for IdauHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IdauHandle")
    }
}

impl PartialEq for IdauHandle {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for IdauHandle {}

/// Result of `SecurityCheck()`: the attribution and the SAU/IDAU region numbers (for `TT`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SecAttr {
    pub attr: Attr,
    pub sregion: Option<u8>,
    pub iregion: Option<u8>,
}

/// SAU registers (`0xE000_EDD0..=0xE000_EDE8`, Secure only) and the SecureFault status.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sau {
    /// `SAU_CTRL`: ENABLE, ALLNS.
    pub ctrl: u32,
    /// `SAU_RNR`: selected region.
    pub rnr: u32,
    /// `SAU_RBAR`: BADDR[31:5].
    pub rbar: [u32; REGIONS],
    /// `SAU_RLAR`: LADDR[31:5], NSC, ENABLE.
    pub rlar: [u32; REGIONS],
    /// `SFSR` (write-one-to-clear).
    pub sfsr: u32,
    /// `SFAR`, valid while `SFSR.SFARVALID` is set.
    pub sfar: u32,
}

impl Sau {
    fn selected(&self) -> Option<usize> {
        let r = self.rnr as usize;
        (r < REGIONS).then_some(r)
    }

    /// Read the register at offset `off` from `SAU_CTRL` (0xE000_EDD0); `None` if unimplemented.
    pub fn read(&self, off: u32) -> Option<u32> {
        Some(match off {
            0x00 => self.ctrl,
            0x04 => REGIONS as u32,
            0x08 => self.rnr,
            0x0C => self.selected().map_or(0, |r| self.rbar[r]),
            0x10 => self.selected().map_or(0, |r| self.rlar[r]),
            0x14 => self.sfsr,
            0x18 => self.sfar,
            _ => return None,
        })
    }

    /// Write the register at offset `off`; returns false if unimplemented. Region writes with
    /// `SAU_RNR` out of range are ignored.
    pub fn write(&mut self, off: u32, v: u32) -> bool {
        match off {
            0x00 => self.ctrl = v & (CTRL_ENABLE | CTRL_ALLNS),
            0x04 => {}
            0x08 => self.rnr = v & 0xFF,
            0x0C => {
                if let Some(r) = self.selected() {
                    self.rbar[r] = v & !GRANULE;
                }
            }
            0x10 => {
                if let Some(r) = self.selected() {
                    self.rlar[r] = v & (!GRANULE | RLAR_NSC | RLAR_ENABLE);
                }
            }
            0x14 => self.sfsr &= !v,
            0x18 => self.sfar = v,
            _ => return false,
        }
        true
    }

    fn region_contains(&self, r: usize, addr: u32) -> bool {
        let rlar = self.rlar[r];
        rlar & RLAR_ENABLE != 0 && self.rbar[r] & !GRANULE <= addr && addr <= rlar | GRANULE
    }

    /// `SecurityCheck()` (ruling R2a). Exempt addresses (the system space, or the IDAU's
    /// `Exempt`) come first. Then the SAU: disabled, every address is Secure, or Non-secure with
    /// `ALLNS`; enabled, an address in no region, or in more than one, is Secure. Finally the
    /// IDAU can make the result more secure (most secure wins).
    pub(crate) fn check(&self, addr: u32, idau: &dyn Idau) -> SecAttr {
        let exempt = SecAttr {
            attr: Attr::Exempt,
            sregion: None,
            iregion: None,
        };
        if addr >= EXEMPT_START {
            return exempt;
        }
        let idau = idau.attr(addr);
        if let Some((Attr::Exempt, _)) = idau {
            return exempt;
        }

        let (mut ns, mut nsc, mut sregion) = (false, false, None);
        if self.ctrl & CTRL_ENABLE != 0 {
            let mut hits = (0..REGIONS).filter(|&r| self.region_contains(r, addr));
            if let (Some(r), None) = (hits.next(), hits.next()) {
                nsc = self.rlar[r] & RLAR_NSC != 0;
                ns = !nsc;
                sregion = Some(r as u8);
            }
        } else if self.ctrl & CTRL_ALLNS != 0 {
            ns = true;
        }
        // Most secure wins.
        if let Some((attr, _)) = idau {
            let (idau_ns, idau_nsc) = match attr {
                Attr::NonSecure => (true, false),
                Attr::NonSecureCallable => (false, true),
                _ => (false, false),
            };
            if !idau_ns && (ns || (!idau_nsc && nsc)) {
                ns = false;
                nsc = idau_nsc;
            }
        }
        SecAttr {
            attr: if ns {
                Attr::NonSecure
            } else if nsc {
                Attr::NonSecureCallable
            } else {
                Attr::Secure
            },
            sregion,
            iregion: idau.and_then(|(_, r)| r),
        }
    }
}
