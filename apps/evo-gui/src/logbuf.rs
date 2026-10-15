//! Log buffer: `env_logger` output plus a copy of every Info-and-above line in a bounded ring that
//! the debug console's "Log" view drains.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

/// Lines kept in the global ring; older lines are dropped first.
pub const LOG_CAP: usize = 1000;

/// A bounded ring of log lines. Drained lines are gone; the oldest line is dropped when full.
#[derive(Debug)]
pub struct LogBuf {
    cap: usize,
    lines: Mutex<VecDeque<String>>,
}

impl LogBuf {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            lines: Mutex::new(VecDeque::with_capacity(cap.min(LOG_CAP))),
        }
    }

    fn lines(&self) -> std::sync::MutexGuard<'_, VecDeque<String>> {
        self.lines.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Appends `line`, dropping the oldest line when the ring is full.
    pub fn record(&self, line: String) {
        if self.cap == 0 {
            return;
        }
        let mut lines = self.lines();
        while lines.len() >= self.cap {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    /// The lines recorded since the last drain, oldest first.
    pub fn drain(&self) -> Vec<String> {
        self.lines().drain(..).collect()
    }
}

fn global() -> &'static LogBuf {
    static BUF: OnceLock<LogBuf> = OnceLock::new();
    BUF.get_or_init(|| LogBuf::new(LOG_CAP))
}

/// Records `line` in the global ring.
pub fn record(line: String) {
    global().record(line);
}

/// New lines of the global ring since the last drain, oldest first, at most [`LOG_CAP`].
pub fn drain() -> Vec<String> {
    global().drain()
}

/// Forwards to `env_logger` and copies Info and above into the global ring.
struct Tee {
    inner: env_logger::Logger,
}

impl log::Log for Tee {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info || self.inner.enabled(metadata)
    }

    fn log(&self, record: &log::Record<'_>) {
        if record.level() <= log::Level::Info {
            self::record(format!(
                "{} {}: {}",
                record.level(),
                record.target(),
                record.args()
            ));
        }
        if self.inner.matches(record) {
            self.inner.log(record);
        }
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

/// Installs the logger: `env_logger` configured from `RUST_LOG`, plus the ring copy. A second call
/// (or another logger already installed) leaves the first logger in place.
pub fn init() {
    let inner = env_logger::Builder::from_default_env().build();
    let max = inner.filter().max(log::LevelFilter::Info);
    if log::set_boxed_logger(Box::new(Tee { inner })).is_ok() {
        log::set_max_level(max);
    }
}
