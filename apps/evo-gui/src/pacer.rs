//! Frame pacing: how much to emulate per UI frame, and the speed meter.
//!
//! A frame runs at most [`MAX_FRAME_DT`] of emulated time and at most
//! [`WALL_BUDGET`] of wall time, so a slow host never spirals into catch-up
//! and the UI stays responsive.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::backend::{CLOCK_HZ, EmuBackend, RunStatus};

/// The longest frame interval emulated in one frame; longer gaps are dropped.
pub const MAX_FRAME_DT: f64 = 0.1;
/// The largest chunk passed to `run_for`.
pub const CHUNK_CYCLES: u64 = 200_000;
/// The smallest chunk the adaptive sizing shrinks to.
pub const MIN_CHUNK_CYCLES: u64 = 10_000;
/// Wall time one frame may spend emulating.
pub const WALL_BUDGET: Duration = Duration::from_millis(12);

/// The user-selected emulation speed.
///
/// [`Speed::Unlimited`] is 1000 times real time; [`WALL_BUDGET`] still caps
/// each frame, so it means "as fast as the host allows".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Speed {
    #[default]
    X1,
    X2,
    X4,
    Unlimited,
}

impl Speed {
    /// Every speed, slowest first.
    pub const ALL: [Speed; 4] = [Speed::X1, Speed::X2, Speed::X4, Speed::Unlimited];

    /// The multiplier for [`Pacer::speed`].
    pub fn factor(self) -> f64 {
        match self {
            Speed::X1 => 1.0,
            Speed::X2 => 2.0,
            Speed::X4 => 4.0,
            Speed::Unlimited => 1000.0,
        }
    }

    /// The menu label.
    pub fn label(self) -> &'static str {
        match self {
            Speed::X1 => "Speed 1\u{d7}",
            Speed::X2 => "Speed 2\u{d7}",
            Speed::X4 => "Speed 4\u{d7}",
            Speed::Unlimited => "Speed unlimited",
        }
    }
}

/// Converts frame intervals to cycle budgets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pacer {
    pub clock_hz: u64,
    /// Emulation speed multiplier (1.0 = real time).
    pub speed: f64,
}

impl Pacer {
    /// `round(clamp(dt, 0, MAX_FRAME_DT) * clock_hz * speed)`; 0 for NaN or a
    /// non-positive result.
    pub fn budget(&self, dt_secs: f64) -> u64 {
        if dt_secs.is_nan() {
            return 0;
        }
        let cycles = dt_secs.clamp(0.0, MAX_FRAME_DT) * self.clock_hz as f64 * self.speed;
        if cycles > 0.0 {
            // `as` saturates for out-of-range floats.
            cycles.round() as u64
        } else {
            0
        }
    }
}

/// The result of one [`FrameRunner::run`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameRun {
    /// Cycles the backend advanced.
    pub cycles: u64,
    /// The status of the last chunk (`Running` if no chunk ran).
    pub status: RunStatus,
}

/// Runs a frame's budget in chunks, adapting the chunk size so a slow build
/// still checks the wall clock often enough.
#[derive(Clone, Debug)]
pub struct FrameRunner {
    chunk: u64,
}

impl Default for FrameRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameRunner {
    pub fn new() -> Self {
        Self {
            chunk: CHUNK_CYCLES,
        }
    }

    /// The current chunk size, kept across frames.
    pub fn chunk(&self) -> u64 {
        self.chunk
    }

    /// Runs up to `budget` cycles. `elapsed` returns the wall time since the
    /// frame started; it is called once after each chunk, never before the first.
    pub fn run(
        &mut self,
        b: &mut dyn EmuBackend,
        budget: u64,
        wall: Duration,
        elapsed: &mut dyn FnMut() -> Duration,
    ) -> FrameRun {
        let start = b.cycles();
        let mut done = 0u64;
        let mut status = RunStatus::Running;
        let mut prev = Duration::ZERO;
        while done < budget {
            let before = b.cycles();
            status = b.run_for(self.chunk.min(budget - done));
            let after = b.cycles();
            done = after.saturating_sub(start);

            let now = elapsed();
            let took = now.saturating_sub(prev);
            prev = now;
            if took > WALL_BUDGET / 4 {
                self.chunk = (self.chunk / 2).max(MIN_CHUNK_CYCLES);
            } else if took < WALL_BUDGET / 16 {
                self.chunk = self.chunk.saturating_mul(2).min(CHUNK_CYCLES);
            }

            if status.is_stop() || status == RunStatus::Idle || after <= before || now >= wall {
                break;
            }
        }
        FrameRun {
            cycles: done,
            status,
        }
    }
}

/// `round(100 * cycles / (wall_secs * clock_hz))`; `None` if `wall_secs` is
/// not positive (or NaN) or `clock_hz` is 0.
pub fn speed_percent(cycles: u64, wall_secs: f64, clock_hz: u64) -> Option<u32> {
    if wall_secs.is_nan() || wall_secs <= 0.0 || clock_hz == 0 {
        return None;
    }
    let pct = 100.0 * cycles as f64 / (wall_secs * clock_hz as f64);
    // `as` saturates for out-of-range floats.
    pct.is_finite().then(|| pct.round() as u32)
}

/// Emulation speed in percent of real time, averaged over whole seconds.
#[derive(Clone, Debug, Default)]
pub struct SpeedMeter {
    cycles: u64,
    wall: f64,
    percent: Option<u32>,
}

impl SpeedMeter {
    /// Adds one frame. Once at least 1.0 s is collected, updates the percentage
    /// and starts a new window. Non-finite or negative wall times are ignored.
    pub fn push(&mut self, cycles: u64, wall_secs: f64) {
        if !wall_secs.is_finite() || wall_secs < 0.0 {
            return;
        }
        self.cycles = self.cycles.saturating_add(cycles);
        self.wall += wall_secs;
        if self.wall >= 1.0 {
            self.percent = speed_percent(self.cycles, self.wall, CLOCK_HZ);
            self.cycles = 0;
            self.wall = 0.0;
        }
    }

    /// `None` until the first full second.
    pub fn percent(&self) -> Option<u32> {
        self.percent
    }

    /// Forgets everything (called on pause).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}
