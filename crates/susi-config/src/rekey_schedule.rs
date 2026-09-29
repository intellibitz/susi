//! Automated cluster-key rotation schedule (VC-201-077).
//!
//! The rekey protocol (`stage_key` / `activate_staged_key`) already exists;
//! this module adds the *policy* that decides when a coordinator should
//! start a rotation: on a wall-clock interval, and whenever a member was
//! removed. Offline peers are tolerated — staging stays inert until the
//! ledger commits the fingerprint, so a lagging node activates when it
//! next catches up.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Why a rekey should (or should not) start now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RekeyReason {
    /// Wall-clock interval since the last successful activation elapsed.
    ScheduleDue,
    /// A member was removed since the last activation — rotate so the
    /// departed node cannot keep signing with the shared key.
    MemberRemoved,
    /// Both schedule and membership triggers fired.
    ScheduleAndMemberRemoved,
    /// Neither trigger is active.
    NotDue,
}

/// Operator policy for automatic rotation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RekeyPolicy {
    /// Rotate at least this often (seconds). `0` disables the schedule arm.
    pub interval_secs: u64,
    /// Rotate after any membership removal recorded since last activation.
    pub rotate_on_member_removed: bool,
}

impl Default for RekeyPolicy {
    fn default() -> Self {
        Self {
            interval_secs: 30 * 24 * 60 * 60, // 30 days
            rotate_on_member_removed: true,
        }
    }
}

/// Inputs the coordinator observes when evaluating the policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RekeyScheduleDecision {
    pub should_rekey: bool,
    pub reason: RekeyReason,
    /// Unix seconds of the last successful activation (`0` = never).
    pub last_activated_unix: u64,
    pub now_unix: u64,
    pub members_removed_since: u64,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Evaluate whether a rekey should start. Pure: no I/O, no key mutation —
/// the caller stages/activates through the existing protocol, which already
/// tolerates offline peers (staged key is inert until commit).
#[must_use]
pub fn evaluate_rekey_policy(
    policy: &RekeyPolicy,
    last_activated_unix: u64,
    members_removed_since: u64,
    now_unix_secs: Option<u64>,
) -> RekeyScheduleDecision {
    let now = now_unix_secs.unwrap_or_else(now_unix);
    let schedule_due =
        policy.interval_secs > 0 && now.saturating_sub(last_activated_unix) >= policy.interval_secs;
    let member_due = policy.rotate_on_member_removed && members_removed_since > 0;

    let (should_rekey, reason) = match (schedule_due, member_due) {
        (true, true) => (true, RekeyReason::ScheduleAndMemberRemoved),
        (true, false) => (true, RekeyReason::ScheduleDue),
        (false, true) => (true, RekeyReason::MemberRemoved),
        (false, false) => (false, RekeyReason::NotDue),
    };

    RekeyScheduleDecision {
        should_rekey,
        reason,
        last_activated_unix,
        now_unix: now,
        members_removed_since,
    }
}
