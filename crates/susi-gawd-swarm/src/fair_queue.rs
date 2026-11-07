//! Bounded fair queues for concurrent missions (VC-201-025).
//!
//! Per-workspace concurrency and queue limits with aging fairness so a
//! sustained load from one mission cannot indefinitely starve another.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueLimits {
    pub max_running: usize,
    pub max_queued: usize,
    /// Age (unix secs) after which a waiting mission gains priority boost.
    pub age_boost_secs: u64,
}

impl Default for QueueLimits {
    fn default() -> Self {
        Self {
            max_running: 2,
            max_queued: 8,
            age_boost_secs: 30,
        }
    }
}

impl QueueLimits {
    /// Verifies structural invariants of [`QueueLimits`].
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if `max_running`, `max_queued`, or `age_boost_secs` is `0`.
    pub fn verify_limits_invariants(&self) -> Result<(), String> {
        if self.max_running == 0 {
            return Err("QueueLimits max_running must be > 0".to_string());
        }
        if self.max_queued == 0 {
            return Err("QueueLimits max_queued must be > 0".to_string());
        }
        if self.age_boost_secs == 0 {
            return Err("QueueLimits age_boost_secs must be > 0".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedMission {
    pub mission_id: String,
    pub workspace: String,
    pub enqueued_at: u64,
    pub weight: u32,
}

impl QueuedMission {
    /// Verifies structural invariants of a [`QueuedMission`].
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if `mission_id` or `workspace` is empty, or if `weight == 0`.
    pub fn verify_mission_invariants(&self) -> Result<(), String> {
        if self.mission_id.trim().is_empty() {
            return Err("QueuedMission mission_id must not be empty".to_string());
        }
        if self.workspace.trim().is_empty() {
            return Err("QueuedMission workspace must not be empty".to_string());
        }
        if self.weight == 0 {
            return Err("QueuedMission weight must be > 0".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueResult {
    Running,
    Queued,
    RejectedFull,
}

#[derive(Debug, Default)]
pub struct FairQueue {
    limits: QueueLimits,
    running: BTreeMap<String, QueuedMission>,
    waiting: Vec<QueuedMission>,
}

impl FairQueue {
    #[must_use]
    pub fn new(limits: QueueLimits) -> Self {
        Self {
            limits,
            running: BTreeMap::new(),
            waiting: Vec::new(),
        }
    }

    #[must_use]
    pub fn running_count(&self) -> usize {
        self.running.len()
    }

    #[must_use]
    pub fn waiting_count(&self) -> usize {
        self.waiting.len()
    }

    /// Admit a mission: run immediately if under concurrency cap, else queue
    /// (or reject when the waiting bound is full).
    pub fn enqueue(&mut self, mission: QueuedMission) -> EnqueueResult {
        if self.running.len() < self.limits.max_running {
            self.running.insert(mission.mission_id.clone(), mission);
            return EnqueueResult::Running;
        }
        if self.waiting.len() >= self.limits.max_queued {
            return EnqueueResult::RejectedFull;
        }
        self.waiting.push(mission);
        EnqueueResult::Queued
    }

    /// Release a running mission and promote the next fair waiter.
    pub fn complete(&mut self, mission_id: &str, now: u64) -> Option<QueuedMission> {
        self.running.remove(mission_id)?;
        self.promote(now)
    }

    /// Weighted aging: prefer highest `weight`, then oldest when both aged.
    fn promote(&mut self, now: u64) -> Option<QueuedMission> {
        if self.waiting.is_empty() || self.running.len() >= self.limits.max_running {
            return None;
        }
        let boost = self.limits.age_boost_secs;
        let idx = self
            .waiting
            .iter()
            .enumerate()
            .max_by_key(|(_, m)| {
                let age = now.saturating_sub(m.enqueued_at);
                let aged = age >= boost;
                // Aged waiters outrank any weight; among them, older wins.
                let age_score = if aged { 1_000_000_000u64 } else { 0 };
                age_score + age * 1_000 + u64::from(m.weight)
            })
            .map(|(i, _)| i)?;
        let next = self.waiting.remove(idx);
        self.running.insert(next.mission_id.clone(), next.clone());
        Some(next)
    }

    /// True when a later-enqueued mission eventually runs despite an earlier
    /// sustained load from another mission (aging fairness).
    #[must_use]
    pub fn would_starve_without_aging(
        &self,
        sustained_id: &str,
        starved_id: &str,
        now: u64,
    ) -> bool {
        let Some(starved) = self.waiting.iter().find(|m| m.mission_id == starved_id) else {
            return false;
        };
        let aged = now.saturating_sub(starved.enqueued_at) >= self.limits.age_boost_secs;
        aged && self.running.contains_key(sustained_id)
    }

    /// Verifies structural, capacity, and disjointness invariants of the [`FairQueue`].
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if `limits` fails [`QueueLimits::verify_limits_invariants`],
    /// `running.len() > limits.max_running`, `waiting.len() > limits.max_queued`, any mission fails
    /// [`QueuedMission::verify_mission_invariants`], or a mission appears in both `running` and `waiting`.
    pub fn verify_queue_invariants(&self) -> Result<(), String> {
        self.limits.verify_limits_invariants()?;
        if self.running.len() > self.limits.max_running {
            return Err(format!(
                "FairQueue running count ({}) exceeds max_running ({})",
                self.running.len(),
                self.limits.max_running
            ));
        }
        if self.waiting.len() > self.limits.max_queued {
            return Err(format!(
                "FairQueue waiting count ({}) exceeds max_queued ({})",
                self.waiting.len(),
                self.limits.max_queued
            ));
        }
        for (id, m) in &self.running {
            if id != &m.mission_id {
                return Err(format!(
                    "FairQueue running key '{id}' != mission_id '{}'",
                    m.mission_id
                ));
            }
            m.verify_mission_invariants()?;
        }
        for m in &self.waiting {
            m.verify_mission_invariants()?;
            if self.running.contains_key(&m.mission_id) {
                return Err(format!(
                    "FairQueue mission '{}' cannot be in both running and waiting",
                    m.mission_id
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn default_queue_limits_and_enqueue_respect_running_and_waiting_bounds() {
        let def = QueueLimits::default();
        assert_eq!(def.max_running, 2);
        assert_eq!(def.max_queued, 8);
        assert_eq!(def.age_boost_secs, 30);

        let mut q = FairQueue::new(QueueLimits {
            max_running: 1,
            max_queued: 1,
            age_boost_secs: 10,
        });
        let m1 = QueuedMission {
            mission_id: "m1".to_string(),
            workspace: "ws".to_string(),
            enqueued_at: 100,
            weight: 1,
        };
        let m2 = QueuedMission {
            mission_id: "m2".to_string(),
            ..m1.clone()
        };
        let m3 = QueuedMission {
            mission_id: "m3".to_string(),
            ..m1.clone()
        };
        assert_eq!(q.enqueue(m1), EnqueueResult::Running);
        assert_eq!(q.enqueue(m2), EnqueueResult::Queued);
        assert_eq!(q.enqueue(m3), EnqueueResult::RejectedFull);
        assert_eq!(q.running_count(), 1);
        assert_eq!(q.waiting_count(), 1);
    }

    #[test]
    fn complete_promotes_highest_weight_waiter_when_none_are_aged() {
        let mut q = FairQueue::new(QueueLimits {
            max_running: 1,
            max_queued: 4,
            age_boost_secs: 100,
        });
        q.enqueue(QueuedMission {
            mission_id: "run-1".to_string(),
            workspace: "ws".to_string(),
            enqueued_at: 10,
            weight: 1,
        });
        q.enqueue(QueuedMission {
            mission_id: "wait-low".to_string(),
            workspace: "ws".to_string(),
            enqueued_at: 10,
            weight: 2,
        });
        q.enqueue(QueuedMission {
            mission_id: "wait-high".to_string(),
            workspace: "ws".to_string(),
            enqueued_at: 10,
            weight: 9,
        });

        let promoted = q.complete("run-1", 15).expect("promote next");
        assert_eq!(promoted.mission_id, "wait-high");
        assert_eq!(q.waiting_count(), 1);
    }

    #[test]
    fn complete_promotes_aged_waiter_ahead_of_higher_weight_fresh_waiter() {
        let mut q = FairQueue::new(QueueLimits {
            max_running: 1,
            max_queued: 4,
            age_boost_secs: 20,
        });
        q.enqueue(QueuedMission {
            mission_id: "run-1".to_string(),
            workspace: "ws".to_string(),
            enqueued_at: 0,
            weight: 1,
        });
        q.enqueue(QueuedMission {
            mission_id: "aged-low-weight".to_string(),
            workspace: "ws".to_string(),
            enqueued_at: 5,
            weight: 1,
        });
        q.enqueue(QueuedMission {
            mission_id: "fresh-heavy".to_string(),
            workspace: "ws".to_string(),
            enqueued_at: 25,
            weight: 500,
        });

        let promoted = q.complete("run-1", 30).expect("promote aged waiter");
        assert_eq!(promoted.mission_id, "aged-low-weight");
    }

    #[test]
    fn would_starve_without_aging_detects_aged_waiter_behind_sustained_running_mission() {
        let mut q = FairQueue::new(QueueLimits {
            max_running: 1,
            max_queued: 4,
            age_boost_secs: 10,
        });
        q.enqueue(QueuedMission {
            mission_id: "sustained".to_string(),
            workspace: "ws".to_string(),
            enqueued_at: 0,
            weight: 10,
        });
        q.enqueue(QueuedMission {
            mission_id: "starved".to_string(),
            workspace: "ws".to_string(),
            enqueued_at: 5,
            weight: 1,
        });
        assert!(!q.would_starve_without_aging("sustained", "starved", 10));
        assert!(q.would_starve_without_aging("sustained", "starved", 15));
        assert!(!q.would_starve_without_aging("other", "starved", 15));
    }

    #[test]
    fn complete_returns_none_for_unknown_mission_and_queue_types_roundtrip_serde() {
        let mut q = FairQueue::default();
        assert!(q.complete("nonexistent", 100).is_none());

        let mission = QueuedMission {
            mission_id: "m-serde".to_string(),
            workspace: "ws-1".to_string(),
            enqueued_at: 42,
            weight: 3,
        };
        let encoded = serde_json::to_string(&mission).expect("serialize QueuedMission");
        let decoded: QueuedMission =
            serde_json::from_str(&encoded).expect("deserialize QueuedMission");
        assert_eq!(decoded, mission);

        assert!(QueueLimits::default().verify_limits_invariants().is_ok());
        assert!(QueueLimits {
            max_running: 0,
            ..QueueLimits::default()
        }
        .verify_limits_invariants()
        .is_err());

        assert!(mission.verify_mission_invariants().is_ok());
        assert!(QueuedMission {
            weight: 0,
            ..mission.clone()
        }
        .verify_mission_invariants()
        .is_err());

        let mut fq = FairQueue::default();
        fq.enqueue(mission);
        assert!(fq.verify_queue_invariants().is_ok());
    }
}
