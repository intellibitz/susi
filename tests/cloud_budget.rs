#![allow(missing_docs)] // Integration test crate: the fixture has no public API.
#![allow(clippy::expect_used)] // Test joins and reservations are required assertions.

use std::sync::Arc;

use susi_gawd_agents::cloud_budget::{
    BudgetLedger, Denial, FreeHeadroom, ReserveRequest, SpendPolicy,
};

fn request<'a>(
    account: &'a str,
    credential_fp: &'a str,
    policy: SpendPolicy,
    estimate: u64,
) -> ReserveRequest<'a> {
    ReserveRequest {
        account,
        credential_fp,
        model: "test-model",
        policy,
        is_free_candidate: false,
        free_headroom: FreeHeadroom::Exhausted,
        estimate,
    }
}

#[test]
fn cloud_budget() {
    let policy = SpendPolicy::PaidAuthorized {
        max_spend_micro: 1_000,
    };

    let ledger = Arc::new(BudgetLedger::new());
    let mut workers = Vec::new();
    for _ in 0..10 {
        let ledger = Arc::clone(&ledger);
        workers.push(std::thread::spawn(move || {
            ledger
                .reserve(request("shared", "worker", policy, 150))
                .ok()
        }));
    }
    let wins = workers
        .into_iter()
        .filter_map(|worker| worker.join().expect("budget worker must join"))
        .collect::<Vec<_>>();
    assert_eq!(wins.len(), 6);
    assert_eq!(ledger.account_exposure("shared"), 900);

    let exhausted = BudgetLedger::new();
    assert_eq!(
        exhausted
            .reserve(ReserveRequest {
                account: "free-only",
                credential_fp: "key",
                model: "test-model",
                policy: SpendPolicy::FreeOnly,
                is_free_candidate: false,
                free_headroom: FreeHeadroom::Exhausted,
                estimate: 1,
            })
            .expect_err("exhausted free-only budget must refuse paid work"),
        Denial::FreeExhausted
    );

    let uncertain = BudgetLedger::new();
    let unknown_price = uncertain
        .reserve(request("uncertain", "key", policy, 0))
        .expect("unknown price must reserve the remaining bounded cap");
    uncertain.commit(unknown_price, None);
    assert_eq!(uncertain.account_exposure("uncertain"), 1_000);

    let rotated = BudgetLedger::new();
    let first_key = rotated
        .reserve(request("account", "key-a", policy, 600))
        .expect("first account key should reserve");
    assert!(matches!(
        rotated.reserve(request("account", "key-b", policy, 600)),
        Err(Denial::OverBudget { .. })
    ));
    rotated.release(first_key);
    assert!(rotated
        .reserve(request("account", "key-b", policy, 600))
        .is_ok());
}
