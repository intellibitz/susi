//! Route local-vs-cloud placement by task class, budget, and Lagrangian
//! utility-constrained optimization over thermal state, throughput, and cache reuse.

use crate::thermal_routing::HostFitness;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Placement {
    Local,
    Cloud,
}

/// Place work locally for cheap/coding classes when budget is tight.
#[must_use]
pub fn place(task_class: &str, daily_budget_usd: f64) -> Placement {
    let cheap = matches!(
        task_class,
        "coding" | "summarize" | "classify" | "local_only"
    );
    if cheap || daily_budget_usd < 1.0 {
        Placement::Local
    } else {
        Placement::Cloud
    }
}

/// Multi-factor telemetry inputs for Lagrangian placement optimization.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlacementContext<'a> {
    pub task_class: &'a str,
    pub daily_budget_usd: f64,
    pub spent_today_usd: f64,
    pub expected_cloud_cost_usd: f64,
    pub local_tokens_per_sec: f32,
    pub host_fitness: HostFitness,
    pub local_only_policy: bool,
    pub local_meets_capability_floor: bool,
}

/// Explainable Lagrangian placement decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlacementOptimization {
    pub placement: Placement,
    /// Shadow price $\lambda_{\text{budget}} \ge 0$ of the remaining daily budget constraint.
    pub budget_shadow_price: f64,
    /// Net utility differential $U(\text{Cloud}) - U(\text{Local})$.
    pub cloud_utility_advantage: f64,
    pub reason: String,
}

/// Solve the constrained placement problem between `Local` and `Cloud`:
///
/// 1. Hard privacy constraint: `local_only_policy` or `task_class == "local_only"` $\implies$ `Local`.
/// 2. Hard budget exhaustion: remaining budget $\le 0$ or `< expected_cloud_cost_usd` $\implies$ `Local`.
/// 3. Otherwise, evaluates Lagrangian net utility:
///    $$\Delta U = \Delta Q_{\text{class}} + \text{ThermalPenalty}(\text{host}) + \text{SlowLocalPenalty}(tps) - \lambda_{\text{budget}} \cdot C_{\text{cloud}}$$
///    where $\lambda_{\text{budget}} = \frac{1}{\max(0.05, B_{\text{rem}})}$ rises hyperbolically as the daily budget depletes.
#[must_use]
pub fn optimize_placement(ctx: PlacementContext<'_>) -> PlacementOptimization {
    if ctx.local_only_policy || ctx.task_class == "local_only" {
        return PlacementOptimization {
            placement: Placement::Local,
            budget_shadow_price: f64::INFINITY,
            cloud_utility_advantage: f64::NEG_INFINITY,
            reason: "privacy/local_only constraint enforced".into(),
        };
    }

    let remaining_usd = (ctx.daily_budget_usd - ctx.spent_today_usd).max(0.0);
    if remaining_usd < ctx.expected_cloud_cost_usd.max(1e-6) || ctx.daily_budget_usd < 1.0 {
        return PlacementOptimization {
            placement: Placement::Local,
            budget_shadow_price: 100.0,
            cloud_utility_advantage: -1.0,
            reason: format!(
                "budget constraint active (remaining=${remaining_usd:.4}, daily=${:.2})",
                ctx.daily_budget_usd
            ),
        };
    }

    let budget_shadow_price = 1.0 / remaining_usd.max(0.05);
    let class_quality_gain = match ctx.task_class {
        "reasoning" | "estate_ops" | "research" => 0.45,
        "code" => 0.25,
        "coding" => -0.15,
        "chat" => 0.05,
        "reflex" | "summarize" | "classify" => -0.25,
        _ => 0.10,
    };
    let floor_urgency = if ctx.local_meets_capability_floor {
        0.0
    } else {
        0.60
    };
    let thermal_push = match ctx.host_fitness {
        HostFitness::Fit => 0.0,
        HostFitness::Recovering | HostFitness::Unknown => 0.25,
        HostFitness::UnfitThermal | HostFitness::UnfitPower => 0.65,
    };
    let slow_local_push = if ctx.local_tokens_per_sec > 0.0 && ctx.local_tokens_per_sec < 6.0 {
        f64::from((6.0 - ctx.local_tokens_per_sec) * 0.06)
    } else {
        0.0
    };

    let cost_drag = budget_shadow_price * ctx.expected_cloud_cost_usd.max(0.001) * 10.0;
    let cloud_utility_advantage =
        class_quality_gain + floor_urgency + thermal_push + slow_local_push - cost_drag;

    let placement = if cloud_utility_advantage > 0.0 {
        Placement::Cloud
    } else {
        Placement::Local
    };
    PlacementOptimization {
        placement,
        budget_shadow_price,
        cloud_utility_advantage,
        reason: format!(
            "Lagrangian advantage={cloud_utility_advantage:+.3} (shadow_price={budget_shadow_price:.3}, fitness={:?})",
            ctx.host_fitness
        ),
    }
}

#[cfg(test)]
mod placement_uses_task_class_tests {
    use super::*;

    #[test]
    fn placement_uses_task_class_and_budget() {
        assert_eq!(place("coding", 50.0), Placement::Local);
        assert_eq!(place("research", 0.5), Placement::Local);
        assert_eq!(place("research", 20.0), Placement::Cloud);
    }

    #[test]
    fn lagrangian_optimizer_routes_by_shadow_price_and_thermal_fitness() {
        // Healthy host + fast local GPU + cheap task -> Local
        let local_fit = optimize_placement(PlacementContext {
            task_class: "coding",
            daily_budget_usd: 20.0,
            spent_today_usd: 2.0,
            expected_cloud_cost_usd: 0.01,
            local_tokens_per_sec: 28.0,
            host_fitness: HostFitness::Fit,
            local_only_policy: false,
            local_meets_capability_floor: true,
        });
        assert_eq!(local_fit.placement, Placement::Local);

        // Thermally throttled host on the same coding task with healthy budget -> Cloud
        let thermal_cloud = optimize_placement(PlacementContext {
            host_fitness: HostFitness::UnfitThermal,
            ..PlacementContext {
                task_class: "coding",
                daily_budget_usd: 20.0,
                spent_today_usd: 2.0,
                expected_cloud_cost_usd: 0.01,
                local_tokens_per_sec: 3.0,
                host_fitness: HostFitness::UnfitThermal,
                local_only_policy: false,
                local_meets_capability_floor: true,
            }
        });
        assert_eq!(thermal_cloud.placement, Placement::Cloud);

        // Local-only privacy constraint never routes to cloud even when thermally throttled:
        let privacy_locked = optimize_placement(PlacementContext {
            local_only_policy: true,
            host_fitness: HostFitness::UnfitThermal,
            task_class: "research",
            daily_budget_usd: 100.0,
            spent_today_usd: 0.0,
            expected_cloud_cost_usd: 0.001,
            local_tokens_per_sec: 1.0,
            local_meets_capability_floor: false,
        });
        assert_eq!(privacy_locked.placement, Placement::Local);
    }

    #[test]
    fn optimize_placement_forces_local_when_budget_exhausted_or_below_floor() {
        let exhausted = optimize_placement(PlacementContext {
            task_class: "reasoning",
            daily_budget_usd: 10.0,
            spent_today_usd: 10.0,
            expected_cloud_cost_usd: 0.05,
            local_tokens_per_sec: 15.0,
            host_fitness: HostFitness::Fit,
            local_only_policy: false,
            local_meets_capability_floor: false,
        });
        assert_eq!(exhausted.placement, Placement::Local);
        assert!((exhausted.budget_shadow_price - 100.0).abs() < f64::EPSILON);

        let tiny_daily = optimize_placement(PlacementContext {
            daily_budget_usd: 0.50,
            spent_today_usd: 0.0,
            ..PlacementContext {
                task_class: "reasoning",
                daily_budget_usd: 0.50,
                spent_today_usd: 0.0,
                expected_cloud_cost_usd: 0.01,
                local_tokens_per_sec: 15.0,
                host_fitness: HostFitness::Fit,
                local_only_policy: false,
                local_meets_capability_floor: false,
            }
        });
        assert_eq!(tiny_daily.placement, Placement::Local);
        assert!((tiny_daily.budget_shadow_price - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn optimize_placement_routes_reasoning_below_capability_floor_to_cloud() {
        let decision = optimize_placement(PlacementContext {
            task_class: "reasoning",
            daily_budget_usd: 50.0,
            spent_today_usd: 0.0,
            expected_cloud_cost_usd: 0.01,
            local_tokens_per_sec: 20.0,
            host_fitness: HostFitness::Fit,
            local_only_policy: false,
            local_meets_capability_floor: false,
        });
        assert_eq!(decision.placement, Placement::Cloud);
        assert!(decision.cloud_utility_advantage > 0.0);
    }
}
