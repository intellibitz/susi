//! Vector VC-201-048 mastery tests.
//!
//! Vector: Make placement respect host thermal and power limits.
//! Mastery target: Use available thermal and power telemetry with explicit unknown
//! states and operator limits; simulated pressure throttles or reroutes eligible
//! work while preserving local-only requirements.

use crate::thermal_routing::{
    HostFitness, LocalWorkGate, OperatorThermalLimits, ThermalPlacement, ThermalRouter,
    ThermalSample,
};

#[test]
fn vc_201_048_mastery_thermal_and_power_pressure_transitions() {
    let mut router = ThermalRouter::new(85.0, 70.0, 2);
    assert_eq!(router.state(), HostFitness::Fit);
    assert!(router.local_ok_for_latency_sensitive());
    assert!(!router.prefer_cloud());

    // Single hot sample does not trigger unfit immediately (requires sustained_needed = 2)
    router.observe(ThermalSample {
        temp_c: 88.0,
        power_w: 120.0,
        power_cap_w: 250.0,
        util_pct: 85.0,
    });
    assert_eq!(router.state(), HostFitness::Fit);

    // Sustained hot sample triggers UnfitThermal
    router.observe(ThermalSample {
        temp_c: 89.0,
        power_w: 130.0,
        power_cap_w: 250.0,
        util_pct: 90.0,
    });
    assert_eq!(router.state(), HostFitness::UnfitThermal);
    assert!(router.prefer_cloud());
}

#[test]
fn vc_201_048_mastery_missing_unknown_telemetry_state_handling() {
    // Unreadable sysfs or missing sensor readings (0.0, negative, or NaN)
    // transition explicitly to HostFitness::Unknown rather than silently
    // reporting HostFitness::Fit.
    let zero_sample = ThermalSample {
        temp_c: 0.0,
        power_w: 0.0,
        power_cap_w: 0.0,
        util_pct: 0.0,
    };
    let mut router = ThermalRouter::new(85.0, 70.0, 1);
    assert_eq!(router.observe(zero_sample), HostFitness::Unknown);
    assert!(router.prefer_cloud());

    let nan_sample = ThermalSample {
        temp_c: f32::NAN,
        power_w: 40.0,
        power_cap_w: 100.0,
        util_pct: 20.0,
    };
    assert_eq!(router.observe(nan_sample), HostFitness::Unknown);
    assert!(router.prefer_cloud());
}

#[test]
fn vc_201_048_mastery_prefer_cloud_lacks_local_only_preservation() {
    let mut router = ThermalRouter::new(80.0, 65.0, 1);
    router.observe(ThermalSample {
        temp_c: 92.0,
        power_w: 180.0,
        power_cap_w: 200.0,
        util_pct: 95.0,
    });
    assert!(router.prefer_cloud());

    let mut gate = LocalWorkGate::new(2);
    // Cloud-eligible requests reroute under thermal pressure.
    assert_eq!(
        router.decide_placement(false, &mut gate),
        ThermalPlacement::RerouteCloud
    );
    // Local-only requests never reroute to cloud: they queue up to the gate
    // capacity and then throttle locally.
    assert_eq!(
        router.decide_placement(true, &mut gate),
        ThermalPlacement::QueueLocal { queued: 1 }
    );
    assert_eq!(
        router.decide_placement(true, &mut gate),
        ThermalPlacement::QueueLocal { queued: 2 }
    );
    assert_eq!(
        router.decide_placement(true, &mut gate),
        ThermalPlacement::ThrottleLocal { queued: 2 }
    );

    // Once the host cools to Fit, local-only work runs and drains the queue.
    router.observe(ThermalSample {
        temp_c: 55.0,
        power_w: 40.0,
        power_cap_w: 200.0,
        util_pct: 20.0,
    });
    assert_eq!(router.state(), HostFitness::Fit);
    assert_eq!(
        router.decide_placement(true, &mut gate),
        ThermalPlacement::RunLocal
    );
    assert_eq!(gate.queued(), 0);
}

#[test]
fn vc_201_048_mastery_operator_thermal_and_power_limits_override_defaults() {
    let mut router = ThermalRouter::new(85.0, 70.0, 1);
    router.set_operator_limits(OperatorThermalLimits {
        max_temp_c: Some(75.0),
        max_power_w: Some(120.0),
    });
    assert_eq!(
        router.operator_limits(),
        OperatorThermalLimits {
            max_temp_c: Some(75.0),
            max_power_w: Some(120.0),
        }
    );

    // 78C is below hardware 85C but above operator 75C limit -> UnfitThermal.
    assert_eq!(
        router.observe(ThermalSample {
            temp_c: 78.0,
            power_w: 60.0,
            power_cap_w: 250.0,
            util_pct: 50.0,
        }),
        HostFitness::UnfitThermal
    );

    // Cool down, then exceed operator 120W power limit even when hardware cap is 250W.
    assert_eq!(
        router.observe(ThermalSample {
            temp_c: 60.0,
            power_w: 60.0,
            power_cap_w: 250.0,
            util_pct: 30.0,
        }),
        HostFitness::Fit
    );
    assert_eq!(
        router.observe(ThermalSample {
            temp_c: 62.0,
            power_w: 119.0,
            power_cap_w: 250.0,
            util_pct: 70.0,
        }),
        HostFitness::UnfitPower
    );
}

/// Acceptance: unknown host telemetry is an explicit state that placement
/// never reads as fit, and the operator gate queues then throttles local-only
/// work under pressure instead of rerouting it to cloud.
#[test]
fn vc_201_048_thermal_router_unknown_telemetry_in_placement() {
    let mut router = ThermalRouter::new(85.0, 70.0, 2);

    // Unknown telemetry must not be read as "fit".
    assert_eq!(router.observe_unknown(), HostFitness::Unknown);
    assert!(!router.local_ok_for_latency_sensitive());
    assert!(router.prefer_cloud());

    // A real (cool) sample clears the unknown state and reports fit again.
    assert_eq!(
        router.observe(ThermalSample {
            temp_c: 60.0,
            power_w: 10.0,
            power_cap_w: 0.0,
            util_pct: 0.2,
        }),
        HostFitness::Fit
    );
    assert!(router.local_ok_for_latency_sensitive());

    // Operator gate: under pressure, local-only work is queued, then
    // throttled — never admitted.
    let mut gate = LocalWorkGate::new(2);
    let (run, q) = gate.admit(HostFitness::Fit);
    assert!(run);
    assert_eq!(q, 0);

    let (run, q) = gate.admit(HostFitness::UnfitThermal);
    assert!(!run);
    assert_eq!(q, 1);
    let (run, q) = gate.admit(HostFitness::UnfitThermal);
    assert!(!run);
    assert_eq!(q, 2);
    // Past the cap: throttled, queue stays at the limit.
    let (run, q) = gate.admit(HostFitness::UnfitThermal);
    assert!(!run);
    assert_eq!(q, 2);

    // Recovery drains the backlog and admits work again.
    let (run, q) = gate.admit(HostFitness::Fit);
    assert!(run);
    assert_eq!(q, 0);
}

#[test]
fn vc_201_048_mastery_local_work_gate_queues_on_unfit_power_and_unknown_telemetry() {
    let mut gate = LocalWorkGate::new(1);
    let (run, q) = gate.admit(HostFitness::UnfitPower);
    assert!(!run);
    assert_eq!(q, 1);

    let (run, q) = gate.admit(HostFitness::Unknown);
    assert!(!run);
    assert_eq!(q, 1);

    let (run, q) = gate.admit(HostFitness::Fit);
    assert!(run);
    assert_eq!(q, 0);
}
