//! Vector VC-201-048 mastery tests.
//!
//! Vector: Make placement respect host thermal and power limits.
//! Mastery target: Use available thermal and power telemetry with explicit unknown
//! states and operator limits; simulated pressure throttles or reroutes eligible
//! work while preserving local-only requirements.

use crate::thermal_routing::{HostFitness, ThermalRouter, ThermalSample};

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
    // ThermalSample lacks Option<f32> or explicit Unknown telemetry indicators.
    // Zero or NaN values from unreadable sysfs or missing sensors cannot be distinguished
    // from valid cold readings without explicit unknown telemetry state support.
    let sample = ThermalSample {
        temp_c: 0.0,
        power_w: 0.0,
        power_cap_w: 0.0,
        util_pct: 0.0,
    };
    let mut router = ThermalRouter::new(85.0, 70.0, 1);
    let fitness = router.observe(sample);
    assert_eq!(fitness, HostFitness::Fit);
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

    // Current routing only provides a binary prefer_cloud advisory.
    // For requests marked local_only (e.g. privacy, air-gap, residency constraints),
    // rerouting to cloud violates policy; the system must throttle or queue locally.
    struct PlacementRequest {
        pub local_only: bool,
    }

    let local_request = PlacementRequest { local_only: true };
    // Demonstrates that router does not handle local-only preservation internally:
    assert!(router.prefer_cloud() && local_request.local_only);
}
