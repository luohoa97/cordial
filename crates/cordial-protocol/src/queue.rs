//! The bounded event queue a runtime puts between its engine threads and the
//! socket (spec section 2).
//!
//! **A launcher that stops reading must never stall the engine.** Events are
//! produced on engine threads; if the socket's send buffer fills, a queue that
//! blocks would block the engine with it. This one never blocks a producer: at
//! capacity the *newest* event is dropped and counted, and the count goes out
//! as `events.dropped {count}` ahead of the next event that does get through,
//! so the launcher learns it missed something instead of seeing a clean stream.
//!
//! The counter `n` is assigned when an event leaves the queue, not when it
//! enters, so a dropped event consumes no number and the launcher's `n` runs
//! without gaps. The loss is reported by `events.dropped` and not inferred.

use crate::frame::Event;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// How many events wait before the newest starts being dropped.
pub const CAPACITY: usize = 256;

/// The event that reports drops.
pub const EVENTS_DROPPED: &str = "events.dropped";

struct Inner {
    queue: VecDeque<(String, Value)>,
    dropped: u64,
    next_n: u64,
}

/// A multi-producer queue with a drop-and-count overflow rule. Share it behind
/// an `Arc`; every method takes `&self`.
pub struct EventQueue {
    capacity: usize,
    inner: Mutex<Inner>,
    ready: Condvar,
}

impl Default for EventQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl EventQueue {
    pub fn new() -> Self {
        Self::with_capacity(CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        EventQueue {
            capacity,
            inner: Mutex::new(Inner { queue: VecDeque::new(), dropped: 0, next_n: 1 }),
            ready: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A producer that panicked must not take the engine's event path with
        // it; the queue's state is plain data and is valid after any panic.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queue an event. Never blocks. `false` means the queue was full and this
    /// event was dropped and counted.
    pub fn push(&self, ev: impl Into<String>, p: Value) -> bool {
        let mut inner = self.lock();
        if inner.queue.len() >= self.capacity {
            inner.dropped += 1;
            // A drop is still something to report, so wake a consumer that is
            // waiting only because the queue looked empty to it.
            self.ready.notify_one();
            return false;
        }
        inner.queue.push_back((ev.into(), p));
        self.ready.notify_one();
        true
    }

    /// The next event to send, with its `n` assigned. Drops that have not been
    /// reported yet come out first, as one `events.dropped`.
    pub fn pop(&self) -> Option<Event> {
        Self::take(&mut self.lock())
    }

    /// Like [`pop`](Self::pop), waiting up to `timeout` for something to send.
    pub fn pop_timeout(&self, timeout: Duration) -> Option<Event> {
        let mut inner = self.lock();
        if inner.queue.is_empty() && inner.dropped == 0 {
            inner = self.ready.wait_timeout(inner, timeout).unwrap_or_else(|e| e.into_inner()).0;
        }
        Self::take(&mut inner)
    }

    fn take(inner: &mut Inner) -> Option<Event> {
        let (ev, p) = if inner.dropped > 0 {
            let count = std::mem::take(&mut inner.dropped);
            (EVENTS_DROPPED.to_string(), json!({ "count": count }))
        } else {
            inner.queue.pop_front()?
        };
        let n = inner.next_n;
        inner.next_n += 1;
        Some(Event { ev, n, p })
    }

    /// Events waiting, not counting an unreported drop.
    pub fn len(&self) -> usize {
        self.lock().queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn events_leave_in_order_with_a_gapless_counter() {
        let q = EventQueue::new();
        for i in 0..3 {
            assert!(q.push("game.left", json!({ "at": i })));
        }
        let got: Vec<_> = std::iter::from_fn(|| q.pop()).collect();
        assert_eq!(got.iter().map(|e| e.n).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(got[2].p, json!({ "at": 2 }));
    }

    #[test]
    fn at_capacity_the_newest_is_dropped_counted_and_reported_first() {
        let q = EventQueue::with_capacity(4);
        for i in 0..4 {
            assert!(q.push("e", json!(i)));
        }
        for i in 4..9 {
            assert!(!q.push("e", json!(i)), "full: the newest is the one lost");
        }
        let first = q.pop().unwrap();
        assert_eq!((first.ev.as_str(), first.p), (EVENTS_DROPPED, json!({ "count": 5 })));
        // What survived is the oldest four, untouched.
        let rest: Vec<_> = std::iter::from_fn(|| q.pop()).map(|e| e.p).collect();
        assert_eq!(rest, vec![json!(0), json!(1), json!(2), json!(3)]);
        // Reported once, not every time.
        assert!(q.pop().is_none());
    }

    #[test]
    fn the_default_capacity_is_256() {
        let q = EventQueue::new();
        let accepted = (0..300).filter(|i| q.push("e", json!(i))).count();
        assert_eq!(accepted, 256);
        assert_eq!(q.pop().unwrap().p, json!({ "count": 44 }));
    }

    #[test]
    fn a_producer_never_blocks_when_nobody_consumes() {
        let q = Arc::new(EventQueue::with_capacity(8));
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let q = q.clone();
                std::thread::spawn(move || (0..10_000).filter(|i| q.push("e", json!([t, i]))).count())
            })
            .collect();
        let accepted: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(accepted, 8);
        assert_eq!(q.pop().unwrap().p, json!({ "count": 40_000 - 8 }));
    }

    #[test]
    fn pop_timeout_wakes_for_a_push_and_times_out_when_idle() {
        let q = Arc::new(EventQueue::new());
        assert!(q.pop_timeout(Duration::from_millis(10)).is_none());
        let producer = q.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            producer.push("engine.version", json!({ "version": "0.1" }));
        });
        let got = q.pop_timeout(Duration::from_secs(5)).expect("woken by the push");
        assert_eq!(got.ev, "engine.version");
        t.join().unwrap();
    }
}
