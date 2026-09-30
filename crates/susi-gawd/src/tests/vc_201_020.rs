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
