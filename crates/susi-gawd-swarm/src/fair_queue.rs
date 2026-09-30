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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedMission {
    pub mission_id: String,
    pub workspace: String,
    pub enqueued_at: u64,
    pub weight: u32,
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
}
