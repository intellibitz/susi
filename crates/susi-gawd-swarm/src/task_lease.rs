//! Durable task ownership leases with monotonic fencing tokens (VC-201-022).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLease {
    pub task_id: String,
    pub owner: String,
    pub fence: u64,
    pub lease_until_unix: u64,
}

/// Lease state is serialized into mission state (T-DEVIN-9): a restarted
/// run keeps the monotonic `next_fence` so a stale worker's token can never
/// collide with a freshly issued lease, and live leases survive crashes.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseTable {
    #[serde(default)]
    leases: std::collections::BTreeMap<String, TaskLease>,
    #[serde(default)]
    next_fence: u64,
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
        self.next_fence = self.next_fence.saturating_add(1);
        let lease = TaskLease {
            task_id: task_id.to_string(),
            owner: owner.to_string(),
            fence: self.next_fence,
            lease_until_unix: now.saturating_add(ttl.max(1)),
        };
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
        }
        verdict
    }

    /// Snapshot of live leases (for durable mission state).
    #[must_use]
    pub fn leases(&self) -> &std::collections::BTreeMap<String, TaskLease> {
        &self.leases
    }

    /// Restore persisted lease state: adopt live leases and keep the fence
    /// counter monotonic across restarts (`max` — never rewind a fence).
    pub fn adopt(&mut self, persisted: &LeaseTable) {
        self.next_fence = self.next_fence.max(persisted.next_fence);
        for (id, lease) in &persisted.leases {
            self.leases
                .entry(id.clone())
                .or_insert_with(|| lease.clone());
        }
    }
}
