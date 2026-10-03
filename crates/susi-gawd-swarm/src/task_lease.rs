//! Durable task ownership leases with monotonic fencing tokens (VC-201-022).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// The authority generation that owns a mission's leases.
///
/// Fence numbers are unique within a mission record, while these epochs make
/// a coordinator restart distinguishable even when a worker presents a fence
/// that was valid before the restart.  Both values are persisted with the
/// mission and only move forward.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OwnershipEpoch {
    pub mission: u64,
    pub coordinator: u64,
}

/// Recovery metadata that must survive a coordinator crash.  A scope is
/// recorded before workers start so a replacement coordinator can cancel or
/// quarantine it before redispatching the node.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRecovery {
    pub generation: u64,
    pub recovered_at_unix: u64,
    #[serde(default)]
    pub active_scopes: BTreeSet<String>,
    #[serde(default)]
    pub revoked_workers: BTreeSet<String>,
}

/// Authority presented at a mutating boundary.  It is intentionally
/// immutable: a worker cannot renew or manufacture a token by changing its
/// local copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityToken {
    pub epoch: OwnershipEpoch,
    pub fence: u64,
}

/// A successful pre-side-effect authorization.  It is intentionally owned so
/// a delayed worker can be rechecked after another worker takes over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationPermit {
    pub task_id: String,
    pub owner: String,
    pub token: AuthorityToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLease {
    pub task_id: String,
    pub owner: String,
    pub fence: u64,
    pub lease_until_unix: u64,
    /// Last accepted renewal. Older mission files deserialize this as zero.
    #[serde(default)]
    pub renewed_at_unix: u64,
}

/// Lease state is serialized into mission state (T-DEVIN-9): a restarted
/// run keeps the monotonic `next_fence` so a stale worker's token can never
/// collide with a freshly issued lease, and live leases survive crashes.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseTable {
    #[serde(default)]
    leases: BTreeMap<String, TaskLease>,
    #[serde(default)]
    next_fence: u64,
    #[serde(default)]
    pub epoch: OwnershipEpoch,
    #[serde(default)]
    pub recovery: LeaseRecovery,
    #[serde(default)]
    lease_epochs: BTreeMap<String, OwnershipEpoch>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompleteVerdict {
    Accepted,
    StaleFence,
    NotOwner,
    Expired,
    UnknownTask,
}

impl LeaseTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn dispatch(&mut self, task_id: &str, owner: &str, now: u64, ttl: u64) -> TaskLease {
        self.dispatch_in_epoch(task_id, owner, now, ttl, self.epoch)
    }

    /// Issue a lease bound to a coordinator generation.
    #[allow(clippy::too_many_arguments)] // task identity, owner, clock, TTL and epoch are all part of the lease boundary
    pub fn dispatch_in_epoch(
        &mut self,
        task_id: &str,
        owner: &str,
        now: u64,
        ttl: u64,
        epoch: OwnershipEpoch,
    ) -> TaskLease {
        let epoch = self.epoch.max(epoch);
        self.epoch = epoch;
        self.next_fence = self.next_fence.saturating_add(1);
        let lease = TaskLease {
            task_id: task_id.to_string(),
            owner: owner.to_string(),
            fence: self.next_fence,
            lease_until_unix: now.saturating_add(ttl.max(1)),
            renewed_at_unix: now,
        };
        self.lease_epochs.insert(task_id.to_string(), epoch);
        self.recovery.revoked_workers.remove(owner);
        self.leases.insert(task_id.to_string(), lease.clone());
        lease
    }

    /// Non-destructive fence check (T-DEVIN-9): the verdict a write or
    /// completion would get, without consuming the lease. A worker must
    /// pass this *before* its first mutating operation — rejecting only at
    /// completion cannot undo the writes already made.
    #[must_use]
    pub fn check_fence(&self, task_id: &str, owner: &str, fence: u64, now: u64) -> CompleteVerdict {
        let Some(lease) = self.leases.get(task_id) else {
            return CompleteVerdict::UnknownTask;
        };
        if self
            .lease_epochs
            .get(task_id)
            .is_some_and(|lease_epoch| *lease_epoch != self.epoch)
        {
            return CompleteVerdict::StaleFence;
        }
        if now >= lease.lease_until_unix {
            return CompleteVerdict::Expired;
        }
        // Fence is checked before owner so a recovered worker with an old
        // token is rejected as StaleFence even if it still names the prior owner.
        if fence != lease.fence {
            return CompleteVerdict::StaleFence;
        }
        if lease.owner != owner {
            return CompleteVerdict::NotOwner;
        }
        CompleteVerdict::Accepted
    }

    /// Check an authority token at the exact boundary before a mutation.
    /// Epoch mismatch is reported as stale even when the fence number happens
    /// to match, which prevents a restarted coordinator from reusing an old
    /// token.
    #[must_use]
    pub fn check_authority(
        &self,
        task_id: &str,
        owner: &str,
        token: AuthorityToken,
        now: u64,
    ) -> CompleteVerdict {
        let Some(_lease) = self.leases.get(task_id) else {
            return CompleteVerdict::UnknownTask;
        };
        let lease_epoch = self
            .lease_epochs
            .get(task_id)
            .copied()
            .unwrap_or(self.epoch);
        if token.epoch != self.epoch || lease_epoch != token.epoch {
            return CompleteVerdict::StaleFence;
        }
        self.check_fence(task_id, owner, token.fence, now)
    }

    /// Authorize one mutating operation without changing lease state.
    /// The returned permit can be held across a delayed operation, but must
    /// still be checked again immediately before that operation.
    pub fn authorize_mutation(
        &self,
        task_id: &str,
        owner: &str,
        token: AuthorityToken,
        now: u64,
    ) -> Result<MutationPermit, CompleteVerdict> {
        match self.check_authority(task_id, owner, token, now) {
            CompleteVerdict::Accepted => Ok(MutationPermit {
                task_id: task_id.to_string(),
                owner: owner.to_string(),
                token,
            }),
            verdict @ (CompleteVerdict::StaleFence
            | CompleteVerdict::NotOwner
            | CompleteVerdict::Expired
            | CompleteVerdict::UnknownTask) => Err(verdict),
        }
    }

    /// Verify a permit immediately before a side effect.  A permit is not a
    /// completion token: every mutating tool call must invoke this boundary.
    #[must_use]
    pub fn check_permit(&self, permit: MutationPermit, now: u64) -> CompleteVerdict {
        self.check_authority(&permit.task_id, &permit.owner, permit.token, now)
    }

    /// Renew a live lease and record when the renewal was accepted.
    #[allow(clippy::too_many_arguments)] // renewal must bind task, owner, authority, clock and TTL atomically
    pub fn renew(
        &mut self,
        task_id: &str,
        owner: &str,
        token: AuthorityToken,
        now: u64,
        ttl: u64,
    ) -> CompleteVerdict {
        let verdict = self.check_authority(task_id, owner, token, now);
        if verdict != CompleteVerdict::Accepted {
            return verdict;
        }
        if let Some(lease) = self.leases.get_mut(task_id) {
            lease.lease_until_unix = now.saturating_add(ttl.max(1));
            lease.renewed_at_unix = now;
        }
        verdict
    }

    /// Authoritative completion: requires matching live owner + fence.
    pub fn complete(
        &mut self,
        task_id: &str,
        owner: &str,
        fence: u64,
        now: u64,
    ) -> CompleteVerdict {
        let verdict = self.check_fence(task_id, owner, fence, now);
        if verdict == CompleteVerdict::Accepted {
            self.leases.remove(task_id);
            self.lease_epochs.remove(task_id);
        }
        verdict
    }

    /// Snapshot of live leases (for durable mission state).
    #[must_use]
    pub fn leases(&self) -> &BTreeMap<String, TaskLease> {
        &self.leases
    }

    /// Advance authority for coordinator recovery and revoke every old lease.
    /// Revocation is represented by retaining the old lease and epoch mapping
    /// until a replacement is dispatched, so delayed completions are reported
    /// as stale rather than becoming an unknown, ambiguous write.
    pub fn begin_recovery(&mut self, epoch: OwnershipEpoch, now: u64) {
        let epoch = self.epoch.max(epoch);
        self.recovery.generation = self.recovery.generation.saturating_add(1);
        self.recovery.recovered_at_unix = now;
        for lease in self.leases.values() {
            self.recovery.revoked_workers.insert(lease.owner.clone());
        }
        self.epoch = epoch;
    }

    /// Return the authority token bound to the current lease, including its
    /// original epoch when the lease has been revoked by recovery.
    #[must_use]
    pub fn authority_for(&self, task_id: &str) -> Option<AuthorityToken> {
        let lease = self.leases.get(task_id)?;
        let epoch = self
            .lease_epochs
            .get(task_id)
            .copied()
            .unwrap_or(self.epoch);
        Some(lease.authority(epoch))
    }

    /// Restore persisted lease state: adopt live leases and keep the fence
    /// counter monotonic across restarts (`max` — never rewind a fence).
    pub fn adopt(&mut self, persisted: &LeaseTable) {
        self.next_fence = self.next_fence.max(persisted.next_fence);
        self.epoch = self.epoch.max(persisted.epoch);
        self.recovery.generation = self.recovery.generation.max(persisted.recovery.generation);
        self.recovery.recovered_at_unix = self
            .recovery
            .recovered_at_unix
            .max(persisted.recovery.recovered_at_unix);
        self.recovery
            .active_scopes
            .extend(persisted.recovery.active_scopes.iter().cloned());
        self.recovery
            .revoked_workers
            .extend(persisted.recovery.revoked_workers.iter().cloned());
        for (id, lease) in &persisted.leases {
            let replace = self
                .leases
                .get(id)
                .is_none_or(|current| lease.fence > current.fence);
            if replace {
                self.leases.insert(id.clone(), lease.clone());
                if let Some(epoch) = persisted.lease_epochs.get(id) {
                    self.lease_epochs.insert(id.clone(), *epoch);
                }
            }
        }
    }
}

impl OwnershipEpoch {
    /// Monotonically advance both mission and coordinator authority.
    #[must_use]
    pub fn next(self) -> Self {
        Self {
            mission: self.mission.saturating_add(1),
            coordinator: self.coordinator.saturating_add(1),
        }
    }
}

impl TaskLease {
    #[must_use]
    pub fn authority(&self, epoch: OwnershipEpoch) -> AuthorityToken {
        AuthorityToken {
            epoch,
            fence: self.fence,
        }
    }
}
