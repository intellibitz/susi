//! Mastery verification for VC-201-052: Reserve estimated spend
//! before requests and reconcile provider-reported usage afterward across
//! parallel agents and retries; unknown prices or missing usage follow
//! explicit policy and cannot silently bypass a hard budget.

use std::collections::BTreeMap;

use crate::cloud_failover::{run, AttemptOutcome, FailoverBudget, Runner, Stores};
use crate::cloud_lockout::LockoutTracker;
use susi_config::cloud_eligibility::{EligibilityStore, InferenceResult, Subject};
use susi_config::cloud_quota::QuotaInventory;
use susi_gawd_agents::cloud_budget::{BudgetLedger, SpendPolicy};
use susi_gawd_agents::cloud_intent::{Candidate, IntentConstraints};

struct UsageReporter<'a> {
    ledger: &'a BudgetLedger,
    observed_before_dispatch: Option<u64>,
    reported_usage: Option<u64>,
}

impl Runner for UsageReporter<'_> {
    fn attempt(&mut self, _index: usize, _deadline_remaining_ms: u64) -> AttemptOutcome {
        self.observed_before_dispatch = Some(self.ledger.account_exposure(""));
        AttemptOutcome::Success("measured-answer".into())
    }

    fn take_reported_usage_micros(&mut self) -> Option<u64> {
        self.reported_usage.take()
    }
}

fn candidate() -> Candidate {
    Candidate {
        provider: "acme".into(),
        api_key: "credential".into(),
        account: None,
        region: None,
        model: "measured-model".into(),
        context_tokens: 128_000,
        modalities: vec![],
        supports_tools: true,
        supports_structured_output: true,
        residency: None,
        est_latency_ms: 800,
        cost_per_mtok: Some(1.0),
        quality: BTreeMap::new(),
    }
}

#[test]
fn vc_201_052_mastery_reserve_and_reconcile() {
    let candidates = vec![candidate()];
    let mut eligibility = EligibilityStore::new();
    eligibility.record_inference(
        Subject {
            provider: &candidates[0].provider,
            api_key: &candidates[0].api_key,
            account: candidates[0].account.as_deref(),
            region: candidates[0].region.as_deref(),
            model: &candidates[0].model,
        },
        &InferenceResult::Success,
        1_700_000_000,
    );
    let quota = QuotaInventory::new();
    let mut lockouts = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
    let ledger = BudgetLedger::new();
    let mut runner = UsageReporter {
        ledger: &ledger,
        observed_before_dispatch: None,
        reported_usage: Some(7),
    };

    let result = run(
        &IntentConstraints {
            task_class: "coding".into(),
            discovery_budget: 4,
            ..Default::default()
        },
        &candidates,
        &mut Stores {
            eligibility: &mut eligibility,
            quota: &quota,
            lockouts: &mut lockouts,
            ledger: &ledger,
        },
        FailoverBudget {
            max_attempts: 1,
            deadline_ms: Some(1_700_000_060_000),
            spend: SpendPolicy::PaidAuthorized {
                max_spend_micro: 1_000,
            },
            attempt_estimate_micros: 50,
            now_ms: 1_700_000_000_000,
        },
        &mut runner,
    );

    assert_eq!(result.winner, Some(0));
    assert_eq!(
        runner.observed_before_dispatch,
        Some(50),
        "estimated spend must be reserved before dispatch"
    );
    assert_eq!(
        ledger.account_exposure(""),
        7,
        "provider-reported usage must be reconciled after completion"
    );
}

#[test]
fn vc_201_052_mastery_hard_budget_exceeded_refuses_dispatch() {
    let candidates = vec![candidate()];
    let mut eligibility = EligibilityStore::new();
    let quota = QuotaInventory::new();
    let mut lockouts = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
    let ledger = BudgetLedger::new();
    let mut runner = UsageReporter {
        ledger: &ledger,
        observed_before_dispatch: None,
        reported_usage: Some(10),
    };

    let result = run(
        &IntentConstraints {
            task_class: "coding".into(),
            discovery_budget: 4,
            ..Default::default()
        },
        &candidates,
        &mut Stores {
            eligibility: &mut eligibility,
            quota: &quota,
            lockouts: &mut lockouts,
            ledger: &ledger,
        },
        FailoverBudget {
            max_attempts: 1,
            deadline_ms: Some(1_700_000_060_000),
            spend: SpendPolicy::PaidAuthorized {
                max_spend_micro: 10, // Max budget is 10
            },
            attempt_estimate_micros: 50, // Estimate 50 exceeds budget 10!
            now_ms: 1_700_000_000_000,
        },
        &mut runner,
    );

    assert_eq!(
        result.winner, None,
        "dispatch must be refused when estimated spend exceeds max_spend_micro"
    );
    assert_eq!(
        runner.observed_before_dispatch, None,
        "no dispatch attempt should be executed when budget is exceeded"
    );
}

#[test]
fn vc_201_052_mastery_missing_reported_usage_retains_reserved_estimate() {
    let candidates = vec![candidate()];
    let mut eligibility = EligibilityStore::new();
    let quota = QuotaInventory::new();
    let mut lockouts = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
    let ledger = BudgetLedger::new();
    let mut runner = UsageReporter {
        ledger: &ledger,
        observed_before_dispatch: None,
        reported_usage: None,
    };

    let result = run(
        &IntentConstraints {
            task_class: "coding".into(),
            discovery_budget: 4,
            ..Default::default()
        },
        &candidates,
        &mut Stores {
            eligibility: &mut eligibility,
            quota: &quota,
            lockouts: &mut lockouts,
            ledger: &ledger,
        },
        FailoverBudget {
            max_attempts: 1,
            deadline_ms: Some(1_700_000_060_000),
            spend: SpendPolicy::PaidAuthorized {
                max_spend_micro: 500,
            },
            attempt_estimate_micros: 45,
            now_ms: 1_700_000_000_000,
        },
        &mut runner,
    );

    assert_eq!(result.winner, Some(0));
    assert_eq!(runner.observed_before_dispatch, Some(45));
    assert_eq!(
        ledger.account_exposure(""),
        45,
        "when provider reports no usage, conservative pre-reservation must remain committed"
    );
}

#[test]
fn vc_201_052_mastery_free_only_policy_rejects_paid_candidates_with_zero_exposure() {
    let candidates = vec![candidate()];
    let mut eligibility = EligibilityStore::new();
    let quota = QuotaInventory::new();
    let mut lockouts = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
    let ledger = BudgetLedger::new();
    let mut runner = UsageReporter {
        ledger: &ledger,
        observed_before_dispatch: None,
        reported_usage: Some(10),
    };

    let result = run(
        &IntentConstraints {
            task_class: "coding".into(),
            discovery_budget: 4,
            ..Default::default()
        },
        &candidates,
        &mut Stores {
            eligibility: &mut eligibility,
            quota: &quota,
            lockouts: &mut lockouts,
            ledger: &ledger,
        },
        FailoverBudget {
            max_attempts: 1,
            deadline_ms: Some(1_700_000_060_000),
            spend: SpendPolicy::FreeOnly,
            attempt_estimate_micros: 25,
            now_ms: 1_700_000_000_000,
        },
        &mut runner,
    );

    assert_eq!(result.winner, None);
    assert_eq!(runner.observed_before_dispatch, None);
    assert_eq!(ledger.account_exposure(""), 0);
}

#[test]
fn vc_201_052_mastery_empty_candidates_returns_no_winner_and_zero_exposure() {
    let candidates = Vec::new();
    let mut eligibility = EligibilityStore::new();
    let quota = QuotaInventory::new();
    let mut lockouts = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
    let ledger = BudgetLedger::new();
    let mut runner = UsageReporter {
        ledger: &ledger,
        observed_before_dispatch: None,
        reported_usage: Some(10),
    };

    let result = run(
        &IntentConstraints {
            task_class: "coding".into(),
            discovery_budget: 4,
            ..Default::default()
        },
        &candidates,
        &mut Stores {
            eligibility: &mut eligibility,
            quota: &quota,
            lockouts: &mut lockouts,
            ledger: &ledger,
        },
        FailoverBudget {
            max_attempts: 1,
            deadline_ms: Some(1_700_000_060_000),
            spend: SpendPolicy::PaidAuthorized {
                max_spend_micro: 500,
            },
            attempt_estimate_micros: 25,
            now_ms: 1_700_000_000_000,
        },
        &mut runner,
    );

    assert_eq!(result.winner, None);
    assert_eq!(runner.observed_before_dispatch, None);
    assert_eq!(ledger.account_exposure(""), 0);
}
