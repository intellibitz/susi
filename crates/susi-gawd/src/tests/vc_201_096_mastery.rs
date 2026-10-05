//! VC-201-096 mastery tests for the bounded formal-invariant models.

use crate::formal_invariants::{
    budget_transition, lease_is_fenced, policy_precedence, BudgetTransition, PolicyClass,
    PolicyDecision,
};

#[test]
fn vc_201_096_mastery() {
    assert!(lease_is_fenced(3, 3, 99, 100));
    assert_eq!(budget_transition(10, 20, 30, 60), BudgetTransition::Admit);
    assert_eq!(
        policy_precedence(PolicyClass::AskFirst, false, false),
        PolicyDecision::NeedsConsent
    );
}

#[test]
fn vc_201_096_mastery_lease_generation_and_expiry_fence_stale_claims() {
    assert!(lease_is_fenced(3, 3, 99, 100));
    assert!(!lease_is_fenced(2, 3, 99, 100));
    assert!(!lease_is_fenced(3, 3, 100, 100));
}

#[test]
fn vc_201_096_mastery_budget_transition_never_crosses_cap() {
    assert_eq!(budget_transition(10, 20, 30, 60), BudgetTransition::Admit);
    assert_eq!(budget_transition(10, 20, 31, 60), BudgetTransition::Reject);
    assert_eq!(
        budget_transition(u64::MAX, 1, 0, u64::MAX),
        BudgetTransition::Admit
    );
}

#[test]
fn vc_201_096_mastery_policy_precedence_requires_explicit_paid_authority() {
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
