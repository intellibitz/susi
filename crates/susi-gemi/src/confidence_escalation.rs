//! Confidence-calibrated escalation via Expected Value of Information (EVOI)
//! and Wald's Sequential Probability Ratio Test (SPRT).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Escalation {
    pub escalate: bool,
    pub reason: String,
}

/// Escalate when the cheap judge confidence is below threshold.
#[must_use]
pub fn confidence_escalate(judge_confidence: f64, threshold: f64) -> Escalation {
    if judge_confidence < threshold {
        Escalation {
            escalate: true,
            reason: format!("confidence {judge_confidence:.2} < {threshold:.2}"),
        }
    } else {
        Escalation {
            escalate: false,
            reason: "confidence sufficient".into(),
        }
    }
}

/// Decision-theoretic inputs for Expected Value of Information (EVOI) escalation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EvoiEscalationInput {
    /// Calibrated success probability of the current (cheap/local) model $p_{\text{cheap}} \in [0, 1]$.
    pub cheap_confidence: f64,
    /// Empirical Beta-smoothed success rate of the target frontier model $p_{\text{frontier}} \in [0, 1]$.
    pub frontier_reliability: f64,
    /// Marginal USD cost of the cheap/local attempt (0.0 for local/subscription).
    pub cheap_cost_usd: f64,
    /// Marginal USD cost of escalating to the frontier model.
    pub frontier_cost_usd: f64,
    /// Economic value of a verified successful task outcome in USD.
    pub task_value_usd: f64,
    /// Hard safety floor below which we always escalate regardless of cost.
    pub safety_floor: f64,
}

/// Decision-theoretic output of [`evoi_escalate`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvoiDecision {
    pub escalate: bool,
    /// Net Expected Value of Information in USD:
    /// $\text{EVOI} = (p_{\text{frontier}} - p_{\text{cheap}}) \cdot V_{\text{task}} - (C_{\text{frontier}} - C_{\text{cheap}})$.
    pub evoi_usd: f64,
    /// Risk-calibrated indifferent confidence threshold $\tau^* = p_{\text{frontier}} - \Delta C / V_{\text{task}}$.
    pub optimal_threshold: f64,
    pub reason: String,
}

/// Compute whether escalating from a cheap/local tier to a frontier tier has
/// positive Expected Value of Information (EVOI) or breaches the safety floor.
#[must_use]
pub fn evoi_escalate(input: EvoiEscalationInput) -> EvoiDecision {
    let p_cheap = input.cheap_confidence.clamp(0.0, 1.0);
    let p_front = input.frontier_reliability.clamp(0.0, 1.0);
    let delta_cost = (input.frontier_cost_usd - input.cheap_cost_usd).max(0.0);
    let task_val = input.task_value_usd.max(1e-6);
    let optimal_threshold = (p_front - (delta_cost / task_val)).clamp(0.0, 1.0);
    let evoi_usd = (p_front - p_cheap) * task_val - delta_cost;

    if p_cheap < input.safety_floor {
        EvoiDecision {
            escalate: true,
            evoi_usd,
            optimal_threshold,
            reason: format!(
                "confidence {p_cheap:.3} below safety floor {:.3} (EVOI=${evoi_usd:.4})",
                input.safety_floor
            ),
        }
    } else if evoi_usd > 0.0 && p_front > p_cheap {
        EvoiDecision {
            escalate: true,
            evoi_usd,
            optimal_threshold,
            reason: format!(
                "positive EVOI +${evoi_usd:.4} (confidence {p_cheap:.3} < optimal threshold {optimal_threshold:.3})"
            ),
        }
    } else {
        EvoiDecision {
            escalate: false,
            evoi_usd,
            optimal_threshold,
            reason: format!(
                "local tier optimal (confidence {p_cheap:.3} >= {optimal_threshold:.3}, EVOI=${evoi_usd:.4})"
            ),
        }
    }
}

/// Verdict of Wald's Sequential Probability Ratio Test (SPRT) at step $t$.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SprtVerdict {
    /// Null hypothesis $H_0$ accepted ($p \ge p_0$): cheap/local generation is reliable.
    AcceptLocal,
    /// Alternative hypothesis $H_1$ accepted ($p \le p_1$): abort stream and escalate immediately.
    EscalateNow,
    /// Evidence stays between boundaries $(A, B)$: continue observing stream.
    Continue,
}

/// Wald's Sequential Probability Ratio Test (SPRT) for optimal early-stopping
/// escalation over streaming token confidences or multi-step verifiers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SprtEscalator {
    /// Target acceptable token/step reliability under $H_0$ (e.g. `0.85`).
    pub p0_healthy: f64,
    /// Degraded reliability under $H_1$ that warrants escalation (e.g. `0.55`).
    pub p1_degraded: f64,
    /// Lower stopping boundary $A = \ln(\beta / (1 - \alpha))$ (accept local).
    pub lower_bound_a: f64,
    /// Upper stopping boundary $B = \ln((1 - \beta) / \alpha)$ (escalate now).
    pub upper_bound_b: f64,
    /// Cumulative log-likelihood ratio $\Lambda_t = \sum_{i=1}^t \ln \frac{P(x_i \mid H_1)}{P(x_i \mid H_0)}$.
    pub log_likelihood_ratio: f64,
    pub observations: u32,
}

impl SprtEscalator {
    /// Construct a Wald SPRT escalator with Type-I error `alpha` (false escalation)
    /// and Type-II error `beta` (missed escalation).
    #[must_use]
    pub fn new(p0_healthy: f64, p1_degraded: f64, alpha: f64, beta: f64) -> Self {
        let p0 = p0_healthy.clamp(0.51, 0.999);
        let p1 = p1_degraded.clamp(0.001, p0 - 0.05);
        let a = alpha.clamp(1e-4, 0.49);
        let b = beta.clamp(1e-4, 0.49);
        Self {
            p0_healthy: p0,
            p1_degraded: p1,
            lower_bound_a: (b / (1.0 - a)).ln(),
            upper_bound_b: ((1.0 - b) / a).ln(),
            log_likelihood_ratio: 0.0,
            observations: 0,
        }
    }

    /// Observe a continuous token/step quality signal $q \in [0, 1]$ (where $1.0$
    /// is high confidence and $0.0$ is low confidence) and return the optimal
    /// stopping decision.
    pub fn observe(&mut self, step_quality: f64) -> SprtVerdict {
        let q = step_quality.clamp(0.0, 1.0);
        // Expected Bernoulli log-likelihood increment weighted by fractional quality q:
        let llr_success = (self.p1_degraded / self.p0_healthy).ln();
        let llr_failure = ((1.0 - self.p1_degraded) / (1.0 - self.p0_healthy)).ln();
        let step_llr = q * llr_success + (1.0 - q) * llr_failure;
        self.log_likelihood_ratio += step_llr;
        self.observations = self.observations.saturating_add(1);
        self.verdict()
    }

    #[must_use]
    pub fn verdict(&self) -> SprtVerdict {
        if self.log_likelihood_ratio >= self.upper_bound_b {
            SprtVerdict::EscalateNow
        } else if self.log_likelihood_ratio <= self.lower_bound_a {
            SprtVerdict::AcceptLocal
        } else {
            SprtVerdict::Continue
        }
    }
}

#[cfg(test)]
mod confidence_escalation_tests {
    use super::*;

    #[test]
    fn confidence_escalation_triggers_below_threshold() {
        assert!(confidence_escalate(0.4, 0.7).escalate);
        assert!(!confidence_escalate(0.9, 0.7).escalate);
    }

    #[test]
    fn evoi_escalates_only_when_information_gain_exceeds_marginal_cost() {
        // High-value coding task ($2.00 value) where frontier costs $0.02 and lifts
        // success probability from 0.65 to 0.95 -> EVOI = +$0.58 -> escalate!
        let pos = evoi_escalate(EvoiEscalationInput {
            cheap_confidence: 0.65,
            frontier_reliability: 0.95,
            cheap_cost_usd: 0.0,
            frontier_cost_usd: 0.02,
            task_value_usd: 2.0,
            safety_floor: 0.35,
        });
        assert!(pos.escalate);
        assert!(pos.evoi_usd > 0.5);

        // Low-value reflex task ($0.01 value) where frontier costs $0.05 -> negative EVOI
        let neg = evoi_escalate(EvoiEscalationInput {
            cheap_confidence: 0.75,
            frontier_reliability: 0.92,
            cheap_cost_usd: 0.0,
            frontier_cost_usd: 0.05,
            task_value_usd: 0.01,
            safety_floor: 0.35,
        });
        assert!(!neg.escalate);
        assert!(neg.evoi_usd < 0.0);
    }

    #[test]
    fn wald_sprt_stops_early_on_degraded_or_healthy_streams() {
        let mut sprt = SprtEscalator::new(0.85, 0.45, 0.05, 0.05);
        assert_eq!(sprt.verdict(), SprtVerdict::Continue);
        let mut escalated = false;
        for _ in 0..8 {
            if sprt.observe(0.15) == SprtVerdict::EscalateNow {
                escalated = true;
                break;
            }
        }
        assert!(
            escalated,
            "low-quality stream must cross upper Wald boundary B"
        );

        let mut healthy = SprtEscalator::new(0.85, 0.45, 0.05, 0.05);
        let mut accepted = false;
        for _ in 0..10 {
            if healthy.observe(0.98) == SprtVerdict::AcceptLocal {
                accepted = true;
                break;
            }
        }
        assert!(
            accepted,
            "high-quality stream must cross lower Wald boundary A"
        );
    }

    #[test]
    fn confidence_and_evoi_safety_floor_boundary_semantics() {
        assert!(!confidence_escalate(0.7, 0.7).escalate);
        assert!(confidence_escalate(0.699, 0.7).escalate);

        // Even when EVOI is negative (expensive frontier, low task value),
        // cheap_confidence below safety_floor forces escalation.
        let forced = evoi_escalate(EvoiEscalationInput {
            cheap_confidence: 0.20,
            frontier_reliability: 0.90,
            cheap_cost_usd: 0.0,
            frontier_cost_usd: 0.50,
            task_value_usd: 0.05,
            safety_floor: 0.35,
        });
        assert!(forced.escalate);
        assert!(forced.evoi_usd < 0.0);
        assert!(forced.reason.contains("safety floor"));
    }

    #[test]
    fn sprt_escalator_clamps_constructor_bounds_and_serde_roundtrips() {
        let sprt = SprtEscalator::new(2.0, -1.0, 0.99, 0.0);
        assert!(sprt.upper_bound_b > 0.0);
        assert!(sprt.lower_bound_a < 0.0);
        assert_eq!(sprt.observations, 0);
        let encoded = serde_json::to_string(&sprt).expect("serialize SprtEscalator");
        let decoded: SprtEscalator =
            serde_json::from_str(&encoded).expect("deserialize SprtEscalator");
        assert_eq!(decoded, sprt);
    }
}
