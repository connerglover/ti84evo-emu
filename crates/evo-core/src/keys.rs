//! Key scripts: a text form of timed key presses, for the CLI's `--keys` and tests.
//!
//! Tokens are separated by whitespace or `;`. A `KEY` is a TI-84 key name from
//! [`evo_hw::dev::keypad::KEY_NAMES`] (`enter`, `alpha`, `2nd`, `ln`, `sin`, digits, arrows,
//! `on`, …; any ASCII case) or `r<R>c<C>`, a matrix position with single digits R, C in 0–7.
//!
//! - `KEY` taps: press at the cursor, release [`TAP_HOLD_MS`] later; the cursor moves on by
//!   `TAP_HOLD_MS + TAP_GAP_MS`.
//! - `+KEY` presses and `-KEY` releases at the cursor (the cursor stays).
//! - `wait=MS` moves the cursor on by MS milliseconds.
//!
//! Times are milliseconds from the start of the run. Example, the Self-Test boot combo:
//! `+alpha +ln +enter wait=2000 -alpha -ln -enter`.

pub use evo_hw::dev::keypad::Key;

/// How long a tap holds its key, in milliseconds.
pub const TAP_HOLD_MS: u64 = 50;
/// The gap after a tap's release before the next token, in milliseconds.
pub const TAP_GAP_MS: u64 = 50;

/// One key change at a time in the script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyAction {
    /// Milliseconds from the start of the run.
    pub at_ms: u64,
    pub key: Key,
    pub pressed: bool,
}

/// A key script that cannot be parsed.
#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum KeyScriptError {
    /// Token `index` (0-based, in script order) is not a key, `+KEY`, `-KEY` or `wait=MS`.
    #[error("key script token {index} `{token}` is not valid")]
    BadToken { index: usize, token: String },
}

/// `r<R>c<C>` with single digits R, C in 0–7, or a key name.
fn parse_key(s: &str) -> Option<Key> {
    if let [b'r' | b'R', r @ b'0'..=b'7', b'c' | b'C', c @ b'0'..=b'7'] = s.as_bytes() {
        return Some(Key::Matrix {
            row: r - b'0',
            col: c - b'0',
        });
    }
    Key::from_name(s)
}

/// Parse `s` into key actions in script order (times never decrease).
pub fn parse_key_script(s: &str) -> Result<Vec<KeyAction>, KeyScriptError> {
    let mut out = Vec::new();
    let mut cursor: u64 = 0;
    let tokens = s
        .split(|c: char| c.is_whitespace() || c == ';')
        .filter(|t| !t.is_empty());
    for (index, token) in tokens.enumerate() {
        let bad = || KeyScriptError::BadToken {
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
        } else if let Some(k) = token.strip_prefix('+') {
            let key = parse_key(k).ok_or_else(bad)?;
            out.push(KeyAction {
                at_ms: cursor,
                key,
                pressed: true,
            });
        } else if let Some(k) = token.strip_prefix('-') {
            let key = parse_key(k).ok_or_else(bad)?;
            out.push(KeyAction {
                at_ms: cursor,
                key,
                pressed: false,
            });
        } else {
            let key = parse_key(token).ok_or_else(bad)?;
            out.push(KeyAction {
                at_ms: cursor,
                key,
                pressed: true,
            });
            out.push(KeyAction {
                at_ms: cursor.saturating_add(TAP_HOLD_MS),
                key,
                pressed: false,
            });
            cursor = cursor.saturating_add(TAP_HOLD_MS + TAP_GAP_MS);
        }
    }
    Ok(out)
}
