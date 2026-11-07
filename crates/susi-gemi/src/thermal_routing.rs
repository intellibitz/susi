//! Prefer cloud when the local host is thermally throttled or power-capped.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostFitness {
    Fit,
    UnfitThermal,
    UnfitPower,
    Recovering,
    /// Host telemetry is not available: placement must not silently assume
    /// the host is fit.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ThermalSample {
    pub temp_c: f32,
    pub power_w: f32,
    pub power_cap_w: f32,
    pub util_pct: f32,
}

impl ThermalSample {
    /// Verify that a thermal/power telemetry sample has finite, physically valid readings.
    #[must_use]
    pub fn verify_sample_invariants(&self) -> bool {
        self.temp_c.is_finite()
            && self.temp_c > 0.0
            && self.temp_c <= 150.0
            && self.power_w.is_finite()
            && self.power_w >= 0.0
            && self.power_cap_w.is_finite()
            && self.power_cap_w >= 0.0
            && self.util_pct.is_finite()
            && (0.0..=100.0).contains(&self.util_pct)
    }
}

/// Operator-configured thermal and power ceilings that tighten (never loosen)
/// the host's default throttle thresholds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct OperatorThermalLimits {
    pub max_temp_c: Option<f32>,
    pub max_power_w: Option<f32>,
}

/// Placement decision under host thermal/power telemetry that preserves
/// `local_only` constraints instead of leaking restricted work to cloud.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThermalPlacement {
    RunLocal,
    RerouteCloud,
    QueueLocal { queued: usize },
    ThrottleLocal { queued: usize },
}

#[derive(Debug, Clone)]
pub struct ThermalRouter {
    pub temp_throttle_c: f32,
    pub recover_below_c: f32,
    pub sustained_needed: usize,
    operator_limits: OperatorThermalLimits,
    hot_streak: usize,
    cool_streak: usize,
    state: HostFitness,
}

impl ThermalRouter {
    #[must_use]
    pub fn new(temp_throttle_c: f32, recover_below_c: f32, sustained_needed: usize) -> Self {
        Self {
            temp_throttle_c,
            recover_below_c,
            sustained_needed: sustained_needed.max(1),
            operator_limits: OperatorThermalLimits::default(),
            hot_streak: 0,
            cool_streak: 0,
            state: HostFitness::Fit,
        }
    }

    pub fn set_operator_limits(&mut self, limits: OperatorThermalLimits) {
        self.operator_limits = limits;
    }

    #[must_use]
    pub fn operator_limits(&self) -> OperatorThermalLimits {
        self.operator_limits
    }

    fn effective_temp_throttle_c(&self) -> f32 {
        self.operator_limits
            .max_temp_c
            .filter(|t| t.is_finite() && *t > 0.0)
            .map_or(self.temp_throttle_c, |op| self.temp_throttle_c.min(op))
    }

    fn effective_power_cap_w(&self, sample_cap_w: f32) -> f32 {
        let op_cap = self
            .operator_limits
            .max_power_w
            .filter(|p| p.is_finite() && *p > 0.0);
        match (sample_cap_w > 0.0, op_cap) {
            (true, Some(op)) => sample_cap_w.min(op),
            (true, None) => sample_cap_w,
            (false, Some(op)) => op,
            (false, None) => 0.0,
        }
    }

    pub fn observe(&mut self, sample: ThermalSample) -> HostFitness {
        if !sample.temp_c.is_finite()
            || !sample.power_w.is_finite()
            || !sample.power_cap_w.is_finite()
            || !sample.util_pct.is_finite()
            || sample.temp_c <= 0.0
            || sample.power_w < 0.0
            || sample.power_cap_w < 0.0
            || sample.util_pct < 0.0
        {
            return self.observe_unknown();
        }
        // A real telemetry sample clears the unknown state.
        if matches!(self.state, HostFitness::Unknown) {
            self.state = HostFitness::Fit;
        }
        let temp_limit = self.effective_temp_throttle_c();
        let recover_limit = self.recover_below_c.min(temp_limit);
        let effective_cap_w = self.effective_power_cap_w(sample.power_cap_w);
        let power_capped = effective_cap_w > 0.0 && sample.power_w >= effective_cap_w * 0.98;
        let hot = sample.temp_c >= temp_limit || power_capped;
        if hot {
            self.hot_streak = self.hot_streak.saturating_add(1);
            self.cool_streak = 0;
        } else if sample.temp_c <= recover_limit && !power_capped {
            self.cool_streak = self.cool_streak.saturating_add(1);
            self.hot_streak = 0;
        } else {
            self.hot_streak = 0;
            self.cool_streak = 0;
        }

        if self.hot_streak >= self.sustained_needed {
            self.state = if power_capped && sample.temp_c < temp_limit {
                HostFitness::UnfitPower
            } else {
                HostFitness::UnfitThermal
            };
        } else if matches!(
            self.state,
            HostFitness::UnfitThermal | HostFitness::UnfitPower | HostFitness::Recovering
        ) {
            if self.cool_streak >= self.sustained_needed {
                self.state = HostFitness::Fit;
            } else {
                self.state = HostFitness::Recovering;
            }
        }
        self.state
    }

    /// Decide placement for a workload while honouring `local_only`
    /// constraints. Eligible work reroutes to cloud when the host is not fit;
    /// `local_only` work queues up to the operator gate limit and then
    /// throttles locally — never rerouting to cloud.
    pub fn decide_placement(&self, local_only: bool, gate: &mut LocalWorkGate) -> ThermalPlacement {
        if matches!(self.state, HostFitness::Fit) {
            let _ = gate.admit(self.state);
            return ThermalPlacement::RunLocal;
        }
        if !local_only {
            return ThermalPlacement::RerouteCloud;
        }
        let prev = gate.queued();
        let (run, queued) = gate.admit(self.state);
        if run {
            ThermalPlacement::RunLocal
        } else if queued > prev {
            ThermalPlacement::QueueLocal { queued }
        } else {
            ThermalPlacement::ThrottleLocal { queued }
        }
    }

    #[must_use]
    pub fn local_ok_for_latency_sensitive(&self) -> bool {
        matches!(self.state, HostFitness::Fit)
    }

    #[must_use]
    pub fn prefer_cloud(&self) -> bool {
        !self.local_ok_for_latency_sensitive()
    }

    /// Host telemetry is unknown (sensor absent or unreadable): the router
    /// records an explicit `Unknown` state instead of silently assuming the
    /// host is fit.
    pub fn observe_unknown(&mut self) -> HostFitness {
        self.hot_streak = 0;
        self.cool_streak = 0;
        self.state = HostFitness::Unknown;
        self.state
    }

    #[must_use]
    pub fn state(&self) -> HostFitness {
        self.state
    }

    /// Verify structural and hysteresis invariants of the thermal router.
    #[must_use]
    pub fn verify_router_invariants(&self) -> bool {
        self.temp_throttle_c.is_finite()
            && self.temp_throttle_c > 0.0
            && self.recover_below_c.is_finite()
            && self.recover_below_c > 0.0
            && self.recover_below_c <= self.temp_throttle_c
            && self.sustained_needed >= 1
            && !(self.hot_streak > 0 && self.cool_streak > 0)
    }
}

/// An operator limit on local-only work. Under thermal/power pressure local
/// work is queued rather than rerouted to cloud, and past the cap it is
/// throttled (rejected) — never silently handed to the cloud.
#[derive(Debug, Clone)]
pub struct LocalWorkGate {
    max_queued: usize,
    queued: usize,
}

impl LocalWorkGate {
    #[must_use]
    pub fn new(max_queued: usize) -> Self {
        Self {
            max_queued,
            queued: 0,
        }
    }

    /// Whether local-only work may run now. A fit host runs (and drains any
    /// backlog); a non-fit host queues the work up to `max_queued`, then
    /// throttles. Returns `(run_now, queued_count)`.
    pub fn admit(&mut self, fitness: HostFitness) -> (bool, usize) {
        if matches!(fitness, HostFitness::Fit) {
            self.queued = 0;
            (true, 0)
        } else if self.queued < self.max_queued {
            self.queued += 1;
            (false, self.queued)
        } else {
            (false, self.queued)
        }
    }

    #[must_use]
    pub fn queued(&self) -> usize {
        self.queued
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    #[test]
    fn thermal_router_sustained_heat_transitions_to_unfit_thermal_and_recovers() {
        let mut router = ThermalRouter::new(85.0, 75.0, 2);
        assert_eq!(router.state(), HostFitness::Fit);
        assert!(router.local_ok_for_latency_sensitive());

        let hot = ThermalSample {
            temp_c: 88.0,
            power_w: 50.0,
            power_cap_w: 100.0,
            util_pct: 90.0,
        };
        assert_eq!(router.observe(hot), HostFitness::Fit);
        assert_eq!(router.observe(hot), HostFitness::UnfitThermal);
        assert!(router.prefer_cloud());

        let cool = ThermalSample {
            temp_c: 70.0,
            power_w: 30.0,
            power_cap_w: 100.0,
            util_pct: 20.0,
        };
        assert_eq!(router.observe(cool), HostFitness::Recovering);
        assert_eq!(router.observe(cool), HostFitness::Fit);
        assert!(!router.prefer_cloud());
    }

    #[test]
    fn thermal_router_power_cap_saturation_transitions_to_unfit_power() {
        let mut router = ThermalRouter::new(85.0, 75.0, 2);
        let capped = ThermalSample {
            temp_c: 65.0,
            power_w: 99.0,
            power_cap_w: 100.0,
            util_pct: 95.0,
        };
        assert_eq!(router.observe(capped), HostFitness::Fit);
        assert_eq!(router.observe(capped), HostFitness::UnfitPower);
        assert!(router.prefer_cloud());
    }

    #[test]
    fn thermal_router_observe_unknown_prefers_cloud_until_real_sample() {
        let mut router = ThermalRouter::new(85.0, 75.0, 1);
        assert_eq!(router.observe_unknown(), HostFitness::Unknown);
        assert!(router.prefer_cloud());

        let normal = ThermalSample {
            temp_c: 60.0,
            power_w: 25.0,
            power_cap_w: 100.0,
            util_pct: 15.0,
        };
        assert_eq!(router.observe(normal), HostFitness::Fit);
        assert!(router.local_ok_for_latency_sensitive());
    }

    #[test]
    fn local_work_gate_queues_under_pressure_and_drains_when_fit() {
        let mut gate = LocalWorkGate::new(2);
        assert_eq!(gate.queued(), 0);
        assert_eq!(gate.admit(HostFitness::UnfitThermal), (false, 1));
        assert_eq!(gate.admit(HostFitness::UnfitPower), (false, 2));
        assert_eq!(gate.admit(HostFitness::Recovering), (false, 2));
        assert_eq!(gate.admit(HostFitness::Fit), (true, 0));
        assert_eq!(gate.queued(), 0);
    }

    #[test]
    fn local_work_gate_zero_capacity_never_queues_under_pressure() {
        let router = ThermalRouter::new(85.0, 75.0, 0);
        assert_eq!(router.sustained_needed, 1);

        let mut gate = LocalWorkGate::new(0);
        assert_eq!(gate.admit(HostFitness::UnfitThermal), (false, 0));
        assert_eq!(gate.admit(HostFitness::Unknown), (false, 0));
        assert_eq!(gate.queued(), 0);
        assert_eq!(gate.admit(HostFitness::Fit), (true, 0));
    }

    #[test]
    fn verify_sample_and_router_invariants_validate_thermal_routing() {
        let sample = ThermalSample {
            temp_c: 65.0,
            power_w: 45.0,
            power_cap_w: 100.0,
            util_pct: 50.0,
        };
        assert!(sample.verify_sample_invariants());
        assert!(!ThermalSample {
            temp_c: -5.0,
            ..sample
        }
        .verify_sample_invariants());
        assert!(!ThermalSample {
            util_pct: 120.0,
            ..sample
        }
        .verify_sample_invariants());

        let mut router = ThermalRouter::new(85.0, 75.0, 2);
        assert!(router.verify_router_invariants());
        let _ = router.observe(sample);
        assert!(router.verify_router_invariants());

        let inverted = ThermalRouter::new(70.0, 80.0, 2);
        assert!(!inverted.verify_router_invariants());
    }
}
