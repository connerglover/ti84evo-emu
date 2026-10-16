//! Turns pointer and keyboard key events into backend `set_key` edges.
//!
//! Every press reaches the guest for at least [`MIN_HOLD_CYCLES`] of emulated
//! time, so a click whose press and release land in the same UI frame is still
//! seen by the OS's keypad scan.

use std::collections::BTreeMap;

use crate::backend::CLOCK_HZ;
use crate::keypad::KeyId;

/// The shortest press the guest sees: 50 ms at [`CLOCK_HZ`] (Plan 2's `TAP_HOLD_MS`).
pub const MIN_HOLD_CYCLES: u64 = 7_800_000;

const _: () = assert!(MIN_HOLD_CYCLES == 50 * CLOCK_HZ / 1000);

/// Where a press came from. A key stays down while any source holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Pointer,
    Keyboard,
}

impl Source {
    fn bit(self) -> u8 {
        match self {
            Source::Pointer => 1,
            Source::Keyboard => 2,
        }
    }
}

/// A key that is down in the backend.
#[derive(Clone, Copy, Debug)]
struct Hold {
    /// Bitmask of [`Source::bit`] values still pressing the key.
    sources: u8,
    /// Emulated cycle count of the press edge.
    pressed_at: u64,
    /// Set when no source remains but the minimum hold has not elapsed yet.
    release_at: Option<u64>,
}

impl Hold {
    fn earliest_release(&self) -> u64 {
        self.pressed_at.saturating_add(MIN_HOLD_CYCLES)
    }
}

/// Key state between the input sources and the backend. Times are emulated
/// cycle counts (`EmuBackend::cycles`).
#[derive(Debug, Default)]
pub struct KeyLatch {
    holds: BTreeMap<KeyId, Hold>,
    edges: Vec<(KeyId, bool)>,
}

impl KeyLatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Presses `key` for `src`. Emits a press edge only if the key is not down.
    pub fn press(&mut self, key: KeyId, src: Source, now: u64) {
        match self.holds.get_mut(&key) {
            Some(hold) => {
                hold.sources |= src.bit();
                hold.release_at = None;
            }
            None => {
                self.holds.insert(
                    key,
                    Hold {
                        sources: src.bit(),
                        pressed_at: now,
                        release_at: None,
                    },
                );
                self.edges.push((key, true));
            }
        }
    }

    /// Releases `key` for `src`. Once no source remains, the key is released at
    /// `max(now, pressed_at + MIN_HOLD_CYCLES)`.
    pub fn release(&mut self, key: KeyId, src: Source, now: u64) {
        let Some(hold) = self.holds.get_mut(&key) else {
            return;
        };
        if hold.sources & src.bit() == 0 {
            return;
        }
        hold.sources &= !src.bit();
        if hold.sources == 0 {
            self.schedule_release(key, now);
        }
    }

    /// Drops every source of every key, as if each were released at `now`.
    pub fn release_all(&mut self, now: u64) {
        let keys: Vec<KeyId> = self
            .holds
            .iter_mut()
            .filter(|(_, h)| h.sources != 0)
            .map(|(k, h)| {
                h.sources = 0;
                *k
            })
            .collect();
        for key in keys {
            self.schedule_release(key, now);
        }
    }

    /// Forgets all holds, pending releases and queued edges, emitting nothing.
    /// Used after a reset or state load, when `cycles()` jumps; the caller
    /// releases the backend's keys itself.
    pub fn clear(&mut self) {
        self.holds.clear();
        self.edges.clear();
    }

    /// Performs the pending releases whose minimum hold has elapsed by `now`.
    pub fn tick(&mut self, now: u64) {
        let due: Vec<KeyId> = self
            .holds
            .iter()
            .filter(|(_, h)| h.release_at.is_some_and(|at| at <= now))
            .map(|(k, _)| *k)
            .collect();
        for key in due {
            self.holds.remove(&key);
            self.edges.push((key, false));
        }
    }

    /// The `set_key` calls to make on the backend, in order.
    pub fn take_edges(&mut self) -> Vec<(KeyId, bool)> {
        std::mem::take(&mut self.edges)
    }

    /// The earliest cycle a pending release is due at (`pressed_at + MIN_HOLD_CYCLES` of a
    /// key no source holds any more); `None` when no release is pending. A key a source still
    /// holds has no release pending.
    pub fn next_release(&self) -> Option<u64> {
        self.holds.values().filter_map(|h| h.release_at).min()
    }

    /// True while the key is down in the backend (including a pending release).
    pub fn is_down(&self, key: KeyId) -> bool {
        self.holds.contains_key(&key)
    }

    fn schedule_release(&mut self, key: KeyId, now: u64) {
        let Some(hold) = self.holds.get_mut(&key) else {
            return;
        };
        let at = now.max(hold.earliest_release());
        if at <= now {
            self.holds.remove(&key);
            self.edges.push((key, false));
        } else {
            hold.release_at = Some(at);
        }
    }
}
