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

#[derive(Debug, Clone)]
pub struct ThermalRouter {
    pub temp_throttle_c: f32,
    pub recover_below_c: f32,
    pub sustained_needed: usize,
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
            hot_streak: 0,
            cool_streak: 0,
            state: HostFitness::Fit,
        }
    }

    pub fn observe(&mut self, sample: ThermalSample) -> HostFitness {
        // A real telemetry sample clears the unknown state.
        if matches!(self.state, HostFitness::Unknown) {
            self.state = HostFitness::Fit;
        }
        let power_capped = sample.power_cap_w > 0.0 && sample.power_w >= sample.power_cap_w * 0.98;
        let hot = sample.temp_c >= self.temp_throttle_c || power_capped;
        if hot {
            self.hot_streak = self.hot_streak.saturating_add(1);
            self.cool_streak = 0;
        } else if sample.temp_c <= self.recover_below_c && !power_capped {
            self.cool_streak = self.cool_streak.saturating_add(1);
            self.hot_streak = 0;
        } else {
            self.hot_streak = 0;
            self.cool_streak = 0;
        }

        if self.hot_streak >= self.sustained_needed {
            self.state = if power_capped && sample.temp_c < self.temp_throttle_c {
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
