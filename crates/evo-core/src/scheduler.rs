//! Cycle-keyed event queue.
//!
//! Peripherals schedule their own events here instead of being polled on every
//! instruction. The machine advances [`Scheduler::now`] by the cycles each CPU
//! step consumed, then drains [`Scheduler::pop_due`] until it returns `None`.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use serde::{Deserialize, Serialize};

/// Identifies a schedulable event source. At most one event per kind is live.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum EventKind {
    /// A peripheral event; ids are assigned stably per peripheral in Plan 2.
    Peripheral(u16),
}

/// Min-heap of pending events keyed by `(cycle, insertion sequence)`.
#[derive(Clone, Debug, Default)]
pub struct Scheduler {
    /// Current time in cycles. Advanced by the machine, never by the scheduler.
    pub now: u64,
    heap: BinaryHeap<Reverse<(u64, u64, EventKind)>>,
    live: HashMap<EventKind, u64>,
    next_seq: u64,
    /// No event is due before this cycle: the top of the heap, kept by `settle` (0, "look",
    /// until the first mutation). Lets `pop_due` answer without touching the heap.
    due_hint: u64,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedules `kind` to fire at cycle `at`, replacing any pending event of
    /// the same kind. A replacement takes a fresh sequence number, so it fires
    /// after events already queued for the same cycle.
    pub fn schedule(&mut self, at: u64, kind: EventKind) {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        // The previous entry for `kind`, if any, stays in the heap but is no
        // longer the live generation, so it is skipped when it surfaces.
        self.live.insert(kind, seq);
        self.heap.push(Reverse((at, seq, kind)));
        self.settle();
    }

    /// Removes the pending event of `kind`, if any.
    pub fn cancel(&mut self, kind: EventKind) {
        if self.live.remove(&kind).is_some() {
            self.settle();
        }
    }

    /// Cycle of the earliest pending event.
    pub fn next_deadline(&self) -> Option<u64> {
        // `settle` keeps a live entry on top, so no stale entry can be returned.
        self.heap.peek().map(|Reverse((at, _, _))| *at)
    }

    /// Removes and returns the earliest event with `at <= now`.
    pub fn pop_due(&mut self) -> Option<EventKind> {
        if self.now < self.due_hint {
            return None;
        }
        let &Reverse((at, _, kind)) = self.heap.peek()?;
        if at > self.now {
            return None;
        }
        self.heap.pop();
        self.live.remove(&kind);
        self.settle();
        Some(kind)
    }

    fn is_live(live: &HashMap<EventKind, u64>, seq: u64, kind: EventKind) -> bool {
        live.get(&kind) == Some(&seq)
    }

    /// Restores the heap invariants after a mutation: cancelled and replaced
    /// entries are dropped from the top (so the top is always live), and the
    /// heap is rebuilt if stale entries outnumber live ones, which bounds its
    /// size for a peripheral that reschedules itself far more often than it fires.
    fn settle(&mut self) {
        while let Some(&Reverse((_, seq, kind))) = self.heap.peek() {
            if Self::is_live(&self.live, seq, kind) {
                break;
            }
            self.heap.pop();
        }
        if self.heap.len() > 2 * self.live.len() + 32 {
            let live = &self.live;
            self.heap
                .retain(|&Reverse((_, seq, kind))| Self::is_live(live, seq, kind));
        }
        self.due_hint = self.heap.peek().map_or(u64::MAX, |Reverse((at, _, _))| *at);
    }
}

/// Serialized form of a [`Scheduler`]: only live events, so lazily cancelled
/// heap entries never reach a save state.
#[derive(Serialize, Deserialize)]
struct SchedulerState {
    now: u64,
    next_seq: u64,
    events: Vec<PendingEvent>,
}

#[derive(Serialize, Deserialize)]
struct PendingEvent {
    at: u64,
    seq: u64,
    kind: EventKind,
}

impl From<&Scheduler> for SchedulerState {
    fn from(s: &Scheduler) -> Self {
        let mut events: Vec<PendingEvent> = s
            .heap
            .iter()
            .filter(|Reverse((_, seq, kind))| Scheduler::is_live(&s.live, *seq, *kind))
            .map(|&Reverse((at, seq, kind))| PendingEvent { at, seq, kind })
            .collect();
        // Heap iteration order is unspecified; sort so equal schedulers
        // serialize identically.
        events.sort_by_key(|e| (e.at, e.seq));
        SchedulerState {
            now: s.now,
            next_seq: s.next_seq,
            events,
        }
    }
}

impl From<SchedulerState> for Scheduler {
    /// Rebuilds a scheduler from saved state. Input is not trusted: if a kind
    /// appears more than once the newest (highest `seq`) entry wins, and
    /// `next_seq` is raised past every restored `seq` so new events always
    /// queue behind restored ones.
    fn from(state: SchedulerState) -> Self {
        let mut newest: HashMap<EventKind, (u64, u64)> = HashMap::new();
        let mut next_seq = state.next_seq;
        for e in state.events {
            next_seq = next_seq.max(e.seq.saturating_add(1));
            match newest.get(&e.kind) {
                Some(&(_, seq)) if seq >= e.seq => {}
                _ => {
                    newest.insert(e.kind, (e.at, e.seq));
                }
            }
        }
        let live = newest
            .iter()
            .map(|(&kind, &(_, seq))| (kind, seq))
            .collect();
        let heap = newest
            .into_iter()
            .map(|(kind, (at, seq))| Reverse((at, seq, kind)))
            .collect();
        Scheduler {
            now: state.now,
            heap,
            live,
            next_seq,
            // Look at the heap on the first `pop_due`.
            due_hint: 0,
        }
    }
}

impl Serialize for Scheduler {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        SchedulerState::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Scheduler {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        SchedulerState::deserialize(deserializer).map(Scheduler::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: EventKind = EventKind::Peripheral(1);
    const B: EventKind = EventKind::Peripheral(2);
    const C: EventKind = EventKind::Peripheral(3);
    const D: EventKind = EventKind::Peripheral(4);

    /// Pops everything due at the scheduler's current time.
    fn drain(s: &mut Scheduler) -> Vec<EventKind> {
        std::iter::from_fn(|| s.pop_due()).collect()
    }

    #[test]
    fn events_fire_in_order() {
        let mut s = Scheduler::new();
        s.schedule(30, C);
        s.schedule(10, A);
        s.schedule(20, B);
        assert_eq!(s.next_deadline(), Some(10));
        s.now = 100;
        assert_eq!(drain(&mut s), vec![A, B, C]);
        assert_eq!(s.next_deadline(), None);
    }

    #[test]
    fn pop_due_waits_for_now() {
        let mut s = Scheduler::new();
        s.schedule(10, A);
        s.schedule(20, B);
        s.now = 9;
        assert_eq!(s.pop_due(), None);
        s.now = 10;
        assert_eq!(s.pop_due(), Some(A));
        assert_eq!(s.pop_due(), None);
        s.now = 25;
        assert_eq!(s.pop_due(), Some(B));
        assert_eq!(s.pop_due(), None);
    }

    #[test]
    fn past_deadline_fires_on_next_pop() {
        let mut s = Scheduler::new();
        s.now = 50;
        s.schedule(10, A);
        assert_eq!(s.pop_due(), Some(A));
    }

    #[test]
    fn same_time_fifo() {
        let mut s = Scheduler::new();
        s.schedule(10, C);
        s.schedule(10, A);
        s.schedule(10, B);
        s.now = 10;
        assert_eq!(drain(&mut s), vec![C, A, B]);
    }

    #[test]
    fn cancel_removes() {
        let mut s = Scheduler::new();
        s.schedule(10, A);
        s.schedule(20, B);
        s.cancel(A);
        assert_eq!(s.next_deadline(), Some(20));
        s.now = 100;
        assert_eq!(drain(&mut s), vec![B]);

        s.schedule(5, A);
        s.cancel(A);
        assert_eq!(s.next_deadline(), None);
        assert_eq!(s.pop_due(), None);
    }

    #[test]
    fn cancel_unknown_kind_is_noop() {
        let mut s = Scheduler::new();
        s.cancel(A);
        s.schedule(10, B);
        s.cancel(A);
        assert_eq!(s.next_deadline(), Some(10));
    }

    #[test]
    fn reschedule_replaces() {
        let mut s = Scheduler::new();
        s.schedule(10, A);
        s.schedule(20, A);
        assert_eq!(s.next_deadline(), Some(20));
        s.now = 15;
        assert_eq!(s.pop_due(), None);
        s.now = 20;
        assert_eq!(drain(&mut s), vec![A]);

        // Rescheduling earlier replaces too, and the old deadline never fires.
        s.schedule(100, A);
        s.schedule(30, A);
        s.now = 1000;
        assert_eq!(drain(&mut s), vec![A]);
    }

    #[test]
    fn reschedule_goes_behind_events_already_at_that_cycle() {
        let mut s = Scheduler::new();
        s.schedule(10, A);
        s.schedule(10, B);
        s.schedule(10, A);
        s.now = 10;
        assert_eq!(drain(&mut s), vec![B, A]);
    }

    #[test]
    fn popped_kind_can_be_scheduled_again() {
        let mut s = Scheduler::new();
        s.schedule(10, A);
        s.now = 10;
        assert_eq!(s.pop_due(), Some(A));
        s.schedule(20, A);
        assert_eq!(s.next_deadline(), Some(20));
        s.now = 20;
        assert_eq!(s.pop_due(), Some(A));
    }

    #[test]
    fn stale_entries_do_not_accumulate() {
        let mut s = Scheduler::new();
        s.schedule(1, B);
        for at in 0..10_000 {
            s.schedule(at, A);
        }
        assert!(s.heap.len() < 100, "heap grew to {}", s.heap.len());
        s.now = 10_000;
        assert_eq!(drain(&mut s), vec![B, A]);
    }

    #[test]
    fn scheduler_roundtrips_through_serde() {
        let mut s = Scheduler::new();
        s.now = 7;
        s.schedule(10, A);
        s.schedule(10, B);
        s.schedule(50, C);
        s.schedule(40, C); // replaces the 50; the 50 entry is now stale
        s.cancel(B); // stale
        s.schedule(10, B); // live again, behind A at cycle 10
        // Cancelled and never rescheduled. Its entry is not at the heap top
        // (A is), so it stays in the heap as stale and only the live filter
        // in serialization keeps it from being resurrected on restore.
        s.schedule(60, D);
        s.cancel(D);
        assert!(
            s.heap.len() > s.live.len(),
            "setup must leave stale entries"
        );

        let json = serde_json::to_string(&s).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value["events"].as_array().unwrap().len(),
            s.live.len(),
            "serialized events are exactly the live ones: {json}"
        );
        let mut back: Scheduler = serde_json::from_str(&json).unwrap();

        assert_eq!(back.now, 7);
        assert_eq!(back.heap.len(), 3);
        assert_eq!(back.next_deadline(), Some(10));

        // New events after a restore still queue behind restored ones.
        s.schedule(40, A);
        back.schedule(40, A);
        s.now = 1000;
        back.now = 1000;
        let restored = drain(&mut back);
        assert!(!restored.contains(&D), "cancelled event was resurrected");
        assert_eq!(restored, drain(&mut s));
    }

    #[test]
    fn deserialize_tolerates_duplicate_kinds() {
        let json = r#"{"now":0,"next_seq":0,"events":[
            {"at":10,"seq":5,"kind":{"Peripheral":1}},
            {"at":20,"seq":3,"kind":{"Peripheral":1}}]}"#;
        let mut s: Scheduler = serde_json::from_str(json).unwrap();
        assert_eq!(s.next_deadline(), Some(10), "highest seq is the live one");
        s.now = 100;
        assert_eq!(drain(&mut s), vec![A]);
        s.schedule(1, B);
        s.schedule(1, C);
        assert_eq!(drain(&mut s), vec![B, C]);
    }
}
