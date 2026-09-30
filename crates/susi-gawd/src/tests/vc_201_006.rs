use crate::paired_regression::{decide, PairedTrial, RegressionDecision};

#[test]
fn vc_201_006_undersampled_is_inconclusive() {
    let trials = vec![PairedTrial {
        baseline: 1.0,
        candidate: 2.0,
    }];
    assert_eq!(decide(&trials, 3, 0.95), RegressionDecision::Inconclusive);
}

#[test]
fn vc_201_006_clear_improvement_decides_improved() {
    let trials: Vec<_> = (0..8)
        .map(|_| PairedTrial {
            baseline: 1.0,
            candidate: 2.0,
        })
        .collect();
    assert_eq!(decide(&trials, 5, 0.95), RegressionDecision::Improved);
}

#[test]
fn vc_201_006_noisy_pair_is_inconclusive() {
    let trials = vec![
        PairedTrial {
            baseline: 1.0,
            candidate: 1.1,
        },
        PairedTrial {
            baseline: 1.0,
            candidate: 0.9,
        },
        PairedTrial {
            baseline: 1.0,
            candidate: 1.05,
        },
        PairedTrial {
            baseline: 1.0,
            candidate: 0.95,
        },
        PairedTrial {
            baseline: 1.0,
            candidate: 1.0,
        },
    ];
    assert_eq!(decide(&trials, 5, 0.95), RegressionDecision::Inconclusive);
}
