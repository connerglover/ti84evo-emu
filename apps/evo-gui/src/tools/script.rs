//! Key scripts in the GUI: the text form of timed key presses that the CLI's `--keys` reads.
//!
//! Tokens are separated by whitespace or `;`. A `KEY` is a key's [`KeyId::core_name`]
//! (`enter`, `alpha`, `2nd`, `ln`, digits, arrows, `on`, …; any ASCII case).
//!
//! - `KEY` taps: press at the cursor, release [`TAP_HOLD_MS`] later; the cursor moves on by
//!   `TAP_HOLD_MS + TAP_GAP_MS`.
//! - `+KEY` presses and `-KEY` releases at the cursor (the cursor stays).
//! - `wait=MS` moves the cursor on by MS milliseconds.
//!
//! This is the core's key-script grammar without its `r<R>c<C>` matrix positions (the GUI
//! names keys only by [`KeyId`]); the tap constants are the core's, so a script means the same
//! in the CLI and the GUI.

use crate::backend::CLOCK_HZ;
use crate::keypad::KeyId;

/// How long a tap holds its key, in milliseconds (the core's value).
pub const TAP_HOLD_MS: u64 = 50;
/// The gap after a tap's release before the next token, in milliseconds (the core's value).
pub const TAP_GAP_MS: u64 = 50;

/// Extra hold [`to_script`] still writes as a tap: one 60 Hz frame (the real machine may
/// release a clicked key up to an instruction or a frame late).
const TAP_SLACK_MS: u64 = 17;

/// One key change in a script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptEvent {
    /// Milliseconds from the start of the script.
    pub at_ms: u64,
    pub key: KeyId,
    pub pressed: bool,
}

/// A key script that cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScriptError {
    /// Token `index` (0-based, in script order) is not a key, `+KEY`, `-KEY` or `wait=MS`.
    #[error("key script token {index} `{token}` is not valid")]
    BadToken { index: usize, token: String },
}

/// The key whose [`KeyId::core_name`] is `name`, ignoring ASCII case.
pub fn key_by_name(name: &str) -> Option<KeyId> {
    KeyId::ALL
        .into_iter()
        .find(|k| k.core_name().eq_ignore_ascii_case(name))
}

/// Parse `s` into key events in script order (times never decrease).
pub fn parse_script(s: &str) -> Result<Vec<ScriptEvent>, ScriptError> {
    let mut out = Vec::new();
    let mut cursor: u64 = 0;
    let tokens = s
        .split(|c: char| c.is_whitespace() || c == ';')
        .filter(|t| !t.is_empty());
    for (index, token) in tokens.enumerate() {
        let bad = || ScriptError::BadToken {
            index,
            token: token.to_owned(),
        };
        if let Some(ms) = token.strip_prefix("wait=") {
            let ms: u64 = ms
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| ms.parse().ok())
                .flatten()
                .ok_or_else(bad)?;
            cursor = cursor.saturating_add(ms);
        } else if let Some(name) = token.strip_prefix('+') {
            let key = key_by_name(name).ok_or_else(bad)?;
            out.push(ScriptEvent {
                at_ms: cursor,
                key,
                pressed: true,
            });
        } else if let Some(name) = token.strip_prefix('-') {
            let key = key_by_name(name).ok_or_else(bad)?;
            out.push(ScriptEvent {
                at_ms: cursor,
                key,
                pressed: false,
            });
        } else {
            let key = key_by_name(token).ok_or_else(bad)?;
            out.push(ScriptEvent {
                at_ms: cursor,
                key,
                pressed: true,
            });
            out.push(ScriptEvent {
                at_ms: cursor.saturating_add(TAP_HOLD_MS),
                key,
                pressed: false,
            });
            cursor = cursor.saturating_add(TAP_HOLD_MS + TAP_GAP_MS);
        }
    }
    Ok(out)
}

/// Write `events` as a key script that [`parse_script`] reads back to the same key changes.
///
/// Events are sorted by time (stable). A press at `t` whose very next event is the release of
/// the same key at `t + h`, with `TAP_HOLD_MS ≤ h ≤ TAP_HOLD_MS + 17` and the event after that
/// at `t + h + TAP_GAP_MS` or later (or none), becomes the bare key name, which re-parses to
/// an exact [`TAP_HOLD_MS`] hold. Every other event is `+name` or `-name`. A gap from the
/// cursor becomes `wait=N` before the token; tokens are joined by one space.
pub fn to_script(events: &[ScriptEvent]) -> String {
    let mut sorted = events.to_vec();
    sorted.sort_by_key(|e| e.at_ms);
    let mut tokens: Vec<String> = Vec::new();
    let mut cursor: u64 = 0;
    let mut i = 0;
    while i < sorted.len() {
        let e = sorted[i];
        // `e` is never before the cursor: a hold leaves it at its own time, and a bare tap
        // needs the next event at or after the re-parsed cursor (`is_tap`'s gap check).
        if e.at_ms > cursor {
            tokens.push(format!("wait={}", e.at_ms - cursor));
            cursor = e.at_ms;
        }
        let name = e.key.core_name();
        if is_tap(&sorted, i) {
            tokens.push(name.to_owned());
            cursor = cursor.saturating_add(TAP_HOLD_MS + TAP_GAP_MS);
            i += 2;
        } else {
            tokens.push(format!("{}{name}", if e.pressed { '+' } else { '-' }));
            i += 1;
        }
    }
    tokens.join(" ")
}

/// Whether `events[i]` (sorted) and the event after it form a tap [`to_script`] writes bare.
fn is_tap(events: &[ScriptEvent], i: usize) -> bool {
    let press = events[i];
    let Some(&release) = events.get(i + 1) else {
        return false;
    };
    if !press.pressed || release.pressed || release.key != press.key {
        return false;
    }
    let hold = release.at_ms - press.at_ms;
    if !(TAP_HOLD_MS..=TAP_HOLD_MS + TAP_SLACK_MS).contains(&hold) {
        return false;
    }
    events
        .get(i + 2)
        .is_none_or(|next| next.at_ms >= release.at_ms.saturating_add(TAP_GAP_MS))
}

/// Milliseconds from cycle `from_cycle` to `cycle` at [`CLOCK_HZ`], rounded down; 0 when
/// `cycle` is before `from_cycle`.
pub fn ms_between(from_cycle: u64, cycle: u64) -> u64 {
    let cycles = u128::from(cycle.saturating_sub(from_cycle));
    // At most u64::MAX * 1000 / CLOCK_HZ, which fits in a u64.
    (cycles * 1000 / u128::from(CLOCK_HZ)) as u64
}
