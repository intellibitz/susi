#![allow(missing_docs)] // integration test crate: no public API to document
#![allow(clippy::expect_used)] // deterministic fixture transitions are asserted below

use susi_gawd::capacity_limits::{
    admit, saturation_report, AdmitLoad, CapacityError, CapacityLimits, ControlPlaneCapacity,
    LoadPoint, SaturationReport, RECORDED_HARDWARE_PROFILE,
};

#[test]
fn vc_201_094_mastery() {
    assert_eq!(RECORDED_HARDWARE_PROFILE, "linux-x86_64-8cpu-32gb");
    let mut controller = ControlPlaneCapacity::new(CapacityLimits {
        max_missions: 2,
        max_streaming: 2,
        max_peer_repair: 1,
        max_model_churn: 1,
    });

    let mission = controller
        .reserve(LoadPoint {
            missions: 2,
            streaming: 0,
            peer_repair: 0,
            model_churn: 0,
        })
        .expect("two simultaneous missions fit");
    let stream = controller
        .reserve(LoadPoint {
            missions: 0,
            streaming: 2,
            peer_repair: 0,
            model_churn: 0,
        })
        .expect("two streaming calls fit");
    let repair = controller
        .reserve(LoadPoint {
            missions: 0,
            streaming: 0,
            peer_repair: 1,
            model_churn: 0,
        })
        .expect("one peer repair fits");
    let churn = controller
        .reserve(LoadPoint {
            missions: 0,
            streaming: 0,
            peer_repair: 0,
            model_churn: 1,
        })
        .expect("one model change fits");

    let overload = controller.reserve(LoadPoint {
        missions: 1,
        streaming: 0,
        peer_repair: 0,
        model_churn: 0,
    });
    assert!(
        matches!(&overload, Err(CapacityError::Overloaded { .. })),
        "saturation must reject before dispatch"
    );
    let report = match overload {
        Err(CapacityError::Overloaded { report }) => report,
        _ => return,
    };
    assert_eq!(report.saturated_on, Some("missions"));
    assert!(report.healthy && report.cancel_responsive);

    controller
        .release(mission)
        .expect("mission cancellation releases");
    let recovered = controller
        .reserve(LoadPoint {
            missions: 1,
            streaming: 0,
            peer_repair: 0,
            model_churn: 0,
        })
        .expect("released capacity is reusable");
    controller
        .release(recovered)
        .expect("release recovered work");
    controller.release(stream).expect("release stream");
    controller.release(repair).expect("release repair");
    controller.release(churn).expect("release model change");
    assert_eq!(controller.in_flight(), LoadPoint::default());
}

#[test]
fn vc_201_094_saturation_report_identifies_each_dimension_in_priority_order() {
    let limits = CapacityLimits {
        max_missions: 2,
        max_streaming: 3,
        max_peer_repair: 1,
        max_model_churn: 1,
    };

    let ok_load = LoadPoint {
        missions: 2,
        streaming: 3,
        peer_repair: 1,
        model_churn: 1,
    };
    assert_eq!(admit(&ok_load, &limits), AdmitLoad::Accept);
    assert_eq!(saturation_report(&ok_load, &limits).saturated_on, None);

    let stream_sat = LoadPoint {
        streaming: 4,
        ..ok_load
    };
    assert_eq!(admit(&stream_sat, &limits), AdmitLoad::RejectOverload);
    assert_eq!(
        saturation_report(&stream_sat, &limits).saturated_on,
        Some("streaming")
    );

    let repair_sat = LoadPoint {
        peer_repair: 2,
        ..ok_load
    };
    assert_eq!(
        saturation_report(&repair_sat, &limits).saturated_on,
        Some("peer_repair")
    );

    let churn_sat = LoadPoint {
        model_churn: 2,
        ..ok_load
    };
    assert_eq!(
        saturation_report(&churn_sat, &limits).saturated_on,
        Some("model_churn")
    );
}

#[test]
fn vc_201_094_counter_overflow_and_release_underflow_are_reported_cleanly() {
    let mut controller = ControlPlaneCapacity::new(CapacityLimits {
        max_missions: 4,
        max_streaming: 4,
        max_peer_repair: 2,
        max_model_churn: 2,
    });

    let res = controller
        .reserve(LoadPoint {
            missions: 1,
            streaming: 0,
            peer_repair: 0,
            model_churn: 0,
        })
        .expect("reserve 1 mission");

    assert_eq!(
        controller.reserve(LoadPoint {
            missions: usize::MAX,
            streaming: 0,
            peer_repair: 0,
            model_churn: 0,
        }),
        Err(CapacityError::CounterOverflow)
    );
    assert_eq!(controller.in_flight().missions, 1);

    controller.release(res).expect("first release succeeds");
    assert_eq!(
        controller.release(res),
        Err(CapacityError::ReleaseUnderflow)
    );
}

#[test]
fn vc_201_094_limits_accessor_and_serde_roundtrip() {
    let limits = CapacityLimits {
        max_missions: 8,
        max_streaming: 16,
        max_peer_repair: 4,
        max_model_churn: 2,
    };
    let controller = ControlPlaneCapacity::new(limits);
    assert_eq!(controller.limits(), limits);

    let json = serde_json::to_string(&limits).expect("serialize CapacityLimits");
    let decoded: CapacityLimits = serde_json::from_str(&json).expect("deserialize CapacityLimits");
    assert_eq!(decoded, limits);

    let report = SaturationReport {
        saturated_on: Some("missions"),
        healthy: true,
        cancel_responsive: true,
    };
    let rep_json = serde_json::to_string(&report).expect("serialize SaturationReport");
    assert!(rep_json.contains("missions"));
}

#[test]
fn vc_201_094_default_load_point_has_zero_in_flight_across_all_dimensions() {
    let pt = LoadPoint::default();
    assert_eq!(pt.missions, 0);
    assert_eq!(pt.streaming, 0);
    assert_eq!(pt.peer_repair, 0);
    assert_eq!(pt.model_churn, 0);
}
