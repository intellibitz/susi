use crate::experiment_budget::{BudgetLimits, BudgetUsage, ExperimentBudget};

#[test]
fn vc_201_014_reserve_and_debit_across_retries() {
    let mut b = ExperimentBudget::new(
        "e1",
        BudgetLimits {
            tokens: 100,
            wall_ms: 1_000,
            subprocesses: 3,
            cloud_spend_micros: 50_000,
        },
    );
    b.reserve(BudgetUsage {
        tokens: 40,
        wall_ms: 100,
        subprocesses: 1,
        cloud_spend_micros: 10_000,
    })
    .unwrap();
    // retry / descendant
    b.debit(BudgetUsage {
        tokens: 40,
        wall_ms: 100,
        subprocesses: 1,
        cloud_spend_micros: 10_000,
    })
    .unwrap();
    assert!(b.allows_new_work());
    assert_eq!(b.usage.tokens, 80);
}

#[test]
fn vc_201_014_exhaustion_stops_new_work_and_cancels_children() {
    let mut b = ExperimentBudget::new(
        "e2",
        BudgetLimits {
            tokens: 50,
            wall_ms: 500,
            subprocesses: 1,
            cloud_spend_micros: 1_000,
        },
    );
    b.reserve(BudgetUsage {
        tokens: 40,
        wall_ms: 0,
        subprocesses: 0,
        cloud_spend_micros: 0,
    })
    .unwrap();
    let err = b
        .reserve(BudgetUsage {
            tokens: 20,
            wall_ms: 0,
            subprocesses: 0,
            cloud_spend_micros: 0,
        })
        .unwrap_err();
    assert!(err.contains("exhausted"));
    assert!(!b.allows_new_work());
    b.cancel_child("child-a").unwrap();
    assert!(b.cancelled_children.contains("child-a"));
    assert!(b.reserve(BudgetUsage::default()).is_err());
}
