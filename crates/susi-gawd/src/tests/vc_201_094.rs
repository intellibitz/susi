use crate::capacity_limits::{
    admit, saturation, AdmitLoad, CapacityError, CapacityLimits, ControlPlaneCapacity, LoadPoint,
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
