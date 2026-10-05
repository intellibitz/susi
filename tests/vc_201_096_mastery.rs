#![allow(missing_docs)] // integration test crate: no public API to document

use susi_gawd::formal_invariants::{
    budget_transition, lease_is_fenced, policy_precedence, BudgetTransition, PolicyClass,
    PolicyDecision,
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
