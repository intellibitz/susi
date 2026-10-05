#![allow(missing_docs)] // integration test crate: no public API to document
#![allow(clippy::expect_used)] // deterministic fixture transitions are asserted below

use susi_gawd::capacity_limits::{
    CapacityError, CapacityLimits, ControlPlaneCapacity, LoadPoint, RECORDED_HARDWARE_PROFILE,
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
