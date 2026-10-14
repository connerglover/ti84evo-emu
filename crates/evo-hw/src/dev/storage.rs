//! Plain register storage for blocks with no behavior: the LED pattern generator
//! (`docs/hardware/security.md` §6) and the ICACHE (`docs/hardware/spif-nor.md` §7). Other
//! devices also use it as their backing store for "storage" registers.
//!
//! Byte, halfword and word accesses read and write their little-endian lanes at any offset
//! inside the window. An access running past the end of the window, or of another width, reads
//! 0, ignores writes and is logged as unhandled. Every reset (System and PowerOn) clears the
//! window to 0: neither block is read back across a reset by the firmware.

use serde::{Deserialize, Serialize};

use crate::{DevCtx, Device, ResetKind, StateError, load_serde, save_serde};

/// Plain register storage, byte/halfword/word lanes, reset 0. Used for blocks with no behavior.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Storage {
    bytes: Vec<u8>,
}

impl Storage {
    /// `size` bytes of storage, all 0.
    pub fn new(size: u32) -> Self {
        Storage {
            bytes: vec![0; size as usize],
        }
    }

    /// The window size in bytes.
    pub fn size(&self) -> u32 {
        self.bytes.len() as u32
    }

    /// The little-endian word at `off` (bytes past the end read 0).
    pub fn word(&self, off: u32) -> u32 {
        let mut b = [0u8; 4];
        for (i, dst) in b.iter_mut().enumerate() {
            *dst = off
                .checked_add(i as u32)
                .and_then(|o| self.bytes.get(o as usize))
                .copied()
                .unwrap_or(0);
        }
        u32::from_le_bytes(b)
    }

    /// The bytes of a `width`-byte access at `off`, if `width` is 1, 2 or 4 and it fits.
    fn lanes(&self, off: u32, width: u8) -> Option<std::ops::Range<usize>> {
        if !matches!(width, 1 | 2 | 4) {
            return None;
        }
        let start = off as usize;
        let end = start.checked_add(usize::from(width))?;
        (end <= self.bytes.len()).then_some(start..end)
    }

    /// Read `width` (1, 2 or 4) bytes at `off`; `None` if the access does not fit.
    pub fn get(&self, off: u32, width: u8) -> Option<u32> {
        let r = self.lanes(off, width)?;
        let mut b = [0u8; 4];
        b[..r.len()].copy_from_slice(&self.bytes[r]);
        Some(u32::from_le_bytes(b))
    }

    /// Write the low `width` (1, 2 or 4) bytes of `v` at `off`; false if the access does not fit.
    pub fn set(&mut self, off: u32, width: u8, v: u32) -> bool {
        let Some(r) = self.lanes(off, width) else {
            return false;
        };
        let n = r.len();
        self.bytes[r].copy_from_slice(&v.to_le_bytes()[..n]);
        true
    }

    /// Zero every byte.
    pub fn clear(&mut self) {
        self.bytes.fill(0);
    }

    /// Replace the contents with `state`, refusing a state of another size (`self` unchanged).
    pub fn load_checked(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut s = self.clone();
        load_serde(&mut s, state)?;
        if s.bytes.len() != self.bytes.len() {
            return Err(StateError::Mismatch(format!(
                "register storage of {} bytes, expected {}",
                s.bytes.len(),
                self.bytes.len()
            )));
        }
        *self = s;
        Ok(())
    }
}

impl Device for Storage {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        self.get(off, width).unwrap_or_else(|| {
            ctx.unhandled(off, width, None);
            0
        })
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        if !self.set(off, width, v) {
            ctx.unhandled(off, width, Some(v));
        }
    }

    fn reset(&mut self, _kind: ResetKind, _ctx: &mut DevCtx<'_>) {
        self.clear();
    }

    fn save(&self) -> Vec<u8> {
        save_serde(self)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        self.load_checked(state)
    }
}
