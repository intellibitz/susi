use crate::rsi_generations::{next_action, Generation, MultiGenResult};

#[test]
fn vc_201_020_requires_three_generations_then_can_stop() {
    let hist = vec![
        Generation {
            index: 1,
            delta: 0.1,
            budget_remaining: 10.0,
            rejected: false,
        },
        Generation {
            index: 2,
            delta: 0.05,
            budget_remaining: 8.0,
            rejected: false,
        },
    ];
    assert_eq!(next_action(&hist, 3), MultiGenResult::Continue);
    let mut hist = hist;
    hist.push(Generation {
        index: 3,
        delta: 0.0,
        budget_remaining: 6.0,
        rejected: false,
    });
    assert_eq!(next_action(&hist, 3), MultiGenResult::StopNonImprovement);
}

#[test]
fn vc_201_020_regression_stops() {
    let hist = vec![
        Generation {
            index: 1,
            delta: 0.1,
            budget_remaining: 5.0,
            rejected: false,
        },
        Generation {
            index: 2,
            delta: 0.1,
            budget_remaining: 4.0,
            rejected: false,
        },
        Generation {
            index: 3,
            delta: -0.2,
            budget_remaining: 3.0,
            rejected: false,
        },
    ];
    assert_eq!(next_action(&hist, 3), MultiGenResult::StopRegression);
}

#[test]
fn vc_201_020_budget_exhaustion_stops_even_before_min_generations() {
    let hist = vec![Generation {
        index: 1,
        delta: 0.5,
        budget_remaining: 0.0,
        rejected: false,
    }];
    assert_eq!(next_action(&hist, 3), MultiGenResult::StopBudget);
}

#[test]
fn vc_201_020_rejected_stops_and_positive_delta_after_min_gens_continues() {
    let rejected_hist = vec![
        Generation {
            index: 1,
            delta: 0.2,
            budget_remaining: 5.0,
            rejected: false,
        },
        Generation {
            index: 2,
            delta: 0.3,
            budget_remaining: 4.0,
            rejected: true,
        },
    ];
    assert_eq!(
        next_action(&rejected_hist, 2),
        MultiGenResult::StopRegression
    );

    let improving_hist = vec![
        Generation {
            index: 1,
            delta: 0.2,
            budget_remaining: 5.0,
            rejected: false,
        },
        Generation {
            index: 2,
            delta: 0.1,
            budget_remaining: 4.0,
            rejected: false,
        },
    ];
    assert_eq!(next_action(&improving_hist, 2), MultiGenResult::Continue);
}
