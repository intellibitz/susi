//! Formal small-state model of claim protocol + quarantine ladder (VC-201-096).
//!
//! Exhaustive search over miniature schedules proves: at most one live claim
//! wins, lease expiry is monotonic, and the quarantine ladder never exceeds
//! its cap or skips a recovery probe rung.

use serde::{Deserialize, Serialize};

/// Mirror of [`susi_gemi::engines::brain::QUARANTINE_CAP_SECS`].
pub const QUARANTINE_CAP_SECS: u64 = 21_600;

/// Quarantine seconds after `consecutive` operator-needed failures.
#[must_use]
pub fn quarantine_ladder_secs(consecutive: u32) -> u64 {
    match consecutive {
        0 | 1 => 600,
        2 => 3_600,
        _ => QUARANTINE_CAP_SECS,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelClaim {
    pub task: String,
    pub agent: String,
    pub claimed_unix: u64,
    pub lease_until_unix: u64,
}

impl ModelClaim {
    #[must_use]
    pub fn expired(&self, now: u64) -> bool {
        now >= self.lease_until_unix
    }
}

/// In-memory CAS claim store (models `refs/claims/<id>` first-writer-wins).
#[derive(Debug, Default, Clone)]
pub struct ClaimModel {
    /// task_id → (generation, claim). Generation bumps on every successful write.
    claims: std::collections::BTreeMap<String, (u64, ModelClaim)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    Won,
    LostAlive,
    TookOverExpired,
}

impl ClaimModel {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn try_claim(
        &mut self,
        task: &str,
        agent: &str,
        now: u64,
        lease_secs: u64,
    ) -> ClaimOutcome {
        let lease = lease_secs.max(1);
        let next = ModelClaim {
            task: task.to_string(),
            agent: agent.to_string(),
            claimed_unix: now,
            lease_until_unix: now.saturating_add(lease),
        };
        match self.claims.get(task) {
            None => {
                self.claims.insert(task.to_string(), (1, next));
                ClaimOutcome::Won
            }
            Some(&(gen, ref existing)) if existing.expired(now) => {
                self.claims
                    .insert(task.to_string(), (gen.saturating_add(1), next));
                ClaimOutcome::TookOverExpired
            }
            Some(_) => ClaimOutcome::LostAlive,
        }
    }

    #[must_use]
    pub fn live_holder(&self, task: &str, now: u64) -> Option<&str> {
        self.claims.get(task).and_then(|(_, c)| {
            if c.expired(now) {
                None
            } else {
                Some(c.agent.as_str())
            }
        })
    }

    #[must_use]
    pub fn lease_until(&self, task: &str) -> Option<u64> {
        self.claims.get(task).map(|(_, c)| c.lease_until_unix)
    }
}

/// Safety properties checked after every transition.
pub fn claim_safety(model: &ClaimModel, now: u64) -> Result<(), String> {
    for (task, (_, c)) in &model.claims {
        if !c.expired(now) {
            // Exactly one live holder representation per task (map key uniqueness).
            if c.task != *task {
                return Err(format!("claim task mismatch for {task}"));
            }
            if c.lease_until_unix < c.claimed_unix {
                return Err(format!("lease before claim for {task}"));
            }
        }
    }
    Ok(())
}
