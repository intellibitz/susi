#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! VC-201-048: Make placement respect host thermal and power limits.
//!
//! Mastery criteria:
//! 1. Telemetry-driven pressure transitions: sustained thermal or power
//!    headroom violations transition host state from Fit to UnfitThermal
//!    or UnfitPower, advising cloud routing for eligible workloads.
//! 2. Explicit Unknown telemetry state: missing, unreadable, or failing
//!    sensor telemetry is explicitly classified as Unknown and never
//!    treated as Fit.
//! 3. Hysteresis recovery: returning below recovery temperature threshold
//!    requires sustained cool observations to prevent flutter.
//! 4. Local-only requirement preservation: under pressure, workloads requiring
//!    local residency (e.g. privacy or air-gap) are never rerouted to cloud;
//!    instead they are queued up to an operator limit and then throttled.

use susi_gemi::thermal_routing::{HostFitness, LocalWorkGate, ThermalRouter, ThermalSample};

#[test]
fn vc_201_048_mastery() {
    let mut router = ThermalRouter::new(85.0, 70.0, 2);

    // 1. Initial state is Fit
    assert_eq!(router.state(), HostFitness::Fit);
    assert!(router.local_ok_for_latency_sensitive());
    assert!(!router.prefer_cloud());

    // 2. Explicit Unknown telemetry state is never read as Fit
    assert_eq!(router.observe_unknown(), HostFitness::Unknown);
    assert!(!router.local_ok_for_latency_sensitive());
    assert!(
        router.prefer_cloud(),
        "Unknown telemetry must trigger conservative cloud advisory"
    );

    // 3. Telemetry recovery from Unknown to Fit on cool sample
    let cool_sample = ThermalSample {
        temp_c: 55.0,
        power_w: 60.0,
        power_cap_w: 250.0,
        util_pct: 30.0,
    };
    assert_eq!(router.observe(cool_sample), HostFitness::Fit);
    assert!(router.local_ok_for_latency_sensitive());
    assert!(!router.prefer_cloud());

    // 4. Single hot sample does not trigger Unfit immediately (sustained_needed = 2)
    let hot_sample = ThermalSample {
        temp_c: 88.0,
        power_w: 180.0,
        power_cap_w: 250.0,
        util_pct: 85.0,
    };
    assert_eq!(router.observe(hot_sample), HostFitness::Fit);

    // Sustained hot sample triggers UnfitThermal
    assert_eq!(router.observe(hot_sample), HostFitness::UnfitThermal);
    assert!(!router.local_ok_for_latency_sensitive());
    assert!(router.prefer_cloud());

    // 5. Recovery hysteresis: cool sample transitions to Recovering before Fit
    let mid_cool = ThermalSample {
        temp_c: 65.0,
        power_w: 70.0,
        power_cap_w: 250.0,
        util_pct: 40.0,
    };
    assert_eq!(router.observe(mid_cool), HostFitness::Recovering);
    assert_eq!(router.observe(mid_cool), HostFitness::Fit);

    // 6. Power cap violation triggers UnfitPower even if temperature is cool
    let power_capped_sample = ThermalSample {
        temp_c: 62.0,
        power_w: 248.0, // >= 98% of 250.0 cap
        power_cap_w: 250.0,
        util_pct: 95.0,
    };
    router.observe(power_capped_sample);
    assert_eq!(router.observe(power_capped_sample), HostFitness::UnfitPower);
    assert!(router.prefer_cloud());

    // 7. Local-only work gate: queue up to operator cap, then throttle (never reroute to cloud)
    let mut gate = LocalWorkGate::new(3);

    // Fit host admits work immediately without queueing
    let (run_now, queued) = gate.admit(HostFitness::Fit);
    assert!(run_now);
    assert_eq!(queued, 0);

    // Under pressure (UnfitThermal or UnfitPower), local-only work is queued up to cap
    let (run_now, queued) = gate.admit(HostFitness::UnfitThermal);
    assert!(!run_now);
    assert_eq!(queued, 1);

    let (run_now, queued) = gate.admit(HostFitness::UnfitPower);
    assert!(!run_now);
    assert_eq!(queued, 2);

    let (run_now, queued) = gate.admit(HostFitness::UnfitThermal);
    assert!(!run_now);
    assert_eq!(queued, 3);

    // Beyond operator cap (max_queued = 3): throttled, queue remains capped
    let (run_now, queued) = gate.admit(HostFitness::UnfitThermal);
    assert!(!run_now, "Work past limit is throttled/rejected");
    assert_eq!(queued, 3, "Queue depth is bounded by operator limit");

    // Once host recovers to Fit, backlog is drained and work executes
    let (run_now, queued) = gate.admit(HostFitness::Fit);
    assert!(run_now, "Host recovered; local work admitted");
    assert_eq!(queued, 0, "Backlog drained");
}
