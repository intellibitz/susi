//! Prefer cloud when the local host is thermally throttled or power-capped.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostFitness {
    Fit,
    UnfitThermal,
    UnfitPower,
    Recovering,
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

    #[must_use]
    pub fn state(&self) -> HostFitness {
        self.state
    }
}
