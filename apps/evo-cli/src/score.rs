//! Slot-base discovery: score candidate flash addresses for an MCUboot image
//! whose load address the package does not record.
//!
//! A *slot* is an image as programmed into flash: MCUboot header, then
//! payload. Offsets here are slot offsets, header included. A candidate
//! *base* is the flash offset of the slot's first byte, so the byte at slot
//! offset `o` is addressed as `base + o` (Non-secure alias) or
//! `(base | SECURE_ALIAS) + o` (Secure alias). Every criterion tries both.
//!
//! The criteria:
//! 1. +10 per vector-table entry (the words after the initial SP at
//!    `hdr_size`) that is odd, lands inside the slot and decodes there to
//!    something other than [`Instr::Undefined`].
//! 2. +1 per word-aligned 32-bit literal in the payload equal to the address
//!    of the start of a NUL-terminated printable string (at least 6
//!    characters) in the payload.
//! 3. +1 per BL in the first 64 KiB of code whose target is inside the
//!    payload and is a PUSH. BL is PC-relative, so this is the same for every
//!    base: it shows the payload is Thumb code, not where it lives.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use armv8m::decode::is_32bit;
use armv8m::{Instr, decode16, decode32};

/// Setting bit 28 of a flash offset gives its Secure alias; the offset itself
/// is the Non-secure alias address of the same byte.
pub const SECURE_ALIAS: u32 = 0x1000_0000;

/// Criterion 1 weight.
const VECTOR_POINTS: u32 = 10;
/// The initial SP plus 15 system exceptions plus 480 interrupts: the largest
/// Armv8-M vector table.
const MAX_VECTOR_WORDS: usize = 496;
/// Criterion 2: shortest run of printable characters that counts as a string.
const MIN_STRING_LEN: usize = 6;
/// Criterion 3: how much of the payload is swept for BLs.
const CODE_WINDOW: usize = 64 * 1024;

/// Hits for one candidate, split by the alias the matching address used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AliasHits {
    /// Address was `base + offset` (the Non-secure alias, = flash offset).
    pub ns: u32,
    /// Address was `(base | SECURE_ALIAS) + offset`.
    pub s: u32,
}

/// Evidence that an image's slot starts at flash offset `base`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseScore {
    pub base: u32,
    /// Criterion 1: vector-table entries that land in the slot and decode.
    pub vectors: AliasHits,
    /// Criterion 2: literal words equal to the address of a string start.
    pub strings: AliasHits,
    /// How many different strings those hits name. Not scored: it shows
    /// whether the hits are many references or one constant repeated.
    pub distinct_strings: u32,
    /// Criterion 3: BLs in the first 64 KiB of code that call a PUSH.
    pub bl_prologues: u32,
}

/// Candidate slot bases `start, start + step, …` below `end`, as flash offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidates {
    pub start: u32,
    pub end: u32,
    pub step: u32,
}

/// Outcome of the stop rule applied to a set of scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    pub winner: BaseScore,
    pub runner_up: Option<BaseScore>,
    /// The most different strings any other candidate names (corroboration
    /// only; the rule uses hit counts).
    pub best_other_distinct: u32,
    pub accepted: bool,
}

impl AliasHits {
    pub fn total(self) -> u32 {
        self.ns + self.s
    }

    fn add(&mut self, secure: bool) {
        if secure {
            self.s += 1;
        } else {
            self.ns += 1;
        }
    }
}

impl BaseScore {
    pub fn total(&self) -> u32 {
        VECTOR_POINTS * self.vectors.total() + self.strings.total() + self.bl_prologues
    }
}

impl Candidates {
    /// Slot bases from 0 to 8 MiB in 4 KiB (flash sector) steps.
    pub const WIDE: Self = Self {
        start: 0,
        end: 0x80_0000,
        step: 0x1000,
    };

    /// Number of bases. Bases at or above [`SECURE_ALIAS`] are not flash
    /// offsets and are left out; a zero step yields none.
    fn count(self) -> usize {
        let end = self.end.min(SECURE_ALIAS);
        if self.step == 0 || self.start >= end {
            return 0;
        }
        ((end - self.start - 1) / self.step) as usize + 1
    }

    fn base(self, k: usize) -> u32 {
        self.start + k as u32 * self.step
    }

    /// Indexes of the bases in `lo..=hi`.
    fn indexes_in(self, lo: i64, hi: i64) -> Range<usize> {
        let n = self.count() as i64;
        if n == 0 || hi < lo {
            return 0..0;
        }
        let (start, step) = (i64::from(self.start), i64::from(self.step));
        let first = ((lo - start).max(0) + step - 1) / step;
        let last = hi - start;
        if last < 0 {
            return 0..0;
        }
        let end = (last / step + 1).min(n);
        first.min(end) as usize..end as usize
    }
}

impl Verdict {
    /// Winner's string-literal hits over the runner-up's; `None` when the
    /// runner-up has none (an unbounded margin) or there is no runner-up.
    pub fn margin(&self) -> Option<f64> {
        let theirs = self.runner_up?.strings.total();
        (theirs > 0).then(|| f64::from(self.winner.strings.total()) / f64::from(theirs))
    }
}

/// Score every candidate base for the image in `slot` (header + payload; the
/// payload starts at `hdr_size`). `vectors` enables criterion 1; turn it off
/// for an image whose words at `hdr_size` are not a vector table.
///
/// Returns one score per candidate, best total first (ties: lower base first).
pub fn score_bases(
    slot: &[u8],
    hdr_size: usize,
    candidates: Candidates,
    vectors: bool,
) -> Vec<BaseScore> {
    let bl_prologues = bl_prologues(slot, hdr_size);
    let mut scores: Vec<BaseScore> = (0..candidates.count())
        .map(|k| BaseScore {
            base: candidates.base(k),
            vectors: AliasHits::default(),
            strings: AliasHits::default(),
            distinct_strings: 0,
            bl_prologues,
        })
        .collect();
    if vectors {
        score_vectors(slot, hdr_size, candidates, &mut scores);
    }
    score_strings(slot, hdr_size, candidates, &mut scores);
    scores.sort_by(|a, b| b.total().cmp(&a.total()).then(a.base.cmp(&b.base)));
    scores
}

/// The stop rule. The winner is the best total. Criterion 3 is the same for
/// every base and criterion 1 is flat across every base whose slot covers
/// all the handlers, so a ratio of totals measures those constants, not the
/// evidence. The winner is accepted when:
/// - it explains as many vector-table entries as any candidate, and
/// - its string-literal hits are non-zero and at least twice those of the
///   best other candidate (the runner-up, chosen by string hits).
///
/// `None` when there are no candidates.
pub fn verdict(scores: &[BaseScore]) -> Option<Verdict> {
    let winner = *scores
        .iter()
        .max_by(|a, b| a.total().cmp(&b.total()).then(b.base.cmp(&a.base)))?;
    let runner_up = scores
        .iter()
        .filter(|s| s.base != winner.base)
        .max_by(|a, b| {
            a.strings
                .total()
                .cmp(&b.strings.total())
                .then(b.base.cmp(&a.base))
        })
        .copied();
    let best_other_distinct = scores
        .iter()
        .filter(|s| s.base != winner.base)
        .map(|s| s.distinct_strings)
        .max()
        .unwrap_or(0);
    let most_vectors = scores.iter().map(|s| s.vectors.total()).max();
    let hits = u64::from(winner.strings.total());
    let accepted = hits > 0
        && Some(winner.vectors.total()) == most_vectors
        && runner_up.is_none_or(|r| hits >= 2 * u64::from(r.strings.total()));
    Some(Verdict {
        winner,
        runner_up,
        best_other_distinct,
        accepted,
    })
}

fn half_at(slot: &[u8], off: usize) -> Option<u16> {
    let b = slot.get(off..off.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

/// The little-endian word at slot offset `off`, if the slot holds all of it.
pub fn word_at(slot: &[u8], off: usize) -> Option<u32> {
    let b = slot.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// The instruction at slot offset `off` and its length in bytes, or `None`
/// if it runs past the end of the slot.
pub fn decode_at(slot: &[u8], off: usize) -> Option<(Instr, u8)> {
    let hw1 = half_at(slot, off)?;
    if is_32bit(hw1) {
        Some((decode32(hw1, half_at(slot, off.checked_add(2)?)?), 4))
    } else {
        Some((decode16(hw1), 2))
    }
}

/// The vector table's entries after the initial SP: the run of words at
/// `hdr_size + 4` onwards that are zero or odd (Thumb addresses), ending at
/// the first even non-zero word or the end of the slot.
pub fn vector_entries(slot: &[u8], hdr_size: usize) -> Vec<u32> {
    (1..MAX_VECTOR_WORDS)
        .map_while(|i| word_at(slot, hdr_size.checked_add(4 * i)?))
        .take_while(|w| w & 1 == 1 || *w == 0)
        .collect()
}

/// Criterion 1.
fn score_vectors(slot: &[u8], hdr_size: usize, cands: Candidates, scores: &mut [BaseScore]) {
    let len = slot.len() as i64;
    for entry in vector_entries(slot, hdr_size) {
        if entry == 0 {
            continue;
        }
        let target = entry & !1;
        for secure in [false, true] {
            let alias = if secure { SECURE_ALIAS } else { 0 };
            let Some(flash) = target.checked_sub(alias) else {
                continue;
            };
            let flash = i64::from(flash);
            // Bases whose slot holds `flash`: base <= flash < base + len.
            for k in cands.indexes_in(flash - len + 1, flash) {
                let off = (flash - i64::from(scores[k].base)) as usize;
                if decode_at(slot, off).is_some_and(|(i, _)| !matches!(i, Instr::Undefined { .. }))
                {
                    scores[k].vectors.add(secure);
                }
            }
        }
    }
}

fn is_printable(b: u8) -> bool {
    (0x20..=0x7E).contains(&b) || matches!(b, b'\t' | b'\n' | b'\r')
}

/// Slot offsets of the payload's strings: maximal runs of at least
/// [`MIN_STRING_LEN`] printable characters ended by a NUL.
pub fn string_starts(slot: &[u8], hdr_size: usize) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut run = None;
    for (i, &b) in slot.iter().enumerate().skip(hdr_size) {
        if is_printable(b) {
            run.get_or_insert(i);
            continue;
        }
        if let Some(start) = run.take()
            && b == 0
            && i - start >= MIN_STRING_LEN
        {
            starts.push(start);
        }
    }
    starts
}

/// Criterion 2. A literal `x` (under either alias) names a string at slot
/// offset `o` for the base `x - o`; that is a candidate only when
/// `x - o - start` is a multiple of `step`, so strings are bucketed by their
/// offset modulo `step` and each literal only visits its own bucket.
fn score_strings(slot: &[u8], hdr_size: usize, cands: Candidates, scores: &mut [BaseScore]) {
    let n = scores.len() as i64;
    if n == 0 {
        return;
    }
    let (start, step) = (i64::from(cands.start), i64::from(cands.step));
    let mut buckets: HashMap<i64, Vec<i64>> = HashMap::new();
    for o in string_starts(slot, hdr_size) {
        let o = o as i64;
        buckets.entry(o % step).or_default().push(o);
    }
    // `scores` is still in candidate order here.
    let mut named = HashSet::new();
    let first = hdr_size.next_multiple_of(4);
    for off in (first..slot.len()).step_by(4) {
        let Some(word) = word_at(slot, off) else {
            break;
        };
        for secure in [false, true] {
            let alias = if secure { SECURE_ALIAS } else { 0 };
            let Some(x) = word.checked_sub(alias) else {
                continue;
            };
            let rel = i64::from(x) - start;
            let Some(bucket) = buckets.get(&rel.rem_euclid(step)) else {
                continue;
            };
            for &o in bucket {
                let k = (rel - o) / step;
                if rel >= o && k < n {
                    let score = &mut scores[k as usize];
                    score.strings.add(secure);
                    if named.insert((k, o)) {
                        score.distinct_strings += 1;
                    }
                }
            }
        }
    }
}

/// Criterion 3: a linear sweep of the first [`CODE_WINDOW`] bytes of payload.
fn bl_prologues(slot: &[u8], hdr_size: usize) -> u32 {
    let payload = hdr_size as i64..slot.len() as i64;
    let end = slot.len().min(hdr_size.saturating_add(CODE_WINDOW));
    let mut count = 0;
    let mut off = hdr_size;
    while off < end {
        let Some((instr, len)) = decode_at(slot, off) else {
            break;
        };
        if let Instr::Bl { imm } = instr {
            let target = off as i64 + 4 + i64::from(imm);
            if payload.contains(&target)
                && matches!(
                    decode_at(slot, target as usize),
                    Some((Instr::Push { .. }, _))
                )
            {
                count += 1;
            }
        }
        off += usize::from(len);
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDE: Candidates = Candidates::WIDE;

    /// A zero-filled slot written little-endian at slot offsets.
    struct Slot(Vec<u8>);

    impl Slot {
        fn new(len: usize) -> Self {
            Self(vec![0; len])
        }
        fn u16(&mut self, off: usize, v: u16) {
            self.0[off..off + 2].copy_from_slice(&v.to_le_bytes());
        }
        fn u32(&mut self, off: usize, v: u32) {
            self.0[off..off + 4].copy_from_slice(&v.to_le_bytes());
        }
        fn words(&mut self, off: usize, vs: &[u32]) {
            for (i, &v) in vs.iter().enumerate() {
                self.u32(off + 4 * i, v);
            }
        }
        /// `s` followed by a NUL.
        fn cstr(&mut self, off: usize, s: &str) {
            self.0[off..off + s.len()].copy_from_slice(s.as_bytes());
            self.0[off + s.len()] = 0;
        }
        /// `BL` at slot offset `from` calling slot offset `to`.
        fn bl(&mut self, from: usize, to: usize) {
            let off = (to as i64 - (from as i64 + 4)) as u32;
            let s = off >> 24 & 1;
            let j1 = !(off >> 23 ^ s) & 1;
            let j2 = !(off >> 22 ^ s) & 1;
            let hw1 = 0xF000 | s << 10 | (off >> 12 & 0x3FF);
            let hw2 = 0xD000 | j1 << 13 | j2 << 11 | (off >> 1 & 0x7FF);
            self.u16(from, hw1 as u16);
            self.u16(from + 2, hw2 as u16);
        }
    }

    const PUSH_R4_LR: u16 = 0xB510;
    const BX_LR: u16 = 0x4770;
    const B_SELF: u16 = 0xE7FE;
    const UDF: u16 = 0xDE00;
    const NOP: u16 = 0xBF00;
    /// `push.w {r4-r11, lr}` (STMDB sp!).
    const PUSH_W: [u16; 2] = [0xE92D, 0x4FF0];
    /// Two `bx lr`: an even word, which ends a vector table.
    const NOT_A_VECTOR: u32 = 0x4770_4770;

    /// The brief's synthetic image: linked at 0x1000_0000, i.e. the Secure
    /// alias of flash offset 0. Header 0x400, vector table at slot+0x400.
    fn image_linked_at_0x1000_0000() -> Vec<u8> {
        const LINK: u32 = 0x1000_0000;
        let mut s = Slot::new(0x4000);
        s.words(
            0x400,
            &[
                0x2000_1000,            // SP
                (LINK + 0x800) | 1,     // reset -> push: hit
                (LINK + 0x840) | 1,     // NMI -> bx lr: hit
                (LINK + 0x880) | 1,     // HardFault -> b .: hit
                (LINK + 0x8C0) | 1,     // -> udf: decodes as Undefined, no hit
                (LINK + 0x90_0000) | 1, // outside every candidate's slot: no hit
            ],
        );
        // Entries 6..15 stay zero (reserved). Entry 16 is even: the table
        // ends there, so the odd word after it is not a vector.
        s.u32(0x440, NOT_A_VECTOR);
        s.u32(0x444, (LINK + 0x800) | 1);

        s.u16(0x800, PUSH_R4_LR);
        s.u16(0x840, BX_LR);
        s.u16(0x880, B_SELF);
        s.u16(0x8C0, UDF);

        // Criterion 3: two of these four calls reach a PUSH.
        s.bl(0x1000, 0x800); // push {r4, lr}: hit
        s.bl(0x1004, 0x840); // bx lr: no
        s.bl(0x1008, 0x5000); // past the end of the slot: no
        s.bl(0x100C, 0x1100); // push.w: hit
        s.u16(0x1100, PUSH_W[0]);
        s.u16(0x1102, PUSH_W[1]);

        s.cstr(0x2000, "hello, world");
        s.cstr(0x2010, "evo strings!");
        s.cstr(0x2020, "abcdef"); // exactly 6 characters: a string
        s.cstr(0x2030, "abcde"); // 5: too short
        s.cstr(0x2040, "unterminated");
        s.0[0x2040 + 12] = 0xFF;

        s.words(
            0x3000,
            &[
                LINK + 0x2000, // hit
                LINK + 0x2000, // hit (every literal counts)
                LINK + 0x2010, // hit
                LINK + 0x2020, // hit
                LINK + 0x2030, // too short: no
                LINK + 0x2004, // middle of a string: no
                LINK + 0x2040, // not NUL-terminated: no
            ],
        );
        // Literal pools are word-aligned; a misaligned match does not count.
        s.u32(0x3022, LINK + 0x2010);
        // Decoys: two pointers that fit base 0x2_0000 through its NS alias.
        s.words(0x3040, &[0x2_0000 + 0x2000, 0x2_0000 + 0x2010]);
        s.0
    }

    #[test]
    fn score_picks_known_base() {
        let slot = image_linked_at_0x1000_0000();
        let scores = score_bases(&slot, 0x400, WIDE, true);
        assert_eq!(scores.len(), 0x800, "one score per candidate");

        assert_eq!(
            scores[0],
            BaseScore {
                base: 0,
                vectors: AliasHits { ns: 0, s: 3 },
                strings: AliasHits { ns: 0, s: 4 },
                distinct_strings: 3, // "hello, world" is named twice
                bl_prologues: 2,
            }
        );
        assert_eq!(scores[0].total(), 3 * 10 + 4 + 2);
        assert_eq!(
            scores[1],
            BaseScore {
                base: 0x2_0000,
                vectors: AliasHits::default(),
                strings: AliasHits { ns: 2, s: 0 },
                distinct_strings: 2,
                bl_prologues: 2,
            }
        );
        // BL is PC-relative, so criterion 3 cannot tell candidates apart.
        assert!(scores.iter().all(|s| s.bl_prologues == 2));
        assert!(scores[2..].iter().all(|s| s.total() == 2));

        // An image scored without its vector table (as for image 1).
        let no_vectors = score_bases(&slot, 0x400, WIDE, false);
        assert_eq!(no_vectors[0].base, 0);
        assert_eq!(no_vectors[0].vectors, AliasHits::default());
        assert_eq!(no_vectors[0].strings, AliasHits { ns: 0, s: 4 });
    }

    #[test]
    fn score_counts_both_aliases() {
        const BASE: u32 = 0x4_2000;
        const S: u32 = BASE | SECURE_ALIAS;
        let mut s = Slot::new(0x2000);
        s.words(
            0x200,
            &[
                0x2000_8000,
                (S + 0x400) | 1,
                (BASE + 0x440) | 1,
                (S + 0x480) | 1,
            ],
        );
        s.u32(0x210, NOT_A_VECTOR);
        s.u16(0x400, PUSH_R4_LR);
        s.u16(0x440, BX_LR);
        s.u16(0x480, NOP);
        s.cstr(0x1000, "secure side");
        s.cstr(0x1010, "non-secure side");
        s.cstr(0x1020, "both aliases");
        s.words(
            0x1800,
            &[
                BASE + 0x1000,
                BASE + 0x1010,
                BASE + 0x1010,
                BASE + 0x1020,
                S + 0x1000,
                S + 0x1020,
                S + 0x1020,
            ],
        );

        let scores = score_bases(&s.0, 0x200, WIDE, true);
        assert_eq!(
            scores[0],
            BaseScore {
                base: BASE,
                vectors: AliasHits { ns: 1, s: 2 },
                strings: AliasHits { ns: 4, s: 3 },
                // A string named through both aliases is one string.
                distinct_strings: 3,
                bl_prologues: 0,
            }
        );
        assert_eq!(scores[0].total(), 3 * 10 + 7);
        // The base one step lower holds every handler address too (they land
        // on zero padding, `movs r0, r0`): criterion 1 is flat between
        // neighbours and only the strings separate them.
        assert_eq!(
            scores[1],
            BaseScore {
                base: BASE - 0x1000,
                vectors: AliasHits { ns: 1, s: 2 },
                strings: AliasHits::default(),
                distinct_strings: 0,
                bl_prologues: 0,
            }
        );
        assert!(scores[2..].iter().all(|s| s.total() == 0));
    }

    #[test]
    fn score_tolerates_degenerate_input() {
        let empty = score_bases(&[], 0x400, WIDE, true);
        assert_eq!(empty.len(), 0x800);
        assert!(empty.iter().all(|s| s.total() == 0));

        let none = |c: Candidates| score_bases(&[0; 0x800], 0x400, c, true);
        assert!(none(Candidates { step: 0, ..WIDE }).is_empty());
        assert!(
            none(Candidates {
                start: 0x2000,
                end: 0x1000,
                step: 0x1000
            })
            .is_empty()
        );
        // Candidates are flash offsets: nothing at or above the alias bit.
        let top = none(Candidates {
            start: 0x0FFF_E000,
            end: u32::MAX,
            step: 0x1000,
        });
        let bases: Vec<u32> = top.iter().map(|s| s.base).collect();
        assert_eq!(bases, [0x0FFF_E000, 0x0FFF_F000]);
        assert!(score_bases(&[0; 8], 0, WIDE, true).len() == 0x800);
        assert_eq!(
            score_bases(
                &[0; 8],
                0,
                Candidates {
                    step: u32::MAX,
                    ..WIDE
                },
                true
            )
            .len(),
            1
        );

        // A vector table cut short whose one entry points at a 32-bit prefix
        // (a BL's first half) in the last halfword: no panic, nothing counted.
        let mut s = Slot::new(0x40A);
        s.u32(0x400, 0x2000_0000);
        s.u16(0x404, 0x0409);
        s.u16(0x408, 0xF000);
        assert!(
            score_bases(&s.0, 0x400, WIDE, true)
                .iter()
                .all(|s| s.total() == 0)
        );
        // A header larger than the slot.
        assert!(
            score_bases(&s.0, 0x1000, WIDE, true)
                .iter()
                .all(|s| s.total() == 0)
        );
    }

    fn score(base: u32, vectors: u32, strings: u32) -> BaseScore {
        BaseScore {
            base,
            vectors: AliasHits { ns: 0, s: vectors },
            strings: AliasHits {
                ns: strings / 2,
                s: strings - strings / 2,
            },
            distinct_strings: strings.min(3),
            bl_prologues: 7,
        }
    }

    #[test]
    fn verdict_needs_twice_the_runner_ups_string_hits() {
        // Totals are dominated by the vector plateau and the constant BL
        // count, so the 2x rule is applied to the string-literal hits.
        let mut scores = [
            score(0x1000, 40, 5),
            score(0xB_0000, 40, 10),
            score(0x2000, 0, 3),
        ];
        let v = verdict(&scores).expect("candidates");
        assert_eq!(v.winner.base, 0xB_0000);
        assert_eq!(v.runner_up.map(|r| r.base), Some(0x1000));
        assert!(v.accepted, "exactly 2x is enough");
        // Corroboration, not part of the rule: the most strings any other
        // candidate names.
        assert_eq!(v.best_other_distinct, 3);
        scores[2].distinct_strings = 0;
        scores[0].distinct_strings = 1;
        assert_eq!(verdict(&scores).expect("candidates").best_other_distinct, 1);

        let close = [score(0xB_0000, 40, 10), score(0x1000, 40, 6)];
        assert!(!verdict(&close).expect("candidates").accepted);

        // No string evidence at all.
        let blank = [score(0, 0, 0), score(0x1000, 0, 0)];
        assert!(!verdict(&blank).expect("candidates").accepted);

        // The top total misses part of the vector table that another
        // candidate explains: inconsistent, however strong its strings.
        let torn = [score(0x5000, 39, 100), score(0x1000, 40, 10)];
        let v = verdict(&torn).expect("candidates");
        assert_eq!(v.winner.base, 0x5000);
        assert!(!v.accepted);

        // A single candidate with evidence has no runner-up to beat.
        let alone = verdict(&[score(0x3000, 0, 4)]).expect("candidates");
        assert_eq!(alone.runner_up, None);
        assert_eq!(alone.best_other_distinct, 0);
        assert!(alone.accepted);

        assert_eq!(verdict(&[]), None);
    }

    /// Reads the user's own OS bundle from `EVO_OS_IMAGE`; skipped when unset.
    /// `Layout::EVO_7_1` must stay what the scorer finds, by the stop rule.
    #[test]
    fn real_bundle_scores_reproduce_evo_7_1() {
        let Some(path) = std::env::var_os("EVO_OS_IMAGE") else {
            eprintln!("skipping real_bundle_scores_reproduce_evo_7_1: EVO_OS_IMAGE is not set");
            return;
        };
        let bundle =
            evo_image::Bundle::open(std::path::Path::new(&path)).expect("open real bundle");
        let pkg = &bundle.package;
        let layout = evo_image::Layout::EVO_7_1;
        for (index, want) in [(0, layout.secure_slot_base), (1, layout.ns_slot_base)] {
            let img = &pkg.images[index];
            let slot = &pkg.bytes[img.pkg_offset..img.body.end];
            let hdr = usize::from(img.header_size);
            let scores = score_bases(slot, hdr, Candidates::WIDE, index == 0);
            let v = verdict(&scores).expect("candidates");
            assert_eq!(v.winner.base, want, "image {index}: {v:x?}");
            assert!(v.accepted, "image {index}: {v:x?}");
            assert!(
                v.winner.distinct_strings >= 2 * v.best_other_distinct,
                "image {index}: {v:x?}"
            );
        }
    }
}
