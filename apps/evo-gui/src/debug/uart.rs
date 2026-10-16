//! A bounded text console over a UART's output; the Console tab shows it.

/// The most bytes of text the console keeps.
pub const UART_CAP: usize = 256 * 1024;

/// UART output as text: `\r` dropped, decoded lossily, bounded to the last
/// [`UART_CAP`] bytes.
#[derive(Clone, Debug, Default)]
pub struct UartConsole {
    text: String,
    /// A trailing incomplete UTF-8 sequence (at most 3 bytes), kept for the next push.
    pending: Vec<u8>,
}

/// Where a trailing incomplete UTF-8 sequence starts in `buf`, or `buf.len()`
/// when `buf` does not end in one.
fn incomplete_tail(buf: &[u8]) -> usize {
    let start = buf.len().saturating_sub(3);
    for i in (start..buf.len()).rev() {
        if buf[i] & 0xC0 != 0x80 {
            // The last non-continuation byte: a lead byte whose sequence is
            // cut short, or anything else (complete or invalid).
            return match std::str::from_utf8(&buf[i..]) {
                Err(e) if e.error_len().is_none() => i,
                _ => buf.len(),
            };
        }
    }
    buf.len()
}

impl UartConsole {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends raw UART bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend(bytes.iter().copied().filter(|&b| b != b'\r'));
        let split = incomplete_tail(&buf);
        self.pending = buf.split_off(split);
        self.text.push_str(&String::from_utf8_lossy(&buf));
        if self.text.len() > UART_CAP {
            let mut cut = self.text.len() - UART_CAP;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(..cut);
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Forgets all text, including a pending partial character.
    pub fn clear(&mut self) {
        self.text.clear();
        self.pending.clear();
    }
}
