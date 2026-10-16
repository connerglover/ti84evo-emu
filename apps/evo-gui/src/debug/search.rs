//! Memory search: byte patterns and a search that reads memory in bounded chunks, so a
//! search over the whole machine (about 35 MiB) runs over several frames instead of
//! freezing the UI.

use crate::backend::{EmuBackend, MemRegion};

/// Bytes read per chunk (plus `pattern.len() - 1` bytes of overlap).
pub const SEARCH_CHUNK: usize = 0x4000;
/// Chunks the Memory tab reads per frame: 256 × [`SEARCH_CHUNK`] = 4 MiB.
pub const SEARCH_CHUNKS_PER_FRAME: usize = 256;

/// Parses a search pattern: hex bytes (`"de ad be ef"` or `"deadbeef"`; every
/// whitespace-separated group has an even number of digits), or ASCII text in double
/// quotes (`"\"text\""`). `None` for anything else, including an empty pattern.
pub fn parse_pattern(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix('"') {
        let text = inner.strip_suffix('"')?;
        if text.is_empty() || !text.is_ascii() {
            return None;
        }
        return Some(text.as_bytes().to_vec());
    }
    let mut out = Vec::new();
    for group in s.split_whitespace() {
        if group.len() % 2 != 0 || !group.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        for pair in group.as_bytes().chunks(2) {
            let pair = std::str::from_utf8(pair).ok()?;
            out.push(u8::from_str_radix(pair, 16).ok()?);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// One stretch to scan: matches may start in `starts` and must end by `end`.
#[derive(Clone, Debug)]
struct Span {
    starts: std::ops::Range<u64>,
    end: u64,
}

/// A memory search that runs a bounded number of chunks per [`Search::step`].
///
/// It finds the first match starting at or after `from` in region order, then wraps once
/// to the start of the first region and finds the first match starting before `from`.
/// A match never spans two regions, and unreadable bytes never match.
#[derive(Clone, Debug)]
pub struct Search {
    pattern: Vec<u8>,
    spans: Vec<Span>,
    /// The span being scanned.
    span: usize,
    /// The next match start to try in that span.
    pos: u64,
}

impl Search {
    pub fn new(regions: Vec<MemRegion>, from: u32, pattern: Vec<u8>) -> Self {
        let from = u64::from(from);
        let bounds: Vec<(u64, u64)> = regions
            .iter()
            .map(|r| {
                let start = u64::from(r.start);
                (start, start + u64::from(r.len))
            })
            .collect();
        let len = pattern.len() as u64;
        let mut spans = Vec::new();
        if len > 0 {
            // First pass: starts at or after `from`; then the wrap: starts before it.
            let passes = [(from, u64::MAX), (0, from)];
            for (lo, hi) in passes {
                for &(start, end) in &bounds {
                    let last_start = (end + 1).saturating_sub(len);
                    let starts = start.max(lo)..last_start.min(hi);
                    if !starts.is_empty() {
                        spans.push(Span { starts, end });
                    }
                }
            }
        }
        let pos = spans.first().map_or(0, |s| s.starts.start);
        Self {
            pattern,
            spans,
            span: 0,
            pos,
        }
    }

    /// Scans up to `max_chunks` chunks (at least one). `None`: not done yet;
    /// `Some(Some(addr))`: the match; `Some(None)`: no match anywhere.
    pub fn step(&mut self, backend: &dyn EmuBackend, max_chunks: usize) -> Option<Option<u32>> {
        let len = self.pattern.len() as u64;
        for _ in 0..max_chunks.max(1) {
            let Some(span) = self.spans.get(self.span) else {
                return Some(None);
            };
            let first = self.pos;
            let stop = (first + SEARCH_CHUNK as u64).min(span.starts.end);
            // The starts `first..stop`, and the `len - 1` bytes after the last one.
            let read_end = (stop + len - 1).min(span.end);
            let addr = u32::try_from(first).ok()?;
            let bytes = backend.peek(addr, (read_end - first) as usize);
            let hit = bytes.windows(self.pattern.len()).position(|w| {
                w.iter()
                    .zip(&self.pattern)
                    .all(|(b, p)| b.is_some_and(|b| b == *p))
            });
            if let Some(i) = hit.filter(|&i| first + (i as u64) < stop) {
                self.span = self.spans.len();
                return Some(u32::try_from(first + i as u64).ok());
            }
            if stop >= span.starts.end {
                self.span += 1;
                self.pos = self.spans.get(self.span).map_or(0, |s| s.starts.start);
            } else {
                self.pos = stop;
            }
        }
        (self.span >= self.spans.len()).then_some(None)
    }
}

/// Runs a [`Search`] to completion: the first match at or after `from` in region order,
/// else (wrapping once) the first match before it.
pub fn find(
    backend: &dyn EmuBackend,
    regions: &[MemRegion],
    from: u32,
    pattern: &[u8],
) -> Option<u32> {
    let mut search = Search::new(regions.to_vec(), from, pattern.to_vec());
    loop {
        if let Some(result) = search.step(backend, SEARCH_CHUNKS_PER_FRAME) {
            return result;
        }
    }
}
