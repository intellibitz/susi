use crate::dev_canary::{dev_instance_ports, evaluate_canary, CanaryResult, CanaryTrial};

#[test]
fn vc_201_017_regression_stops_trial() {
    let t = CanaryTrial {
        candidate_metric: 0.5,
        baseline_metric: 0.9,
        regression_limit: 0.2,
    };
    assert_eq!(evaluate_canary(&t), CanaryResult::StopTrial);
}

#[test]
fn vc_201_017_within_limit_continues() {
    let t = CanaryTrial {
        candidate_metric: 0.85,
        baseline_metric: 0.9,
        regression_limit: 0.2,
    };
    assert_eq!(evaluate_canary(&t), CanaryResult::Continue);
}

#[test]
fn vc_201_017_dev_ports_are_9190_range() {
    assert_eq!(dev_instance_ports(), [9190, 9191, 9192, 9193, 9194]);
}

#[test]
fn vc_201_017_improved_candidate_continues_and_dev_ports_are_disjoint_from_prod() {
    let t = CanaryTrial {
        candidate_metric: 0.95,
        baseline_metric: 0.90,
        regression_limit: 0.05,
    };
    assert_eq!(evaluate_canary(&t), CanaryResult::Continue);
    for port in dev_instance_ports() {
        assert!(!(9090..=9094).contains(&port));
    }
}
