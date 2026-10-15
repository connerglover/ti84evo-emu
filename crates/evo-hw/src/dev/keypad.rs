//! Keypad scanner at `0x4010_3000` (IRQ 13) and the ON key (IRQ 0) (`docs/hardware/keypad.md`).
//!
//! Registers: MODE +0x00 and SIZE +0x04 are plain storage, STATUS +0x08 (bit 1 key change,
//! bit 0 scan done; write 1 to clear), MASK +0x0C (the same two bits), and the 8 row registers
//! at +0x10 + 2·r, one halfword per row with bit `c` set while the key at (row r, column c) is
//! held. MODE, SIZE, STATUS and MASK take aligned 32-bit accesses only; the rows read at any
//! aligned width (a word read returns rows r and r + 1) and are read-only. Every other access
//! reads 0, ignores writes and is reported as unhandled.
//!
//! A press or a release changes the row bit, sets STATUS bit 1 and schedules a scan
//! [`SCAN_CYCLES`] later. A scan sets STATUS bit 0 and schedules the next one while any key is
//! held or the guest unmasks scan-done (MASK bit 0; unmasking it starts the scanner): the BSP
//! decodes the rows on the sixth scan-done after a key change, so a release, too, needs six
//! scans before the BSP sees the key go up. IRQ 13 (level) = `STATUS & MASK != 0`.
//!
//! The ON key is not in the matrix: a press pulses IRQ 0 (its handler reads no register), a
//! release does nothing.
//!
//! A System reset returns the registers to 0 but keeps the held keys (the user's fingers stay
//! on the keys): if any is held, STATUS bit 1 is set again and a scan is scheduled, so a key
//! combo held across the reset is seen as a key-down once the guest unmasks it. PowerOn
//! releases every key.

use serde::{Deserialize, Serialize};

use crate::{CPU_HZ, DevCtx, Device, ResetKind, StateError, load_serde, save_serde};

/// One key: a matrix position (`row`, `col` in `0..8`, register +0x10 + 2·row, bit `col`) or
/// the ON key, which has its own IRQ line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Key {
    Matrix { row: u8, col: u8 },
    On,
}

/// Cycles from a key change to the scan that reports it, and between scans while keys are
/// held: 1 ms at [`CPU_HZ`] (the real scan period is unknown; `keypad.md`).
pub const SCAN_CYCLES: u64 = CPU_HZ / 1_000;
/// The keypad scanner's IRQ line.
pub const KEYPAD_IRQ: u16 = 13;
/// The ON key's IRQ line (edge).
pub const ON_IRQ: u16 = 0;

const fn mk(row: u8, col: u8) -> Key {
    Key::Matrix { row, col }
}

/// Every key by its name (lower case; `docs/hardware/keypad.md` "Key map"): the TI-84 Evo
/// matrix, BSP key code = (7 − row)·8 + col + 1. It is the TI-84 Plus CE matrix except that
/// TOGGLE takes the CE's + position and pushes + − × ÷ one column on (÷ where the CE has ^),
/// ^ is where the CE has x⁻¹, and the fraction key where it has APPS; each key is checked on
/// the real OS by the `all_keys_type_expected_text` milestone test.
pub const KEY_NAMES: &[(&str, Key)] = &[
    ("down", mk(7, 0)),
    ("left", mk(7, 1)),
    ("right", mk(7, 2)),
    ("up", mk(7, 3)),
    ("enter", mk(6, 0)),
    ("toggle", mk(6, 1)),
    ("add", mk(6, 2)),
    ("sub", mk(6, 3)),
    ("mul", mk(6, 4)),
    ("div", mk(6, 5)),
    ("clear", mk(6, 6)),
    ("chs", mk(5, 0)),
    ("3", mk(5, 1)),
    ("6", mk(5, 2)),
    ("9", mk(5, 3)),
    ("rparen", mk(5, 4)),
    ("tan", mk(5, 5)),
    ("vars", mk(5, 6)),
    ("decpnt", mk(4, 0)),
    ("2", mk(4, 1)),
    ("5", mk(4, 2)),
    ("8", mk(4, 3)),
    ("lparen", mk(4, 4)),
    ("cos", mk(4, 5)),
    ("prgm", mk(4, 6)),
    ("stat", mk(4, 7)),
    ("0", mk(3, 0)),
    ("1", mk(3, 1)),
    ("4", mk(3, 2)),
    ("7", mk(3, 3)),
    ("comma", mk(3, 4)),
    ("sin", mk(3, 5)),
    ("frac", mk(3, 6)),
    ("xton", mk(3, 7)),
    ("sto", mk(2, 1)),
    ("ln", mk(2, 2)),
    ("log", mk(2, 3)),
    ("square", mk(2, 4)),
    ("power", mk(2, 5)),
    ("math", mk(2, 6)),
    ("alpha", mk(2, 7)),
    ("graph", mk(1, 0)),
    ("trace", mk(1, 1)),
    ("zoom", mk(1, 2)),
    ("window", mk(1, 3)),
    ("yequ", mk(1, 4)),
    ("2nd", mk(1, 5)),
    ("mode", mk(1, 6)),
    ("del", mk(1, 7)),
    ("on", Key::On),
];

impl Key {
    /// The key called `name` in [`KEY_NAMES`], ignoring ASCII case.
    pub fn from_name(name: &str) -> Option<Key> {
        KEY_NAMES
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|&(_, k)| k)
    }

    /// This key's name in [`KEY_NAMES`]; `None` for a matrix position no key uses.
    pub fn name(self) -> Option<&'static str> {
        KEY_NAMES.iter().find(|&&(_, k)| k == self).map(|&(n, _)| n)
    }
}

const ROWS: usize = 8;

const MODE: u32 = 0x00;
const SIZE: u32 = 0x04;
const STATUS: u32 = 0x08;
const MASK: u32 = 0x0C;
const ROW0: u32 = 0x10;
const ROW_END: u32 = ROW0 + 2 * ROWS as u32;

/// STATUS / MASK bits.
const SCAN_DONE: u32 = 1 << 0;
/// Bit 1: a key went down or up since the guest last cleared it.
const KEY_CHANGE: u32 = 1 << 1;
const INT_BITS: u32 = SCAN_DONE | KEY_CHANGE;

/// The keypad scanner.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Keypad {
    mode: u32,
    size: u32,
    status: u32,
    mask: u32,
    /// Held keys: bit `c` of `rows[r]` for the key at (r, c).
    rows: [u8; ROWS],
}

impl Keypad {
    /// The scanner at power-on: registers 0, no key held.
    pub fn new() -> Self {
        Self::default()
    }

    /// Press or release `key`. A matrix position outside 0..8 logs a warning and changes
    /// nothing; pressing a held key or releasing a free one changes nothing.
    pub fn set_key(&mut self, key: Key, pressed: bool, ctx: &mut DevCtx<'_>) {
        let (row, col) = match key {
            Key::On => {
                if pressed {
                    ctx.pulse_irq(ON_IRQ);
                }
                return;
            }
            Key::Matrix { row, col } if usize::from(row) < ROWS && col < 8 => {
                (usize::from(row), col)
            }
            Key::Matrix { row, col } => {
                log::warn!("keypad: no key at row {row}, column {col}");
                return;
            }
        };
        let bit = 1u8 << col;
        if (self.rows[row] & bit != 0) == pressed {
            return;
        }
        if pressed {
            self.rows[row] |= bit;
        } else {
            self.rows[row] &= !bit;
        }
        self.status |= KEY_CHANGE;
        ctx.schedule_at(ctx.now.saturating_add(SCAN_CYCLES));
        self.update_irq(ctx);
    }

    /// Whether `key` is held (always false for [`Key::On`], which is not latched).
    pub fn pressed(&self, key: Key) -> bool {
        match key {
            Key::Matrix { row, col } if usize::from(row) < ROWS && col < 8 => {
                self.rows[usize::from(row)] & (1 << col) != 0
            }
            _ => false,
        }
    }

    fn any_held(&self) -> bool {
        self.rows.iter().any(|&r| r != 0)
    }

    fn update_irq(&self, ctx: &mut DevCtx<'_>) {
        ctx.set_irq(KEYPAD_IRQ, self.status & self.mask != 0);
    }

    /// Byte `i` of the row registers (row `i / 2`, low byte = columns, high byte 0).
    fn row_byte(&self, i: u32) -> u32 {
        match self.rows.get((i / 2) as usize) {
            Some(&r) if i.is_multiple_of(2) => u32::from(r),
            _ => 0,
        }
    }
}

/// An aligned word access to one of the four control registers.
fn ctrl_access(off: u32, width: u8) -> bool {
    width == 4 && off.is_multiple_of(4) && off < ROW0
}

/// An aligned access inside the row registers.
fn row_access(off: u32, width: u8) -> bool {
    (ROW0..ROW_END).contains(&off)
        && matches!(width, 1 | 2 | 4)
        && off.is_multiple_of(u32::from(width))
        && off + u32::from(width) <= ROW_END
}

impl Device for Keypad {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        if ctrl_access(off, width) {
            return match off {
                MODE => self.mode,
                SIZE => self.size,
                STATUS => self.status,
                MASK => self.mask,
                // `ctrl_access` admits only the four offsets above.
                _ => 0,
            };
        }
        if row_access(off, width) {
            let base = off - ROW0;
            return (0..u32::from(width))
                .map(|i| self.row_byte(base + i) << (8 * i))
                .fold(0, |a, b| a | b);
        }
        ctx.unhandled(off, width, None);
        0
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        if !ctrl_access(off, width) {
            ctx.unhandled(off, width, Some(v));
            return;
        }
        match off {
            MODE => self.mode = v,
            SIZE => self.size = v,
            STATUS => self.status &= !v,
            MASK => {
                let unmasked = v & !self.mask & SCAN_DONE != 0;
                self.mask = v & INT_BITS;
                // While keys are held the scanner already runs.
                if unmasked && !self.any_held() {
                    ctx.schedule_at(ctx.now.saturating_add(SCAN_CYCLES));
                }
            }
            _ => {}
        }
        self.update_irq(ctx);
    }

    /// A scan: STATUS bit 0, and the next scan while keys are held or scan-done is unmasked.
    fn event(&mut self, ctx: &mut DevCtx<'_>) {
        self.status |= SCAN_DONE;
        if self.any_held() || self.mask & SCAN_DONE != 0 {
            ctx.schedule_at(ctx.now.saturating_add(SCAN_CYCLES));
        }
        self.update_irq(ctx);
    }

    fn reset(&mut self, kind: ResetKind, ctx: &mut DevCtx<'_>) {
        let rows = match kind {
            ResetKind::System => self.rows,
            ResetKind::PowerOn => [0; ROWS],
        };
        *self = Keypad {
            rows,
            ..Keypad::new()
        };
        if self.any_held() {
            self.status = KEY_CHANGE;
            ctx.schedule_at(ctx.now.saturating_add(SCAN_CYCLES));
        } else {
            ctx.cancel_event();
        }
        self.update_irq(ctx);
    }

    fn save(&self) -> Vec<u8> {
        save_serde(self)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut s = self.clone();
        load_serde(&mut s, state)?;
        s.status &= INT_BITS;
        s.mask &= INT_BITS;
        *self = s;
        Ok(())
    }
}
