use crate::thermal_routing::{HostFitness, ThermalRouter, ThermalSample};

#[test]
fn thermal_routing_marks_unfit_on_sustained_throttle() {
    let mut r = ThermalRouter::new(85.0, 70.0, 2);
    assert!(r.local_ok_for_latency_sensitive());
    r.observe(ThermalSample {
        temp_c: 90.0,
        power_w: 100.0,
        power_cap_w: 200.0,
        util_pct: 90.0,
    });
    assert_eq!(r.state(), HostFitness::Fit); // need sustained
    r.observe(ThermalSample {
        temp_c: 91.0,
        power_w: 100.0,
        power_cap_w: 200.0,
        util_pct: 90.0,
    });
    assert_eq!(r.state(), HostFitness::UnfitThermal);
    assert!(r.prefer_cloud());
}

#[test]
fn thermal_routing_recovers_when_temps_normalise() {
    let mut r = ThermalRouter::new(85.0, 70.0, 2);
    for _ in 0..2 {
        r.observe(ThermalSample {
            temp_c: 95.0,
            power_w: 150.0,
            power_cap_w: 200.0,
            util_pct: 99.0,
        });
    }
    assert!(r.prefer_cloud());
    r.observe(ThermalSample {
        temp_c: 65.0,
        power_w: 50.0,
        power_cap_w: 200.0,
        util_pct: 10.0,
    });
    assert_eq!(r.state(), HostFitness::Recovering);
    r.observe(ThermalSample {
        temp_c: 60.0,
        power_w: 40.0,
        power_cap_w: 200.0,
        util_pct: 5.0,
    });
    assert_eq!(r.state(), HostFitness::Fit);
    assert!(!r.prefer_cloud());
}

#[test]
fn thermal_routing_power_cap_marks_unfit_power() {
    let mut r = ThermalRouter::new(95.0, 70.0, 1);
    r.observe(ThermalSample {
        temp_c: 60.0,
        power_w: 200.0,
        power_cap_w: 200.0,
        util_pct: 80.0,
    });
    assert_eq!(r.state(), HostFitness::UnfitPower);
}

#[test]
fn thermal_routing_new_starts_fit_and_observe_unknown_clears_on_cool_sample() {
    let mut r = ThermalRouter::new(85.0, 70.0, 2);
    assert_eq!(r.state(), HostFitness::Fit);
    assert!(!r.prefer_cloud());
    assert!(r.local_ok_for_latency_sensitive());

    assert_eq!(r.observe_unknown(), HostFitness::Unknown);
    assert!(r.prefer_cloud());
    assert!(!r.local_ok_for_latency_sensitive());

    let after_cool = r.observe(ThermalSample {
        temp_c: 55.0,
        power_w: 40.0,
        power_cap_w: 200.0,
        util_pct: 15.0,
    });
    assert_eq!(after_cool, HostFitness::Fit);
    assert!(!r.prefer_cloud());
}

#[test]
fn thermal_routing_transient_hot_spike_below_sustained_threshold_keeps_host_fit() {
    let mut r = ThermalRouter::new(85.0, 70.0, 3);
    r.observe(ThermalSample {
        temp_c: 90.0,
        power_w: 100.0,
        power_cap_w: 200.0,
        util_pct: 80.0,
    });
    assert_eq!(r.state(), HostFitness::Fit);

    r.observe(ThermalSample {
        temp_c: 65.0,
        power_w: 60.0,
        power_cap_w: 200.0,
        util_pct: 30.0,
    });
    assert_eq!(r.state(), HostFitness::Fit);
    assert!(!r.prefer_cloud());
    assert!(r.local_ok_for_latency_sensitive());
}
