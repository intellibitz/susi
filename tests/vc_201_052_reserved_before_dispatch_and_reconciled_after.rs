#![allow(missing_docs)] // integration test crate: no public API to document

use std::collections::BTreeMap;

use susi_gawd_agents::cloud_budget::{BudgetLedger, SpendPolicy};
use susi_gawd_agents::cloud_intent::{Candidate, IntentConstraints};
use susi_gawd_swarm::cloud_failover::{run, AttemptOutcome, FailoverBudget, Runner, Stores};
use susi_gawd_swarm::cloud_lockout::LockoutTracker;
use susi_vendor_models::cloud_eligibility::{EligibilityStore, InferenceResult, Subject};
use susi_vendor_models::cloud_quota::QuotaInventory;

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
fn vc_201_052_reserved_before_dispatch_and_reconciled_after() {
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
    let mut lockouts =
        LockoutTracker::new(Default::default(), susi_gawd_swarm::cloud_lockout::now_unix);
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
    assert_eq!(runner.observed_before_dispatch, Some(50));
    assert_eq!(ledger.account_exposure(""), 7);
}
