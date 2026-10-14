//! USB blocks reached by the NS BSP (`docs/hardware/usb.md`): the OTG controller at
//! `0x4000_3000` (IRQ 16) and the probable USB Type-C controller at `0x4010_C000` (IRQ 28).
//!
//! Both are register storage with the few behaviors the BSP needs to get past its USB
//! bring-up: the OTG ID/config constants and a self-clearing soft-reset bit, and the Type-C
//! CC1/CC2 attach state set by the host. Neither block asserts its IRQ.

use serde::{Deserialize, Serialize};

use super::storage::Storage;
use crate::{DevCtx, Device, ResetKind, StateError, load_serde, save_serde};

/// OTG `+0xE0` ID, read-only: bits 31:16 must be `0x0001` (BSP check at 0x0012665c).
pub const USB_ID: u32 = 0x0001_0000;
/// OTG `+0xE4` config, read-only: the endpoint-count fields the BSP accepts.
pub const USB_CONFIG: u32 = 0x0000_0880;

/// Both windows are 4 KiB.
const WINDOW: u32 = 0x1000;
/// OTG `+0xE0` / `+0xE4`.
const USB_ID_OFF: u32 = 0xE0;
const USB_CONFIG_OFF: u32 = 0xE4;
/// OTG `+0x000` bit 1: soft reset, reads 0 (the BSP polls it with no timeout, 0x0012746a).
const USB_SOFT_RESET: u8 = 1 << 1;
/// Type-C `+0x1C`: bits 9:8 = CC1, bits 11:10 = CC2, i.e. the low nibble of byte `+0x1D`.
const TC_CC_BYTE: u32 = 0x1D;
const TC_CC_MASK: u8 = 0x0F;

/// `(bit shift, offset)` of each byte lane of a `width`-byte access at `off`.
fn lane_offsets(off: u32, width: u8) -> impl Iterator<Item = (usize, u32)> {
    (0..u32::from(width)).filter_map(move |i| off.checked_add(i).map(|o| (i as usize * 8, o)))
}

/// USB OTG controller at `0x4000_3000`: byte/halfword/word storage over the window, except
/// that `+0xE0` / `+0xE4` read [`USB_ID`] / [`USB_CONFIG`] and ignore writes, and `+0x000`
/// bit 1 (soft reset) completes at once and reads 0. IRQ 16 is never asserted. Every reset
/// returns to the power-on state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Usb {
    regs: Storage,
}

impl Default for Usb {
    fn default() -> Self {
        Self::new()
    }
}

impl Usb {
    /// The controller in its power-on state.
    pub fn new() -> Self {
        Usb {
            regs: Storage::new(WINDOW),
        }
    }

    /// The constant byte at `off`, if `off` is in the ID or config register.
    fn const_byte(off: u32) -> Option<u8> {
        let (base, v) = match off {
            USB_ID_OFF..=0xE3 => (USB_ID_OFF, USB_ID),
            USB_CONFIG_OFF..=0xE7 => (USB_CONFIG_OFF, USB_CONFIG),
            _ => return None,
        };
        Some(v.to_le_bytes()[(off - base) as usize])
    }
}

impl Device for Usb {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        let Some(mut v) = self.regs.get(off, width) else {
            ctx.unhandled(off, width, None);
            return 0;
        };
        for (shift, o) in lane_offsets(off, width) {
            if let Some(b) = Self::const_byte(o) {
                v = (v & !(0xFF << shift)) | (u32::from(b) << shift);
            }
        }
        v
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        // Keep the read-only bytes' stored value (always 0): read back, merge, write.
        let Some(old) = self.regs.get(off, width) else {
            ctx.unhandled(off, width, Some(v));
            return;
        };
        let mut new = v;
        for (shift, o) in lane_offsets(off, width) {
            let keep = if Self::const_byte(o).is_some() {
                0xFF
            } else if o == 0 {
                u32::from(USB_SOFT_RESET)
            } else {
                0
            };
            let m = keep << shift;
            new = (new & !m) | (old & m);
        }
        self.regs.set(off, width, new);
    }

    fn reset(&mut self, _kind: ResetKind, _ctx: &mut DevCtx<'_>) {
        self.regs.clear();
    }

    fn save(&self) -> Vec<u8> {
        save_serde(&self.regs)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        self.regs.load_checked(state)
    }
}

/// USB Type-C controller at `0x4010_C000` (probable identity, `usb.md`): byte/halfword/word
/// storage for every offset, except that `+0x1C` bits 9:8 (CC1) and 11:10 (CC2) always read
/// the host-controlled attach state ([`UsbTypeC::set_cc`], power-on 0 = unattached) and guest
/// writes to those bits are dropped. IRQ 28 is never asserted. A System reset clears the
/// registers but keeps the host CC state (the cable stays plugged in); PowerOn clears both.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsbTypeC {
    regs: Storage,
    cc1: u8,
    cc2: u8,
}

impl Default for UsbTypeC {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbTypeC {
    /// The controller in its power-on state: storage 0, CC unattached.
    pub fn new() -> Self {
        UsbTypeC {
            regs: Storage::new(WINDOW),
            cc1: 0,
            cc2: 0,
        }
    }

    /// Set the CC1/CC2 attach state the guest reads at `+0x1C` bits 9:8 / 11:10. Each value
    /// is masked to 2 bits.
    pub fn set_cc(&mut self, cc1: u8, cc2: u8) {
        self.cc1 = cc1 & 3;
        self.cc2 = cc2 & 3;
    }

    /// The current host CC1/CC2 state.
    pub fn cc(&self) -> (u8, u8) {
        (self.cc1, self.cc2)
    }

    /// The low nibble of byte `+0x1D`: `cc1 | cc2 << 2`.
    fn cc_nibble(&self) -> u8 {
        self.cc1 | (self.cc2 << 2)
    }
}

impl Device for UsbTypeC {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        let Some(mut v) = self.regs.get(off, width) else {
            ctx.unhandled(off, width, None);
            return 0;
        };
        for (shift, o) in lane_offsets(off, width) {
            if o == TC_CC_BYTE {
                let m = u32::from(TC_CC_MASK) << shift;
                v = (v & !m) | (u32::from(self.cc_nibble()) << shift);
            }
        }
        v
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        let mut new = v;
        for (shift, o) in lane_offsets(off, width) {
            if o == TC_CC_BYTE {
                // The stored CC bits stay 0; reads take them from the host state.
                new &= !(u32::from(TC_CC_MASK) << shift);
            }
        }
        if !self.regs.set(off, width, new) {
            ctx.unhandled(off, width, Some(v));
        }
    }

    fn reset(&mut self, kind: ResetKind, _ctx: &mut DevCtx<'_>) {
        self.regs.clear();
        if kind == ResetKind::PowerOn {
            self.set_cc(0, 0);
        }
    }

    fn save(&self) -> Vec<u8> {
        save_serde(self)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut s = self.clone();
        load_serde(&mut s, state)?;
        if s.regs.size() != WINDOW {
            return Err(StateError::Mismatch(format!(
                "Type-C register storage of {} bytes, expected {WINDOW}",
                s.regs.size()
            )));
        }
        s.cc1 &= 3;
        s.cc2 &= 3;
        *self = s;
        Ok(())
    }
}
