//! Turns the machine's cycle count into whole LCD frames (1/60 s ticks).

use crate::backend::CLOCK_HZ;

/// Cycles in one LCD frame: 1/60 s at [`CLOCK_HZ`].
pub const LCD_FRAME_CYCLES: u64 = CLOCK_HZ / 60;

/// Counts whole 1/60 s ticks between calls, keeping the part of a tick not yet counted.
#[derive(Clone, Debug)]
pub struct FrameSampler {
    /// The cycle count up to which ticks were counted.
    last: u64,
}

impl FrameSampler {
    /// A sampler that counts from cycle `now`.
    pub fn new(now: u64) -> Self {
        Self { last: now }
    }

    /// Whole ticks from the last counted point to `now` (saturating at `u32::MAX`). The rest
    /// of a tick is kept for the next call. When `now` is before the last point (the machine
    /// was reset or a state was loaded), it counts 0 and starts again from `now`.
    pub fn ticks(&mut self, now: u64) -> u32 {
        let Some(gap) = now.checked_sub(self.last) else {
            self.last = now;
            return 0;
        };
        let n = gap / LCD_FRAME_CYCLES;
        self.last += n * LCD_FRAME_CYCLES;
        u32::try_from(n).unwrap_or(u32::MAX)
    }
}
