#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failing contract check should stop loudly"
)]
//! Downstream contracts for model health, account budgets and explicit model selection.

use susi_core::model_health::{FailureClass, ModelHealth};
use susi_gawd_agents::cloud_budget::{
    BudgetLedger, Denial, FreeHeadroom, ReserveRequest, SpendPolicy,
};
use susi_gawd_agents::cloud_intent::{
    select, BlockReason, Candidate, IntentConstraints, PinFallback,
};

#[test]
fn model_health_preserves_reason_and_failure_class_across_transitions() {
    let health = ModelHealth::default().mark_healthy().mark_degraded("slow");
    assert!(health.is_usable());
    assert_eq!(health.failure_class(), None);
    let failed = health.mark_unhealthy(FailureClass::RateLimited, "retry later");
    assert!(!failed.is_usable());
    assert!(!failed.is_terminal());
    assert_eq!(failed.failure_class(), Some(FailureClass::RateLimited));
    let dead = failed.mark_dead(FailureClass::NotFound);
    assert!(dead.is_terminal());
    assert_eq!(dead.failure_class(), Some(FailureClass::NotFound));
}

#[test]
fn account_budget_enforces_quota_and_settles_each_reservation_once() {
    let ledger = BudgetLedger::new();
    let req = ReserveRequest {
        account: "fixture-account",
        credential_fp: "first",
        model: "fixture-model",
        policy: SpendPolicy::FreeOnly,
        is_free_candidate: true,
        free_headroom: FreeHeadroom::Exhausted,
        estimate: 7,
    };
    assert_eq!(ledger.reserve(req), Err(Denial::FreeExhausted));
    let paid = ReserveRequest {
        is_free_candidate: false,
        policy: SpendPolicy::PaidAuthorized {
            max_spend_micro: 10,
        },
        ..req
    };
    let first = ledger.reserve(paid).unwrap();
    assert_eq!(ledger.account_exposure(req.account), 7);
    assert!(matches!(
        ledger.reserve(ReserveRequest {
            credential_fp: "second",
            ..paid
        }),
        Err(Denial::OverBudget {
            held_plus_committed: 7,
            requested: 7,
            cap: 10
        })
    ));
    ledger.commit(first, Some(3));
    ledger.commit(first, Some(9));
    assert_eq!(ledger.account_exposure(req.account), 3);
    let released = ledger.reserve(paid).unwrap();
    ledger.release(released);
    assert_eq!(ledger.account_exposure(req.account), 3);
    let estimated = ledger.reserve(paid).unwrap();
    ledger.commit(estimated, None);
    assert_eq!(ledger.account_exposure(req.account), 10);
}

#[test]
fn a_strict_model_pin_rejects_a_different_candidate() {
    let intent = IntentConstraints {
        pinned_model: Some("requested".into()),
        pin_fallback: PinFallback::Deny,
        ..Default::default()
    };
    let alternative = Candidate {
        provider: "fixture".into(),
        api_key: "fixture-only".into(),
        account: None,
        region: None,
        model: "alternative".into(),
        context_tokens: 8192,
        modalities: vec![],
        supports_tools: true,
        supports_structured_output: true,
        residency: None,
        est_latency_ms: 1,
        cost_per_mtok: None,
        quality: Default::default(),
    };
    let result = select(
        &intent,
        &[alternative],
        &Default::default(),
        &Default::default(),
        100,
    );
    assert!(result.next_after(&[]).is_none());
    assert_eq!(result.rejected.len(), 1);
    assert!(matches!(
        result.rejected[0].reason,
        BlockReason::PinMismatch
    ));
    assert!(!result.rejected[0].candidate.contains("fixture-only"));
}
