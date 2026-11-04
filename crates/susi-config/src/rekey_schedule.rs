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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_rekey_policy_covers_all_four_reasons_and_disabled_arms() {
        let policy = RekeyPolicy::default();
        assert_eq!(policy.interval_secs, 30 * 24 * 60 * 60);
        assert!(policy.rotate_on_member_removed);

        let last = 1_000_000;
        let before_due = last + policy.interval_secs - 1;
        let at_due = last + policy.interval_secs;

        let not_due = evaluate_rekey_policy(&policy, last, 0, Some(before_due));
        assert!(!not_due.should_rekey);
        assert_eq!(not_due.reason, RekeyReason::NotDue);
        assert_eq!(not_due.last_activated_unix, last);
        assert_eq!(not_due.now_unix, before_due);
        assert_eq!(not_due.members_removed_since, 0);

        let schedule_due = evaluate_rekey_policy(&policy, last, 0, Some(at_due));
        assert!(schedule_due.should_rekey);
        assert_eq!(schedule_due.reason, RekeyReason::ScheduleDue);

        let member_removed = evaluate_rekey_policy(&policy, last, 2, Some(before_due));
        assert!(member_removed.should_rekey);
        assert_eq!(member_removed.reason, RekeyReason::MemberRemoved);
        assert_eq!(member_removed.members_removed_since, 2);

        let both = evaluate_rekey_policy(&policy, last, 1, Some(at_due));
        assert!(both.should_rekey);
        assert_eq!(both.reason, RekeyReason::ScheduleAndMemberRemoved);

        let disabled = RekeyPolicy {
            interval_secs: 0,
            rotate_on_member_removed: false,
        };
        let disabled_decision = evaluate_rekey_policy(&disabled, 0, 5, Some(at_due));
        assert!(!disabled_decision.should_rekey);
        assert_eq!(disabled_decision.reason, RekeyReason::NotDue);
    }

    #[test]
    fn rekey_schedule_decision_and_policy_serde_roundtrip() {
        let policy = RekeyPolicy::default();
        let policy_json = serde_json::to_string(&policy).expect("serialize policy");
        let policy_back: RekeyPolicy =
            serde_json::from_str(&policy_json).expect("deserialize policy");
        assert_eq!(policy_back, policy);

        let cases = [
            (RekeyReason::ScheduleDue, "\"schedule_due\"", true),
            (RekeyReason::MemberRemoved, "\"member_removed\"", true),
            (
                RekeyReason::ScheduleAndMemberRemoved,
                "\"schedule_and_member_removed\"",
                true,
            ),
            (RekeyReason::NotDue, "\"not_due\"", false),
        ];

        for (reason, expected_wire, should_rekey) in cases {
            let reason_json = serde_json::to_string(&reason).expect("serialize reason");
            assert_eq!(reason_json, expected_wire);

            let decision = RekeyScheduleDecision {
                should_rekey,
                reason,
                last_activated_unix: 100,
                now_unix: 200,
                members_removed_since: 3,
            };
            let json = serde_json::to_string(&decision).expect("serialize decision");
            let back: RekeyScheduleDecision =
                serde_json::from_str(&json).expect("deserialize decision");
            assert_eq!(back, decision);
        }
    }

    #[test]
    fn evaluate_rekey_policy_handles_backward_clock_skew_and_live_system_clock() {
        let policy = RekeyPolicy::default();

        // 1. Clock skew backwards (now_unix_secs < last_activated_unix) saturates cleanly to 0 elapsed.
        let skewed = evaluate_rekey_policy(&policy, 5_000_000, 0, Some(0));
        assert!(!skewed.should_rekey);
        assert_eq!(skewed.reason, RekeyReason::NotDue);
        assert_eq!(skewed.now_unix, 0);
        assert_eq!(skewed.last_activated_unix, 5_000_000);

        // 2. Passing None for now_unix_secs reads a positive timestamp from the system clock.
        let live = evaluate_rekey_policy(&policy, u64::MAX, 0, None);
        assert!(live.now_unix > 0);
        assert!(!live.should_rekey);
        assert_eq!(live.reason, RekeyReason::NotDue);
    }

    #[test]
    fn evaluate_rekey_policy_with_zero_interval_only_triggers_on_member_removal() {
        let zero_interval = RekeyPolicy {
            interval_secs: 0,
            rotate_on_member_removed: true,
        };

        let not_due = evaluate_rekey_policy(&zero_interval, 0, 0, Some(u64::MAX));
        assert!(!not_due.should_rekey);
        assert_eq!(not_due.reason, RekeyReason::NotDue);
        assert_eq!(not_due.now_unix, u64::MAX);
        assert_eq!(not_due.members_removed_since, 0);

        let removed = evaluate_rekey_policy(&zero_interval, 0, 2, Some(u64::MAX));
        assert!(removed.should_rekey);
        assert_eq!(removed.reason, RekeyReason::MemberRemoved);
        assert_eq!(removed.members_removed_since, 2);
    }
}
