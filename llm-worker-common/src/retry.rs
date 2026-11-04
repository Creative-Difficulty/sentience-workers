//! Per-message retry budget and global back-off.
//!
//! The workers poll "messages without an attempt row, oldest first". Without
//! bookkeeping, a message that always fails stays at the head of that queue
//! forever and every polling round pays for the same failing LLM call
//! (head-of-line stall). [`RetryTracker`] fixes this:
//!
//! * a failing message is put on a cool-down (exponential, capped), and the
//!   polling query skips ids that are cooling down, so the rest of the queue
//!   keeps moving;
//! * every failure that is attributable to the message is a *strike*; after
//!   `max_attempts` strikes the caller records the message as attempted (gives
//!   up) so it leaves the queue for good;
//! * systemic failures (provider down, rate limited, DB unreachable) do not
//!   say anything about the message, so they only count as a strike when the
//!   system was demonstrably healthy right before (the previous call
//!   succeeded or the LLM answered). A provider outage therefore never burns
//!   the backlog, while a message that reliably triggers e.g. an HTTP 500 is
//!   still given up eventually.
//!
//! State is in memory: a restart grants every message a fresh budget. A
//! persistent failure counter would need a schema change (see the workspace
//! notes); the in-memory budget still bounds the spend per process lifetime.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::failure::FailureKind;

/// Entries that have not been touched for this long are dropped (their
/// message left the queue some other way, e.g. it was deleted).
const FORGET_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Strikes after which a message is given up (>= 1).
    pub max_attempts: u32,
    /// Cool-down after the first failure; doubles with every further failure.
    pub base_delay: Duration,
    /// Upper bound of the cool-down.
    pub max_delay: Duration,
}

impl RetryPolicy {
    /// Cool-down after the `failures`-th failure (1-based).
    pub fn cooldown(&self, failures: u32) -> Duration {
        let exp = failures.saturating_sub(1).min(31);
        self.base_delay
            .checked_mul(1u32 << exp)
            .unwrap_or(self.max_delay)
            .min(self.max_delay)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Keep the message in the queue; it is skipped until `retry_at`.
    Retry { strikes: u32, retry_at: Instant },
    /// Budget exhausted: record the message as attempted.
    GiveUp { strikes: u32 },
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    strikes: u32,
    failures: u32,
    retry_at: Instant,
    last_failure: Instant,
}

#[derive(Debug)]
pub struct RetryTracker {
    policy: RetryPolicy,
    entries: HashMap<i64, Entry>,
    /// Did the most recent LLM interaction show a healthy system?
    healthy: bool,
}

impl RetryTracker {
    pub fn new(policy: RetryPolicy) -> Self {
        assert!(policy.max_attempts >= 1, "max_attempts must be >= 1");
        RetryTracker {
            policy,
            entries: HashMap::new(),
            // Unknown at start-up: do not blame the first message for an
            // outage that was already going on.
            healthy: false,
        }
    }

    pub fn policy(&self) -> RetryPolicy {
        self.policy
    }

    /// Ids that must be skipped by the next polling query (cooling down).
    pub fn cooling_down(&mut self, now: Instant) -> Vec<i64> {
        self.entries
            .retain(|_, e| now.saturating_duration_since(e.last_failure) < FORGET_AFTER);
        let mut ids: Vec<i64> = self
            .entries
            .iter()
            .filter(|(_, e)| e.retry_at > now)
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// The message was processed (or handed off) successfully.
    pub fn record_success(&mut self, id: i64) {
        self.entries.remove(&id);
        self.healthy = true;
    }

    /// The LLM answered (even if the answer was unusable): the system works.
    pub fn record_healthy(&mut self) {
        self.healthy = true;
    }

    pub fn record_failure(&mut self, id: i64, kind: FailureKind, now: Instant) -> Verdict {
        self.record_failures(&[id], kind, now)[0]
    }

    /// One failure shared by several messages (e.g. a whole LLM batch). The
    /// strike decision is taken once for the group, so every message of the
    /// batch is treated the same.
    pub fn record_failures(
        &mut self,
        ids: &[i64],
        kind: FailureKind,
        now: Instant,
    ) -> Vec<Verdict> {
        let counts = match kind {
            FailureKind::Message => true,
            FailureKind::Systemic => self.healthy,
        };
        match kind {
            FailureKind::Systemic => self.healthy = false,
            // the model answered (or the input itself was rejected)
            FailureKind::Message => {}
        }
        ids.iter()
            .map(|&id| {
                let e = self.entries.entry(id).or_insert(Entry {
                    strikes: 0,
                    failures: 0,
                    retry_at: now,
                    last_failure: now,
                });
                e.failures = e.failures.saturating_add(1);
                if counts {
                    e.strikes = e.strikes.saturating_add(1);
                }
                e.last_failure = now;
                e.retry_at = now + self.policy.cooldown(e.failures);
                if e.strikes >= self.policy.max_attempts {
                    let strikes = e.strikes;
                    self.entries.remove(&id);
                    Verdict::GiveUp { strikes }
                } else {
                    Verdict::Retry {
                        strikes: e.strikes,
                        retry_at: e.retry_at,
                    }
                }
            })
            .collect()
    }

    /// Number of tracked (currently failing) messages.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Global exponential back-off used after systemic failures, so a provider
/// outage is not hammered by every message of the batch.
#[derive(Debug, Clone)]
pub struct Backoff {
    base: Duration,
    max: Duration,
    current: Option<Duration>,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Backoff {
            base,
            max,
            current: None,
        }
    }

    /// Register a systemic failure; returns how long to pause.
    pub fn next_delay(&mut self) -> Duration {
        let next = match self.current {
            None => self.base,
            Some(d) => d.saturating_mul(2),
        }
        .min(self.max);
        self.current = Some(next);
        next
    }

    pub fn reset(&mut self) {
        self.current = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(max_attempts: u32) -> RetryPolicy {
        RetryPolicy {
            max_attempts,
            base_delay: Duration::from_secs(30),
            max_delay: Duration::from_secs(15 * 60),
        }
    }

    #[test]
    fn cooldown_is_exponential_and_capped() {
        let p = policy(3);
        assert_eq!(p.cooldown(1), Duration::from_secs(30));
        assert_eq!(p.cooldown(2), Duration::from_secs(60));
        assert_eq!(p.cooldown(3), Duration::from_secs(120));
        assert_eq!(p.cooldown(10), Duration::from_secs(900));
        assert_eq!(p.cooldown(u32::MAX), Duration::from_secs(900));
        assert_eq!(p.cooldown(0), Duration::from_secs(30));
    }

    #[test]
    fn message_failures_give_up_after_budget() {
        let mut t = RetryTracker::new(policy(3));
        let now = Instant::now();
        assert!(matches!(
            t.record_failure(1, FailureKind::Message, now),
            Verdict::Retry { strikes: 1, .. }
        ));
        assert_eq!(t.cooling_down(now), vec![1]);
        assert!(t.cooling_down(now + Duration::from_secs(31)).is_empty());
        assert!(matches!(
            t.record_failure(1, FailureKind::Message, now),
            Verdict::Retry { strikes: 2, .. }
        ));
        assert_eq!(
            t.record_failure(1, FailureKind::Message, now),
            Verdict::GiveUp { strikes: 3 }
        );
        assert!(t.is_empty());
    }

    #[test]
    fn systemic_failures_count_only_when_previously_healthy() {
        let mut t = RetryTracker::new(policy(1));
        let now = Instant::now();
        // outage from the start: never blamed
        for _ in 0..10 {
            assert!(matches!(
                t.record_failure(1, FailureKind::Systemic, now),
                Verdict::Retry { strikes: 0, .. }
            ));
        }
        // the system recovers (another message succeeds) ...
        t.record_success(2);
        // ... and message 1 still fails: now it is blamed
        assert_eq!(
            t.record_failure(1, FailureKind::Systemic, now),
            Verdict::GiveUp { strikes: 1 }
        );
        // the systemic failure marked the system unhealthy again
        assert!(matches!(
            t.record_failure(3, FailureKind::Systemic, now),
            Verdict::Retry { strikes: 0, .. }
        ));
    }

    #[test]
    fn group_failures_are_judged_once() {
        let mut t = RetryTracker::new(policy(1));
        let now = Instant::now();
        t.record_success(0); // healthy
        let v = t.record_failures(&[1, 2, 3], FailureKind::Systemic, now);
        assert!(
            v.iter()
                .all(|v| matches!(v, Verdict::GiveUp { strikes: 1 })),
            "{v:?}"
        );
        let v = t.record_failures(&[4, 5], FailureKind::Systemic, now);
        assert!(
            v.iter()
                .all(|v| matches!(v, Verdict::Retry { strikes: 0, .. })),
            "{v:?}"
        );
    }

    #[test]
    fn success_clears_state() {
        let mut t = RetryTracker::new(policy(2));
        let now = Instant::now();
        t.record_failure(1, FailureKind::Message, now);
        t.record_success(1);
        assert!(t.cooling_down(now).is_empty());
        assert!(matches!(
            t.record_failure(1, FailureKind::Message, now),
            Verdict::Retry { strikes: 1, .. }
        ));
    }

    #[test]
    fn stale_entries_are_forgotten() {
        let mut t = RetryTracker::new(policy(5));
        let now = Instant::now();
        t.record_failure(1, FailureKind::Message, now);
        assert_eq!(t.len(), 1);
        t.cooling_down(now + FORGET_AFTER + Duration::from_secs(1));
        assert!(t.is_empty());
    }

    #[test]
    fn zero_base_delay_means_no_cooldown() {
        let mut t = RetryTracker::new(RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::ZERO,
            max_delay: Duration::from_secs(60),
        });
        let now = Instant::now();
        t.record_failure(1, FailureKind::Message, now);
        assert!(t.cooling_down(now).is_empty());
    }

    #[test]
    fn backoff_doubles_and_resets() {
        let mut b = Backoff::new(Duration::from_secs(5), Duration::from_secs(60));
        assert_eq!(b.next_delay(), Duration::from_secs(5));
        assert_eq!(b.next_delay(), Duration::from_secs(10));
        assert_eq!(b.next_delay(), Duration::from_secs(20));
        assert_eq!(b.next_delay(), Duration::from_secs(40));
        assert_eq!(b.next_delay(), Duration::from_secs(60));
        assert_eq!(b.next_delay(), Duration::from_secs(60));
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_secs(5));
    }
}
