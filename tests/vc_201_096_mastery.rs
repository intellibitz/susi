#![allow(missing_docs)] // integration test crate: no public API to document

use susi_gawd::formal_invariants::{
    budget_transition, claim_safety, lease_is_fenced, policy_precedence, quarantine_ladder_secs,
    BudgetTransition, ClaimModel, ClaimOutcome, PolicyClass, PolicyDecision, QUARANTINE_CAP_SECS,
};

#[test]
fn vc_201_096_mastery() {
    assert!(lease_is_fenced(3, 3, 99, 100));
    assert!(!lease_is_fenced(2, 3, 99, 100));
    assert!(!lease_is_fenced(3, 3, 100, 100));

    assert_eq!(budget_transition(10, 20, 30, 60), BudgetTransition::Admit);
    assert_eq!(budget_transition(10, 20, 31, 60), BudgetTransition::Reject);

    assert_eq!(
        policy_precedence(PolicyClass::FreeOnly, true, true),
        PolicyDecision::Free
    );
    assert_eq!(
        policy_precedence(PolicyClass::FreeOnly, false, false),
        PolicyDecision::Denied
    );
    assert_eq!(
        policy_precedence(PolicyClass::AskFirst, false, false),
        PolicyDecision::NeedsConsent
    );
    assert_eq!(
        policy_precedence(PolicyClass::PaidAuthorized, false, false),
        PolicyDecision::Paid
    );
}

#[test]
fn vc_201_096_quarantine_ladder_monotonically_caps_at_six_hours() {
    assert_eq!(quarantine_ladder_secs(0), 600);
    assert_eq!(quarantine_ladder_secs(1), 600);
    assert_eq!(quarantine_ladder_secs(2), 3_600);
    assert_eq!(quarantine_ladder_secs(3), QUARANTINE_CAP_SECS);
    assert_eq!(quarantine_ladder_secs(100), QUARANTINE_CAP_SECS);
    assert!(quarantine_ladder_secs(1) < quarantine_ladder_secs(2));
    assert!(quarantine_ladder_secs(2) < quarantine_ladder_secs(3));
}

#[test]
fn vc_201_096_claim_model_first_writer_wins_and_expired_lease_takeover() {
    let mut model = ClaimModel::new();
    assert_eq!(model.live_holder("T-1", 100), None);
    assert_eq!(model.lease_until("T-1"), None);

    assert_eq!(
        model.try_claim("T-1", "AGENT_A", 100, 60),
        ClaimOutcome::Won
    );
    assert_eq!(model.live_holder("T-1", 120), Some("AGENT_A"));
    assert_eq!(model.lease_until("T-1"), Some(160));
    assert!(claim_safety(&model, 120).is_ok());

    assert_eq!(
        model.try_claim("T-1", "AGENT_B", 159, 60),
        ClaimOutcome::LostAlive
    );
    assert_eq!(model.live_holder("T-1", 159), Some("AGENT_A"));

    assert_eq!(model.live_holder("T-1", 160), None);
    assert_eq!(
        model.try_claim("T-1", "AGENT_B", 160, 60),
        ClaimOutcome::TookOverExpired
    );
    assert_eq!(model.live_holder("T-1", 161), Some("AGENT_B"));
    assert_eq!(model.lease_until("T-1"), Some(220));
    assert!(claim_safety(&model, 161).is_ok());
}

#[test]
fn vc_201_096_budget_transition_saturating_addition_prevents_overflow_bypass() {
    assert_eq!(
        budget_transition(u64::MAX, 1, 1, u64::MAX - 1),
        BudgetTransition::Reject
    );
    assert_eq!(budget_transition(0, 0, 0, 0), BudgetTransition::Admit);
    assert_eq!(
        policy_precedence(PolicyClass::AskFirst, true, false),
        PolicyDecision::NeedsConsent
    );
}

#[test]
fn vc_201_096_policy_precedence_and_lease_fencing_boundary_matrix() {
    assert_eq!(
        policy_precedence(PolicyClass::AskFirst, true, true),
        PolicyDecision::Free
    );
    assert_eq!(
        policy_precedence(PolicyClass::PaidAuthorized, true, true),
        PolicyDecision::Free
    );
    assert_eq!(
        policy_precedence(PolicyClass::FreeOnly, true, false),
        PolicyDecision::Denied
    );
    assert_eq!(
        policy_precedence(PolicyClass::PaidAuthorized, true, false),
        PolicyDecision::Paid
    );
    assert!(!lease_is_fenced(5, 4, 50, 100));
    assert!(lease_is_fenced(4, 4, 0, 1));
}
