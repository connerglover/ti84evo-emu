//! Nested Vectored Interrupt Controller state for the external interrupts (exceptions 16+).

use serde::{Deserialize, Deserializer, Serialize};

/// Number of external interrupt lines.
pub const NUM_IRQS: usize = 480;
/// 32-bit words of per-IRQ bits.
pub const WORDS: usize = NUM_IRQS / 32;
/// End of the priority registers, as an offset from `NVIC_ISER0`.
const IPR_END: u32 = 0x300 + NUM_IRQS as u32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nvic {
    pub enabled: [u32; WORDS],
    pub pending: [u32; WORDS],
    pub active: [u32; WORDS],
    /// Current level of each (level-sensitive) input line.
    pub level: [u32; WORDS],
    /// Interrupt Target Non-secure: 0 routes the IRQ to Secure state (the reset value).
    pub itns: [u32; WORDS],
    /// Priority byte per IRQ (only the implemented top bits are stored). Always `NUM_IRQS`
    /// long: the IPR registers index it directly, so decoding any other length fails.
    #[serde(deserialize_with = "priority_bytes")]
    pub priority: Vec<u8>,
}

/// Decodes `Nvic::priority`, rejecting any length but `NUM_IRQS` (save states are untrusted).
fn priority_bytes<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    let v = Vec::<u8>::deserialize(d)?;
    if v.len() != NUM_IRQS {
        return Err(serde::de::Error::invalid_length(
            v.len(),
            &"one priority byte per IRQ (480)",
        ));
    }
    Ok(v)
}

impl Default for Nvic {
    fn default() -> Self {
        Self {
            enabled: [0; WORDS],
            pending: [0; WORDS],
            active: [0; WORDS],
            level: [0; WORDS],
            itns: [0; WORDS],
            priority: vec![0; NUM_IRQS],
        }
    }
}

fn bit(irq: usize) -> (usize, u32) {
    (irq / 32, 1 << (irq % 32))
}

/// Out-of-range IRQ numbers (an IPSR of 496..=511 restored from a stacked xPSR, for instance)
/// read as clear.
fn get(words: &[u32; WORDS], irq: usize) -> bool {
    let (w, b) = bit(irq);
    words.get(w).is_some_and(|&word| word & b != 0)
}

/// Writes to out-of-range IRQ numbers are ignored.
fn put(words: &mut [u32; WORDS], irq: usize, v: bool) {
    let (w, b) = bit(irq);
    let Some(word) = words.get_mut(w) else {
        return;
    };
    if v {
        *word |= b;
    } else {
        *word &= !b;
    }
}

/// Iterate the set bits of `words` as IRQ numbers.
pub(crate) fn irqs(words: &[u32; WORDS]) -> impl Iterator<Item = usize> + '_ {
    words.iter().enumerate().flat_map(|(w, &bits)| {
        let mut bits = bits;
        std::iter::from_fn(move || {
            if bits == 0 {
                return None;
            }
            let b = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            Some(w * 32 + b)
        })
    })
}

impl Nvic {
    pub fn is_enabled(&self, irq: usize) -> bool {
        get(&self.enabled, irq)
    }

    pub fn is_pending(&self, irq: usize) -> bool {
        get(&self.pending, irq)
    }

    pub fn is_active(&self, irq: usize) -> bool {
        get(&self.active, irq)
    }

    pub fn set_pending(&mut self, irq: usize, v: bool) {
        put(&mut self.pending, irq, v);
    }

    pub fn set_active(&mut self, irq: usize, v: bool) {
        put(&mut self.active, irq, v);
    }

    pub fn level(&self, irq: usize) -> bool {
        get(&self.level, irq)
    }

    pub fn set_level(&mut self, irq: usize, v: bool) {
        put(&mut self.level, irq, v);
    }

    /// True when the IRQ targets Secure state (ITNS bit clear).
    pub fn targets_secure(&self, irq: usize) -> bool {
        !get(&self.itns, irq)
    }

    /// True when Non-secure software may see IRQ `irq` (it targets Non-secure state).
    fn ns_visible(&self, irq: usize) -> bool {
        get(&self.itns, irq)
    }

    /// Read the word at offset `off` from `NVIC_ISER0` (0xE000_E100); `None` if unimplemented.
    /// Through the Non-secure view (`ns`), interrupts that target Secure state read as zero and
    /// `ITNS` is RAZ.
    pub fn read(&self, off: u32, ns: bool) -> Option<u32> {
        let i = ((off % 0x80) / 4) as usize;
        let visible = if ns {
            self.itns.get(i).copied().unwrap_or(0)
        } else {
            u32::MAX
        };
        let word = |w: &[u32; WORDS]| w.get(i).copied().unwrap_or(0) & visible;
        Some(match off {
            0x000..0x040 | 0x080..0x0C0 => word(&self.enabled),
            0x100..0x140 | 0x180..0x1C0 => word(&self.pending),
            0x200..0x240 => word(&self.active),
            0x280..0x2C0 if ns => 0,
            0x280..0x2C0 => word(&self.itns),
            0x300..IPR_END => {
                let base = (off - 0x300) as usize;
                u32::from_le_bytes(std::array::from_fn(|k| {
                    if ns && !self.ns_visible(base + k) {
                        0
                    } else {
                        self.priority[base + k]
                    }
                }))
            }
            _ => return None,
        })
    }

    /// Write `v` (already masked to the written byte lanes `lanes`) at offset `off`. Returns
    /// `None` if the register is unimplemented, else the IRQs that became pending. Through the
    /// Non-secure view (`ns`), interrupts that target Secure state and `ITNS` are WI.
    pub fn write(&mut self, off: u32, v: u32, lanes: u32, prio_mask: u8, ns: bool) -> Option<u32> {
        let i = ((off % 0x80) / 4) as usize;
        let bits = if ns {
            v & self.itns.get(i).copied().unwrap_or(0)
        } else {
            v
        };
        let mut newly_pending = 0;
        match off {
            0x000..0x040 if i < WORDS => self.enabled[i] |= bits,
            0x080..0x0C0 if i < WORDS => self.enabled[i] &= !bits,
            0x100..0x140 if i < WORDS => {
                newly_pending = bits & !self.pending[i];
                self.pending[i] |= bits;
            }
            // A level-sensitive line that is still asserted (and not active) pends again.
            0x180..0x1C0 if i < WORDS => {
                self.pending[i] &= !bits;
                self.pending[i] |= bits & self.level[i] & !self.active[i];
            }
            0x280..0x2C0 if i < WORDS && !ns => {
                self.itns[i] = (self.itns[i] & !lanes) | v;
            }
            0x300..IPR_END => {
                let base = (off - 0x300) as usize;
                for k in 0..4 {
                    if lanes >> (8 * k) & 0xFF != 0 && !(ns && !self.ns_visible(base + k)) {
                        self.priority[base + k] = (v >> (8 * k)) as u8 & prio_mask;
                    }
                }
            }
            // Out-of-range words of implemented banks, ITNS from Non-secure, and IABR are RAZ/WI.
            0x000..0x2C0 => {}
            _ => return None,
        }
        Some(newly_pending)
    }
}
