use crate::capacity_limits::{
    admit, measure_control_plane_saturation, saturation, AdmitLoad, CapacityError, CapacityLimits,
    ControlPlaneCapacity, LoadPoint, RECORDED_HARDWARE_PROFILE,
};

#[test]
fn vc_201_094_rejects_overload_and_stays_healthy() {
    let limits = CapacityLimits {
        max_missions: 2,
        max_streaming: 4,
        max_peer_repair: 1,
        max_model_churn: 1,
    };
    assert_eq!(
        admit(
            &LoadPoint {
                missions: 1,
                streaming: 1,
                peer_repair: 0,
                model_churn: 0,
            },
            &limits
        ),
        AdmitLoad::Accept
    );
    let over = LoadPoint {
        missions: 5,
        streaming: 1,
        peer_repair: 0,
        model_churn: 0,
    };
    assert_eq!(admit(&over, &limits), AdmitLoad::RejectOverload);
    let sat = saturation(&over, &limits);
    assert_eq!(sat.saturated_on, Some("missions"));
    assert!(sat.healthy && sat.cancel_responsive);
}

#[test]
fn vc_201_094_controller_releases_a_cancelled_reservation() {
    let limits = CapacityLimits {
        max_missions: 1,
        max_streaming: 1,
        max_peer_repair: 1,
        max_model_churn: 1,
    };
    let mut controller = ControlPlaneCapacity::new(limits);
    let reservation = controller
        .reserve(LoadPoint {
            missions: 1,
            streaming: 0,
            peer_repair: 0,
            model_churn: 0,
        })
        .expect("within capacity");
    assert!(matches!(
        controller.reserve(LoadPoint {
            missions: 1,
            streaming: 0,
            peer_repair: 0,
            model_churn: 0,
        }),
        Err(CapacityError::Overloaded { .. })
    ));
    controller.release(reservation).expect("release once");
    assert_eq!(controller.in_flight(), LoadPoint::default());
}

#[test]
fn vc_201_094_saturation_identifies_streaming_peer_repair_and_model_churn() {
    let limits = CapacityLimits {
        max_missions: 2,
        max_streaming: 2,
        max_peer_repair: 1,
        max_model_churn: 1,
    };
    let s_sat = saturation(
        &LoadPoint {
            missions: 1,
            streaming: 3,
            peer_repair: 0,
            model_churn: 0,
        },
        &limits,
    );
    assert_eq!(s_sat.saturated_on, Some("streaming"));

    let p_sat = saturation(
        &LoadPoint {
            missions: 1,
            streaming: 1,
            peer_repair: 2,
            model_churn: 0,
        },
        &limits,
    );
    assert_eq!(p_sat.saturated_on, Some("peer_repair"));

    let m_sat = saturation(
        &LoadPoint {
            missions: 1,
            streaming: 1,
            peer_repair: 1,
            model_churn: 2,
        },
        &limits,
    );
    assert_eq!(m_sat.saturated_on, Some("model_churn"));
}

#[test]
fn vc_201_094_double_release_returns_underflow_without_corrupting_in_flight() {
    let limits = CapacityLimits {
        max_missions: 2,
        max_streaming: 2,
        max_peer_repair: 2,
        max_model_churn: 2,
    };
    let mut controller = ControlPlaneCapacity::new(limits);
    let res = controller
        .reserve(LoadPoint {
            missions: 1,
            streaming: 1,
            peer_repair: 0,
            model_churn: 0,
        })
        .unwrap();
    let dup = res;
    assert_eq!(controller.release(res), Ok(()));
    assert_eq!(
        controller.release(dup),
        Err(CapacityError::ReleaseUnderflow)
    );
    assert_eq!(controller.in_flight(), LoadPoint::default());
}

#[test]
fn vc_201_094_exact_limit_load_is_accepted_and_not_saturated() {
    let limits = CapacityLimits {
        max_missions: 3,
        max_streaming: 4,
        max_peer_repair: 2,
        max_model_churn: 1,
    };
    let exact = LoadPoint {
        missions: 3,
        streaming: 4,
        peer_repair: 2,
        model_churn: 1,
    };
    assert_eq!(admit(&exact, &limits), AdmitLoad::Accept);
    let sat = saturation(&exact, &limits);
    assert_eq!(sat.saturated_on, None);
    assert!(sat.healthy && sat.cancel_responsive);
}

#[test]
fn vc_201_094_load_tests_simultaneous_dimensions_and_measures_cancellation_responsiveness() {
    let limits = CapacityLimits {
        max_missions: 4,
        max_streaming: 8,
        max_peer_repair: 2,
        max_model_churn: 1,
    };
    let workload = [
        // Admitted wave 1: simultaneous missions + streaming
        LoadPoint {
            missions: 2,
            streaming: 4,
            peer_repair: 1,
            model_churn: 0,
        },
        // Admitted wave 2: reaches exact capacity across all 4 dimensions
        LoadPoint {
            missions: 2,
            streaming: 4,
            peer_repair: 1,
            model_churn: 1,
        },
        // Overloaded on missions
        LoadPoint {
            missions: 1,
            streaming: 0,
            peer_repair: 0,
            model_churn: 0,
        },
        // Overloaded on streaming
        LoadPoint {
            missions: 0,
            streaming: 1,
            peer_repair: 0,
            model_churn: 0,
        },
        // Overloaded on peer_repair
        LoadPoint {
            missions: 0,
            streaming: 0,
            peer_repair: 1,
            model_churn: 0,
        },
        // Overloaded on model_churn
        LoadPoint {
            missions: 0,
            streaming: 0,
            peer_repair: 0,
            model_churn: 1,
        },
    ];

    let report = measure_control_plane_saturation(limits, &workload, 1_000);
    assert_eq!(report.hardware_profile, RECORDED_HARDWARE_PROFILE);
    assert_eq!(report.admitted_count, 2);
    assert_eq!(report.rejected_overload_count, 4);
    assert!(report.healthy);
    assert!(report.cancel_responsive);
    assert!(report.measured_cancel_latency_ms <= 1_000);

    let saturated_dims: Vec<Option<&'static str>> = report
        .saturation_points
        .iter()
        .map(|s| s.saturated_on)
        .collect();
    assert_eq!(
        saturated_dims,
        vec![
            Some("missions"),
            Some("streaming"),
            Some("peer_repair"),
            Some("model_churn"),
        ]
    );
    assert!(report
        .saturation_points
        .iter()
        .all(|s| s.healthy && s.cancel_responsive));
}
