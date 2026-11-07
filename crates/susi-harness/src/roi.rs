//! **SOVA (Sovereign Optimal Value Arbitrage)** and the 4-Lever Quantified ROI Engine.
//!
//! Susi's Tier-S orchestration USP combines two mathematical invariants:
//!
//! 1. **Cache-Warm Bellman ECVO (Expected Cost per Verified Outcome) Arbitrage**
//!    ([`SovaArbitrageEngine`]):
//!    On turn $t$ of a multi-turn coding task with context length $L$, warm
//!    prefix $W \le L$ cached on the active worker $m_{\text{active}}$, and
//!    expected output $O$, switching to any other worker $m' \ne m_{\text{active}}$
//!    forfeits the warm prefix cache and pays the cold-cache switching barrier
//!    $\Delta C_{\text{switch}} = W \cdot (p_{\text{miss}} - p_{\text{hit}}) / 10^6$.
//!    SOVA prices each candidate by its **Expected Cost per Verified Outcome**:
//!    $$\text{ECVO}(m \mid L, W_m, O) = \left\lceil \frac{1000 \cdot C_m(L, W_m, O)}{\hat{\theta}_m(\text{permille})} \right\rceil$$
//!    where $\hat{\theta}_m$ is the Beta-Binomial posterior reliability
//!    $\frac{\text{ok}_m + 2}{\text{ok}_m + \text{fail}_m + 3}$ decayed by the
//!    current task's consecutive failure streak $k_m$. This holds the warm
//!    prefix cache while the active worker succeeds and triggers automatic
//!    escalation the exact moment failure decay makes staying more expensive
//!    than switching.
//!
//! 2. **Exact 4-Lever Telescoping ROI Decomposition** ([`RoiReport`]):
//!    Every settled row in the append-only ledger is decomposed in exact
//!    integer micro-USD (`u64`) against both the same-model uncached cost and
//!    the uncached Frontier Reference tier ($15.00/\text{M}$ input,
//!    $75.00/\text{M}$ output), satisfying the conservation law:
//!    $$\text{actual\_cost} + \underbrace{\Delta_{\text{prompt\_cache}} + \Delta_{\text{response\_cache}} + \Delta_{\text{arbitrage}}}_{\text{total\_saved}} = \text{frontier\_baseline}$$

use crate::ledger::{LedgerEntry, Outcome};
use crate::money::MicroUsd;
use crate::pricing::{ModelPrice, TokenUsage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Uncached Frontier Reference price ($15.00/M input, $75.00/M output) used as
/// the counterfactual single-model sticker baseline when quantifying SOVA
/// arbitrage and cache ROI.
pub const FRONTIER_BASELINE_PRICE: ModelPrice = ModelPrice {
    input_miss: MicroUsd::from_micros(15_000_000),
    input_hit: MicroUsd::from_micros(15_000_000),
    output: MicroUsd::from_micros(75_000_000),
};

/// Quantified ROI breakdown for a single worker model across settled ledger rows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRoi {
    /// Settled spend in micro-USD.
    pub actual_cost_micro_usd: u64,
    /// Counterfactual cost on the same model without prompt or response caching.
    pub uncached_model_cost_micro_usd: u64,
    /// Counterfactual cost on the uncached Frontier Reference tier.
    pub frontier_baseline_micro_usd: u64,
    /// Micro-USD saved by prefix prompt caching (`cache_hit` billed at `input_hit`).
    pub prompt_cache_saved_micro_usd: u64,
    /// Micro-USD saved by the gateway response cache (`cached: true` at $0).
    pub response_cache_saved_micro_usd: u64,
    /// Micro-USD saved by routing to this worker instead of the Frontier Reference rate.
    pub model_arbitrage_saved_micro_usd: u64,
    /// Total micro-USD saved (`prompt_cache + response_cache + model_arbitrage`).
    pub total_saved_micro_usd: u64,
}

impl WorkerRoi {
    /// Verify the 4-lever conservation and ordering invariants for a single worker.
    ///
    /// # Errors
    ///
    /// Returns an explanation string when cost ordering or telescoping conservation fails.
    pub fn verify_worker_roi_invariants(&self) -> Result<(), String> {
        if self.actual_cost_micro_usd > self.uncached_model_cost_micro_usd {
            return Err(format!(
                "worker actual_cost_micro_usd ({}) exceeds uncached_model_cost_micro_usd ({})",
                self.actual_cost_micro_usd, self.uncached_model_cost_micro_usd
            ));
        }
        if self.uncached_model_cost_micro_usd > self.frontier_baseline_micro_usd {
            return Err(format!(
                "worker uncached_model_cost_micro_usd ({}) exceeds frontier_baseline_micro_usd ({})",
                self.uncached_model_cost_micro_usd, self.frontier_baseline_micro_usd
            ));
        }
        let cache_saved = self
            .prompt_cache_saved_micro_usd
            .saturating_add(self.response_cache_saved_micro_usd);
        if cache_saved
            != self
                .uncached_model_cost_micro_usd
                .saturating_sub(self.actual_cost_micro_usd)
        {
            return Err(format!(
                "worker cache savings ({cache_saved}) != uncached ({}) - actual ({})",
                self.uncached_model_cost_micro_usd, self.actual_cost_micro_usd
            ));
        }
        if self.model_arbitrage_saved_micro_usd
            != self
                .frontier_baseline_micro_usd
                .saturating_sub(self.uncached_model_cost_micro_usd)
        {
            return Err(format!(
                "worker arbitrage savings ({}) != frontier ({}) - uncached ({})",
                self.model_arbitrage_saved_micro_usd,
                self.frontier_baseline_micro_usd,
                self.uncached_model_cost_micro_usd
            ));
        }
        let sum_saved = cache_saved.saturating_add(self.model_arbitrage_saved_micro_usd);
        if self.total_saved_micro_usd != sum_saved {
            return Err(format!(
                "worker total_saved_micro_usd ({}) != lever sum ({sum_saved})",
                self.total_saved_micro_usd
            ));
        }
        if self
            .actual_cost_micro_usd
            .saturating_add(self.total_saved_micro_usd)
            != self.frontier_baseline_micro_usd
        {
            return Err(format!(
                "worker conservation violated: actual ({}) + saved ({}) != frontier ({})",
                self.actual_cost_micro_usd,
                self.total_saved_micro_usd,
                self.frontier_baseline_micro_usd
            ));
        }
        Ok(())
    }
}

/// Complete 4-lever quantified ROI report over a harness ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoiReport {
    /// Settled ledger spend in micro-USD.
    pub actual_cost_micro_usd: u64,
    /// Counterfactual spend on the exact same models without prompt or response caching.
    pub uncached_model_cost_micro_usd: u64,
    /// Counterfactual spend on the uncached Frontier Reference tier ($15/M in, $75/M out).
    pub frontier_baseline_micro_usd: u64,
    /// Lever 1: Micro-USD saved by prefix-locked prompt caching (`SmartParams::cache_scope`).
    pub prompt_cache_saved_micro_usd: u64,
    /// Lever 2: Micro-USD saved by content-addressed gateway response caching (`cached: true`).
    pub response_cache_saved_micro_usd: u64,
    /// Lever 3: Micro-USD saved by SOVA capability-matched model arbitrage vs. frontier sticker price.
    pub model_arbitrage_saved_micro_usd: u64,
    /// Total micro-USD saved across all active levers (`prompt_cache + response_cache + model_arbitrage`).
    pub total_saved_micro_usd: u64,
    /// Total cost reduction vs. the frontier baseline in basis points (`0..=10_000`, where `9250` = 92.50%).
    pub cost_reduction_bps: u16,
    /// Savings from caching alone (`prompt_cache + response_cache`) vs. `uncached_model_cost_micro_usd` in basis points (`0..=10_000`).
    pub cache_savings_bps: u16,
    /// Value delivered per dollar spent in centi-multiples (`100` = 1.00x, `1250` = 12.50x).
    pub roi_multiplier_centi: u64,
}

impl RoiReport {
    /// Compute the exact 4-lever ROI report and per-worker ROI map from settled ledger rows.
    #[must_use]
    pub fn from_entries(entries: &[LedgerEntry]) -> (Self, BTreeMap<String, WorkerRoi>) {
        let mut report = Self::default();
        let mut by_worker: BTreeMap<String, WorkerRoi> = BTreeMap::new();

        for entry in entries {
            let verdict = matches!(entry.outcome, Outcome::Accepted | Outcome::Rejected { .. });
            let actual = entry.cost.micros();
            let worker = by_worker.entry(entry.model.clone()).or_default();
            worker.actual_cost_micro_usd = worker.actual_cost_micro_usd.saturating_add(actual);
            report.actual_cost_micro_usd = report.actual_cost_micro_usd.saturating_add(actual);

            if verdict {
                // Verdict rows carry zero tokens; preserve conservation if a synthetic row carries cost.
                worker.uncached_model_cost_micro_usd =
                    worker.uncached_model_cost_micro_usd.saturating_add(actual);
                worker.frontier_baseline_micro_usd =
                    worker.frontier_baseline_micro_usd.saturating_add(actual);
                report.uncached_model_cost_micro_usd =
                    report.uncached_model_cost_micro_usd.saturating_add(actual);
                report.frontier_baseline_micro_usd =
                    report.frontier_baseline_micro_usd.saturating_add(actual);
                continue;
            }

            let uncached_usage = TokenUsage::new(entry.usage.input, 0, entry.usage.output);
            let uncached_model = entry.price.cost(uncached_usage).micros().max(actual);
            let raw_frontier = FRONTIER_BASELINE_PRICE.cost(uncached_usage).micros();
            let frontier = raw_frontier.max(uncached_model);

            let (prompt_saved, resp_saved) = if entry.cached {
                (0, uncached_model.saturating_sub(actual))
            } else {
                (uncached_model.saturating_sub(actual), 0)
            };
            let arbitrage_saved = frontier.saturating_sub(uncached_model);
            let call_saved = prompt_saved
                .saturating_add(resp_saved)
                .saturating_add(arbitrage_saved);

            worker.uncached_model_cost_micro_usd = worker
                .uncached_model_cost_micro_usd
                .saturating_add(uncached_model);
            worker.frontier_baseline_micro_usd =
                worker.frontier_baseline_micro_usd.saturating_add(frontier);
            worker.prompt_cache_saved_micro_usd = worker
                .prompt_cache_saved_micro_usd
                .saturating_add(prompt_saved);
            worker.response_cache_saved_micro_usd = worker
                .response_cache_saved_micro_usd
                .saturating_add(resp_saved);
            worker.model_arbitrage_saved_micro_usd = worker
                .model_arbitrage_saved_micro_usd
                .saturating_add(arbitrage_saved);
            worker.total_saved_micro_usd = worker.total_saved_micro_usd.saturating_add(call_saved);

            report.uncached_model_cost_micro_usd = report
                .uncached_model_cost_micro_usd
                .saturating_add(uncached_model);
            report.frontier_baseline_micro_usd =
                report.frontier_baseline_micro_usd.saturating_add(frontier);
            report.prompt_cache_saved_micro_usd = report
                .prompt_cache_saved_micro_usd
                .saturating_add(prompt_saved);
            report.response_cache_saved_micro_usd = report
                .response_cache_saved_micro_usd
                .saturating_add(resp_saved);
            report.model_arbitrage_saved_micro_usd = report
                .model_arbitrage_saved_micro_usd
                .saturating_add(arbitrage_saved);
            report.total_saved_micro_usd = report.total_saved_micro_usd.saturating_add(call_saved);
        }

        report.cost_reduction_bps = bps(
            report.total_saved_micro_usd,
            report.frontier_baseline_micro_usd,
        );
        let cache_saved = report
            .prompt_cache_saved_micro_usd
            .saturating_add(report.response_cache_saved_micro_usd);
        report.cache_savings_bps = bps(cache_saved, report.uncached_model_cost_micro_usd);
        report.roi_multiplier_centi = if report.frontier_baseline_micro_usd == 0 {
            100
        } else if report.actual_cost_micro_usd == 0 {
            // 100% zero-cost cached execution: cap display multiplier at 999.99x (99_999 centi).
            99_999
        } else {
            u64::try_from(
                u128::from(report.frontier_baseline_micro_usd)
                    .saturating_mul(100)
                    .checked_div(u128::from(report.actual_cost_micro_usd))
                    .unwrap_or(100),
            )
            .unwrap_or(u64::MAX)
        };

        (report, by_worker)
    }

    /// Human-readable one-line ROI summary for CLI and desktop Mission Control.
    #[must_use]
    pub fn summary_line(&self) -> String {
        let saved = MicroUsd::from_micros(self.total_saved_micro_usd);
        let baseline = MicroUsd::from_micros(self.frontier_baseline_micro_usd);
        let prompt = MicroUsd::from_micros(self.prompt_cache_saved_micro_usd);
        let resp = MicroUsd::from_micros(self.response_cache_saved_micro_usd);
        let arb = MicroUsd::from_micros(self.model_arbitrage_saved_micro_usd);
        format!(
            "saved {saved} ({}.{}% vs {baseline} frontier | {}.{:02}x ROI) [prompt cache {prompt}, resp cache {resp}, SOVA arbitrage {arb}]",
            self.cost_reduction_bps / 100,
            (self.cost_reduction_bps % 100) / 10,
            self.roi_multiplier_centi / 100,
            self.roi_multiplier_centi % 100,
        )
    }

    /// Verify the 4-lever conservation law and basis-point bounds on this [`RoiReport`].
    ///
    /// # Errors
    ///
    /// Returns an explanation string when cost ordering, lever conservation, or
    /// basis-point bounds fail.
    pub fn verify_roi_report_invariants(&self) -> Result<(), String> {
        let as_worker = WorkerRoi {
            actual_cost_micro_usd: self.actual_cost_micro_usd,
            uncached_model_cost_micro_usd: self.uncached_model_cost_micro_usd,
            frontier_baseline_micro_usd: self.frontier_baseline_micro_usd,
            prompt_cache_saved_micro_usd: self.prompt_cache_saved_micro_usd,
            response_cache_saved_micro_usd: self.response_cache_saved_micro_usd,
            model_arbitrage_saved_micro_usd: self.model_arbitrage_saved_micro_usd,
            total_saved_micro_usd: self.total_saved_micro_usd,
        };
        as_worker.verify_worker_roi_invariants()?;
        if self.cost_reduction_bps > 10_000 {
            return Err(format!(
                "cost_reduction_bps ({}) exceeds 10_000",
                self.cost_reduction_bps
            ));
        }
        if self.cache_savings_bps > 10_000 {
            return Err(format!(
                "cache_savings_bps ({}) exceeds 10_000",
                self.cache_savings_bps
            ));
        }
        if self.roi_multiplier_centi < 100 {
            return Err(format!(
                "roi_multiplier_centi ({}) must be at least 100 (1.00x)",
                self.roi_multiplier_centi
            ));
        }
        Ok(())
    }
}

fn bps(numerator: u64, denominator: u64) -> u16 {
    if denominator == 0 {
        return 0;
    }
    let ratio = u128::from(numerator)
        .saturating_mul(10_000)
        .checked_div(u128::from(denominator))
        .unwrap_or(0)
        .min(10_000);
    u16::try_from(ratio).unwrap_or(10_000)
}

/// Candidate worker model evaluated by [`SovaArbitrageEngine`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SovaCandidate {
    /// Canonical model id.
    pub model: String,
    /// Per-million-token price in micro-USD.
    pub price: ModelPrice,
    /// Historical successful attempts recorded in the ledger.
    pub successes: u64,
    /// Historical failed attempts recorded in the ledger.
    pub failures: u64,
    /// Consecutive failures on the current task (`0` when healthy).
    pub consecutive_task_failures: u32,
    /// Whether this model meets the task's minimum capability floor.
    pub meets_capability_floor: bool,
}

/// Evaluated Bellman-ECVO metrics for a single [`SovaCandidate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SovaRankedCandidate {
    /// Model id.
    pub model: String,
    /// Whether this candidate meets the task capability floor.
    pub meets_capability_floor: bool,
    /// Warm prefix tokens available to this candidate (`> 0` only for the active warm model).
    pub warm_prefix_tokens: u64,
    /// Expected single-turn cost in micro-USD given `warm_prefix_tokens`.
    pub turn_cost_micro_usd: u64,
    /// Counterfactual single-turn cost in micro-USD if run with a cold cache (`warm_prefix_tokens = 0`).
    pub cold_turn_cost_micro_usd: u64,
    /// Micro-USD value of the warm prefix cache held by this candidate.
    pub cache_switch_barrier_micro_usd: u64,
    /// Beta-Binomial posterior reliability in parts per thousand (`50..=995`), after streak decay.
    pub reliability_permille: u16,
    /// Expected Cost per Verified Outcome (`ceil(1000 * turn_cost / reliability_permille)`) in micro-USD.
    pub ecvo_micro_usd: u64,
}

/// Why [`SovaArbitrageEngine`] selected the winning worker for the next turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SovaVerdict {
    /// Cold-start selection on the first turn (no warm prefix cache yet).
    InitialSelect {
        /// Selected model id.
        selected_model: String,
        /// Expected Cost per Verified Outcome in micro-USD.
        ecvo_micro_usd: u64,
    },
    /// Kept the currently active model because its warm prefix cache makes its
    /// ECVO lower than or equal to switching cold to another model.
    HoldWarmCache {
        /// Active model retained.
        selected_model: String,
        /// Micro-USD saved on this turn by preserving the warm prefix cache.
        saved_switch_penalty_micro_usd: u64,
        /// Expected Cost per Verified Outcome in micro-USD.
        ecvo_micro_usd: u64,
    },
    /// Escalated away from the warm active model because consecutive task
    /// failures decayed its reliability enough to overcome the prefix-cache
    /// switching barrier.
    EscalateOnFailureDecay {
        /// Previous active model that decayed.
        from_model: String,
        /// Newly selected stronger/healthier model.
        to_model: String,
        /// ECVO reduction in micro-USD achieved by escalating despite cold cache.
        ecvo_saving_micro_usd: u64,
    },
}

/// Complete decision returned by [`SovaArbitrageEngine::select_optimal_worker`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SovaDecision {
    /// Winning model id.
    pub selected_model: String,
    /// Algorithmic explanation of the selection.
    pub verdict: SovaVerdict,
    /// All candidates ordered best-first by capability floor and `ecvo_micro_usd`.
    pub ranked: Vec<SovaRankedCandidate>,
}

impl SovaRankedCandidate {
    /// Verify structural and Bellman-ECVO invariants for a ranked candidate.
    ///
    /// # Errors
    ///
    /// Returns an explanation string when the model id is blank, warm turn cost
    /// exceeds cold turn cost, the switching barrier does not match their
    /// difference, reliability is outside `50..=995`, or `ecvo_micro_usd` is
    /// smaller than `turn_cost_micro_usd`.
    pub fn verify_ranked_candidate_invariants(&self) -> Result<(), String> {
        if self.model.trim().is_empty() {
            return Err("ranked candidate model must not be empty".to_string());
        }
        if self.turn_cost_micro_usd > self.cold_turn_cost_micro_usd {
            return Err(format!(
                "candidate '{}' turn_cost ({}) exceeds cold_turn_cost ({})",
                self.model, self.turn_cost_micro_usd, self.cold_turn_cost_micro_usd
            ));
        }
        if self.cache_switch_barrier_micro_usd
            != self
                .cold_turn_cost_micro_usd
                .saturating_sub(self.turn_cost_micro_usd)
        {
            return Err(format!(
                "candidate '{}' cache_switch_barrier ({}) != cold ({}) - turn ({})",
                self.model,
                self.cache_switch_barrier_micro_usd,
                self.cold_turn_cost_micro_usd,
                self.turn_cost_micro_usd
            ));
        }
        if !(50..=995).contains(&self.reliability_permille) {
            return Err(format!(
                "candidate '{}' reliability_permille ({}) outside 50..=995",
                self.model, self.reliability_permille
            ));
        }
        if self.ecvo_micro_usd < self.turn_cost_micro_usd {
            return Err(format!(
                "candidate '{}' ecvo_micro_usd ({}) < turn_cost_micro_usd ({})",
                self.model, self.ecvo_micro_usd, self.turn_cost_micro_usd
            ));
        }
        Ok(())
    }
}

impl SovaDecision {
    /// Verify structural, ranking, and verdict invariants of a [`SovaDecision`].
    ///
    /// # Errors
    ///
    /// Returns an explanation string if `selected_model` is blank, `ranked` is
    /// empty or out of order, or `verdict` is inconsistent with the winner.
    pub fn verify_decision_invariants(&self) -> Result<(), String> {
        if self.selected_model.trim().is_empty() {
            return Err("selected_model must not be empty".to_string());
        }
        let Some(first) = self.ranked.first() else {
            return Err("ranked candidates must not be empty".to_string());
        };
        if first.model != self.selected_model {
            return Err(format!(
                "selected_model '{}' does not match top-ranked candidate '{}'",
                self.selected_model, first.model
            ));
        }
        for (idx, candidate) in self.ranked.iter().enumerate() {
            candidate.verify_ranked_candidate_invariants()?;
            if idx > 0 {
                let prev = &self.ranked[idx - 1];
                let bad_order = (!prev.meets_capability_floor && candidate.meets_capability_floor)
                    || (prev.meets_capability_floor == candidate.meets_capability_floor
                        && prev.ecvo_micro_usd > candidate.ecvo_micro_usd);
                if bad_order {
                    return Err(format!(
                        "ranked candidates out of ECVO order at index {idx} ('{}' before '{}')",
                        prev.model, candidate.model
                    ));
                }
            }
        }
        match &self.verdict {
            SovaVerdict::InitialSelect {
                selected_model,
                ecvo_micro_usd,
            } => {
                if selected_model != &self.selected_model || *ecvo_micro_usd != first.ecvo_micro_usd
                {
                    return Err(
                        "InitialSelect verdict does not match winning candidate".to_string()
                    );
                }
            }
            SovaVerdict::HoldWarmCache {
                selected_model,
                saved_switch_penalty_micro_usd,
                ecvo_micro_usd,
            } => {
                if selected_model != &self.selected_model
                    || *ecvo_micro_usd != first.ecvo_micro_usd
                    || *saved_switch_penalty_micro_usd != first.cache_switch_barrier_micro_usd
                    || first.warm_prefix_tokens == 0
                {
                    return Err(
                        "HoldWarmCache verdict does not match warm winning candidate".to_string(),
                    );
                }
            }
            SovaVerdict::EscalateOnFailureDecay {
                from_model,
                to_model,
                ..
            } => {
                if from_model.trim().is_empty()
                    || to_model != &self.selected_model
                    || from_model == to_model
                {
                    return Err(
                        "EscalateOnFailureDecay verdict has invalid model transition".to_string(),
                    );
                }
            }
        }
        Ok(())
    }
}

/// Cache-Warm Bellman ECVO (Expected Cost per Verified Outcome) arbitrage engine.
pub struct SovaArbitrageEngine;

impl SovaArbitrageEngine {
    /// Compute the Beta-Binomial Laplace posterior reliability in permille (`50..=995`)
    /// with exponential decay on consecutive task failures.
    #[must_use]
    pub fn reliability_permille(
        successes: u64,
        failures: u64,
        consecutive_task_failures: u32,
    ) -> u16 {
        // Beta(2, 1) prior: an untried model starts at 2/3 = 666 permille.
        let num = u128::from(successes).saturating_add(2).saturating_mul(1000);
        let den = u128::from(successes)
            .saturating_add(u128::from(failures))
            .saturating_add(3);
        let base = num.checked_div(den).unwrap_or(666);
        let decayed = match consecutive_task_failures {
            0 => base,
            1 => base.saturating_mul(3) / 4,
            2 => base / 2,
            _ => base / 4,
        };
        u16::try_from(decayed.clamp(50, 995)).unwrap_or(50)
    }

    /// Rank `candidates` for a turn with `input_tokens`, `warm_prefix_tokens` on
    /// `active_model`, and `expected_output_tokens`, returning the optimal
    /// [`SovaDecision`].
    #[must_use]
    pub fn select_optimal_worker(
        candidates: &[SovaCandidate],
        active_model: Option<&str>,
        input_tokens: u64,
        warm_prefix_tokens: u64,
        expected_output_tokens: u64,
    ) -> Option<SovaDecision> {
        if candidates.is_empty() {
            return None;
        }
        let clamped_warm = warm_prefix_tokens.min(input_tokens);
        let mut ranked: Vec<SovaRankedCandidate> = candidates
            .iter()
            .map(|c| {
                let is_active = active_model.is_some_and(|a| a == c.model);
                let warm = if is_active { clamped_warm } else { 0 };
                let turn_cost = c
                    .price
                    .cost(TokenUsage::new(input_tokens, warm, expected_output_tokens))
                    .micros();
                let cold_cost = c
                    .price
                    .cost(TokenUsage::new(input_tokens, 0, expected_output_tokens))
                    .micros();
                let barrier = cold_cost.saturating_sub(turn_cost);
                let rel = Self::reliability_permille(
                    c.successes,
                    c.failures,
                    c.consecutive_task_failures,
                );
                let ecvo = u64::try_from(
                    u128::from(turn_cost)
                        .saturating_mul(1000)
                        .saturating_add(u128::from(rel).saturating_sub(1))
                        .checked_div(u128::from(rel.max(50)))
                        .unwrap_or(u128::MAX),
                )
                .unwrap_or(u64::MAX);
                SovaRankedCandidate {
                    model: c.model.clone(),
                    meets_capability_floor: c.meets_capability_floor,
                    warm_prefix_tokens: warm,
                    turn_cost_micro_usd: turn_cost,
                    cold_turn_cost_micro_usd: cold_cost,
                    cache_switch_barrier_micro_usd: barrier,
                    reliability_permille: rel,
                    ecvo_micro_usd: ecvo,
                }
            })
            .collect();

        ranked.sort_by(|a, b| {
            b.meets_capability_floor
                .cmp(&a.meets_capability_floor)
                .then_with(|| a.ecvo_micro_usd.cmp(&b.ecvo_micro_usd))
                .then_with(|| a.turn_cost_micro_usd.cmp(&b.turn_cost_micro_usd))
        });

        let winner = ranked.first()?.clone();
        let verdict = match active_model {
            Some(active) if active == winner.model && winner.warm_prefix_tokens > 0 => {
                SovaVerdict::HoldWarmCache {
                    selected_model: winner.model.clone(),
                    saved_switch_penalty_micro_usd: winner.cache_switch_barrier_micro_usd,
                    ecvo_micro_usd: winner.ecvo_micro_usd,
                }
            }
            Some(active) if active != winner.model => {
                let prev_ecvo = ranked
                    .iter()
                    .find(|r| r.model == active)
                    .map_or(winner.ecvo_micro_usd, |r| r.ecvo_micro_usd);
                SovaVerdict::EscalateOnFailureDecay {
                    from_model: active.to_string(),
                    to_model: winner.model.clone(),
                    ecvo_saving_micro_usd: prev_ecvo.saturating_sub(winner.ecvo_micro_usd),
                }
            }
            _ => SovaVerdict::InitialSelect {
                selected_model: winner.model.clone(),
                ecvo_micro_usd: winner.ecvo_micro_usd,
            },
        };

        Some(SovaDecision {
            selected_model: winner.model,
            verdict,
            ranked,
        })
    }
}

/// Runtime self-check used by `susi crown verify` to prove that SOVA's
/// Cache-Warm Bellman ECVO arbitrage and 4-lever ROI conservation law hold in
/// the live binary.
///
/// # Errors
///
/// Returns an explanation string if any invariant is violated.
pub fn verify_sova_invariants() -> Result<String, String> {
    let deepseek_price = ModelPrice {
        input_miss: MicroUsd::from_micros(270_000),
        input_hit: MicroUsd::from_micros(27_000),
        output: MicroUsd::from_micros(1_100_000),
    };
    let gemini_price = ModelPrice {
        input_miss: MicroUsd::from_micros(300_000),
        input_hit: MicroUsd::from_micros(30_000),
        output: MicroUsd::from_micros(1_200_000),
    };

    // 1. Verify Cache-Warm Bellman ECVO holds warm cache when healthy, then escalates on failure decay.
    let healthy_candidates = vec![
        SovaCandidate {
            model: "deepseek-chat".into(),
            price: deepseek_price,
            successes: 18,
            failures: 2,
            consecutive_task_failures: 0,
            meets_capability_floor: true,
        },
        SovaCandidate {
            model: "gemini-2.5-flash".into(),
            price: gemini_price,
            successes: 19,
            failures: 1,
            consecutive_task_failures: 0,
            meets_capability_floor: true,
        },
    ];
    let hold = SovaArbitrageEngine::select_optimal_worker(
        &healthy_candidates,
        Some("deepseek-chat"),
        200_000,
        160_000,
        8_000,
    )
    .ok_or_else(|| "SOVA returned no decision for healthy candidates".to_string())?;
    if !matches!(hold.verdict, SovaVerdict::HoldWarmCache { .. }) {
        return Err(format!(
            "expected HoldWarmCache on healthy warm model, got {:?}",
            hold.verdict
        ));
    }

    let decayed_candidates = vec![
        SovaCandidate {
            model: "deepseek-chat".into(),
            price: deepseek_price,
            successes: 18,
            failures: 2,
            consecutive_task_failures: 3,
            meets_capability_floor: true,
        },
        SovaCandidate {
            model: "gemini-2.5-flash".into(),
            price: gemini_price,
            successes: 19,
            failures: 1,
            consecutive_task_failures: 0,
            meets_capability_floor: true,
        },
    ];
    let escalated = SovaArbitrageEngine::select_optimal_worker(
        &decayed_candidates,
        Some("deepseek-chat"),
        200_000,
        160_000,
        8_000,
    )
    .ok_or_else(|| "SOVA returned no decision for decayed candidates".to_string())?;
    if !matches!(
        escalated.verdict,
        SovaVerdict::EscalateOnFailureDecay { .. }
    ) {
        return Err(format!(
            "expected EscalateOnFailureDecay after 3 consecutive failures, got {:?}",
            escalated.verdict
        ));
    }

    // 2. Verify exact 4-lever ROI conservation law on multi-turn cached + response-cached rows.
    let usage = TokenUsage::new(200_000, 160_000, 10_000);
    let settled_cost = deepseek_price.cost(usage);
    let entries = vec![
        LedgerEntry {
            mission: "crown-sova".into(),
            task: "t1".into(),
            attempt: 1,
            model: "deepseek-chat".into(),
            provider: "deepseek".into(),
            usage,
            price: deepseek_price,
            cost: settled_cost,
            cached: false,
            reported: false,
            started_unix_ms: 100,
            ended_unix_ms: 200,
            outcome: Outcome::Completed,
        },
        LedgerEntry {
            mission: "crown-sova".into(),
            task: "t1".into(),
            attempt: 1,
            model: "deepseek-chat".into(),
            provider: "deepseek".into(),
            usage,
            price: deepseek_price,
            cost: MicroUsd::ZERO,
            cached: true,
            reported: false,
            started_unix_ms: 210,
            ended_unix_ms: 212,
            outcome: Outcome::Completed,
        },
    ];
    let (roi, _) = RoiReport::from_entries(&entries);
    if roi
        .actual_cost_micro_usd
        .saturating_add(roi.total_saved_micro_usd)
        != roi.frontier_baseline_micro_usd
    {
        return Err(format!(
            "ROI conservation law failed: actual {} + saved {} != baseline {}",
            roi.actual_cost_micro_usd, roi.total_saved_micro_usd, roi.frontier_baseline_micro_usd
        ));
    }
    if roi.cost_reduction_bps < 9000 {
        return Err(format!(
            "expected >= 9000 bps (90%) cost reduction on SOVA workload, got {}",
            roi.cost_reduction_bps
        ));
    }

    Ok(format!(
        "SOVA Bellman-ECVO verified (warm hold + failure-decay escalation); 4-lever ROI conservation holds ({})",
        roi.summary_line()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_lever_roi_satisfies_exact_telescoping_conservation_law() {
        let price = ModelPrice {
            input_miss: MicroUsd::from_micros(300_000),
            input_hit: MicroUsd::from_micros(30_000),
            output: MicroUsd::from_micros(1_200_000),
        };
        let usage = TokenUsage::new(100_000, 80_000, 5_000);
        let actual_cost = price.cost(usage);
        // Uncached model cost: 100k * $0.30/M + 5k * $1.20/M = 30,000 + 6,000 = 36,000 micro-USD.
        // Actual cost with 80k cache hit: 20k * $0.30/M + 80k * $0.03/M + 5k * $1.20/M = 6,000 + 2,400 + 6,000 = 14,400 micro-USD.
        assert_eq!(actual_cost.micros(), 14_400);

        let entries = vec![
            LedgerEntry {
                mission: "m1".into(),
                task: "t1".into(),
                attempt: 1,
                model: "deepseek-chat".into(),
                provider: "deepseek".into(),
                usage,
                price,
                cost: actual_cost,
                cached: false,
                reported: false,
                started_unix_ms: 10,
                ended_unix_ms: 20,
                outcome: Outcome::Completed,
            },
            LedgerEntry {
                mission: "m1".into(),
                task: "t1".into(),
                attempt: 1,
                model: "deepseek-chat".into(),
                provider: "deepseek".into(),
                usage,
                price,
                cost: MicroUsd::ZERO,
                cached: true,
                reported: false,
                started_unix_ms: 25,
                ended_unix_ms: 26,
                outcome: Outcome::Completed,
            },
        ];

        let (roi, workers) = RoiReport::from_entries(&entries);
        assert_eq!(roi.actual_cost_micro_usd, 14_400);
        assert_eq!(roi.uncached_model_cost_micro_usd, 72_000);
        assert_eq!(roi.prompt_cache_saved_micro_usd, 21_600);
        assert_eq!(roi.response_cache_saved_micro_usd, 36_000);
        // Frontier baseline per call: 100k * $15/M + 5k * $75/M = 1,500,000 + 375,000 = 1,875,000 micro-USD (x2 = 3,750,000).
        assert_eq!(roi.frontier_baseline_micro_usd, 3_750_000);
        assert_eq!(roi.model_arbitrage_saved_micro_usd, 3_750_000 - 72_000);
        assert_eq!(
            roi.actual_cost_micro_usd + roi.total_saved_micro_usd,
            roi.frontier_baseline_micro_usd
        );
        assert!(roi.cost_reduction_bps > 9900);
        assert_eq!(
            workers["deepseek-chat"].total_saved_micro_usd,
            roi.total_saved_micro_usd
        );
    }

    #[test]
    fn sova_bellman_ecvo_holds_warm_cache_and_escalates_on_streak_decay() {
        let proof = verify_sova_invariants().expect("SOVA invariants must hold");
        assert!(proof.contains("SOVA Bellman-ECVO verified"));
    }

    #[test]
    fn empty_ledger_and_sova_capability_floor_and_micro_usd_helpers_hold() {
        let (empty_roi, workers) = RoiReport::from_entries(&[]);
        assert_eq!(empty_roi.actual_cost_micro_usd, 0);
        assert_eq!(empty_roi.uncached_model_cost_micro_usd, 0);
        assert_eq!(empty_roi.frontier_baseline_micro_usd, 0);
        assert_eq!(empty_roi.total_saved_micro_usd, 0);
        assert_eq!(empty_roi.cost_reduction_bps, 0);
        assert_eq!(empty_roi.cache_savings_bps, 0);
        assert_eq!(empty_roi.roi_multiplier_centi, 100);
        assert!(workers.is_empty());

        let a = MicroUsd::from_micros(250_000);
        let b = MicroUsd::from_micros(100_000);
        assert_eq!(a.saturating_add(b).micros(), 350_000);
        assert_eq!(a.checked_sub(b), Some(MicroUsd::from_micros(150_000)));
        assert_eq!(b.checked_sub(a), None);

        assert!(SovaArbitrageEngine::select_optimal_worker(&[], None, 10_000, 0, 1_000).is_none());

        let cheap_below_floor = SovaCandidate {
            model: "tiny-below-floor".into(),
            price: ModelPrice {
                input_miss: MicroUsd::from_micros(10_000),
                input_hit: MicroUsd::from_micros(1_000),
                output: MicroUsd::from_micros(20_000),
            },
            successes: 50,
            failures: 0,
            consecutive_task_failures: 0,
            meets_capability_floor: false,
        };
        let capable_worker = SovaCandidate {
            model: "capable-pro".into(),
            price: ModelPrice {
                input_miss: MicroUsd::from_micros(500_000),
                input_hit: MicroUsd::from_micros(50_000),
                output: MicroUsd::from_micros(1_500_000),
            },
            successes: 20,
            failures: 1,
            consecutive_task_failures: 0,
            meets_capability_floor: true,
        };
        let decision = SovaArbitrageEngine::select_optimal_worker(
            &[cheap_below_floor, capable_worker],
            None,
            50_000,
            0,
            4_000,
        )
        .expect("SOVA decision for non-empty candidates");
        assert_eq!(decision.selected_model, "capable-pro");
        assert!(matches!(
            decision.verdict,
            SovaVerdict::InitialSelect {
                ref selected_model,
                ..
            } if selected_model == "capable-pro"
        ));
    }

    #[test]
    fn token_usage_clamps_cached_to_input_and_roi_report_summary_line_formats_verdict() {
        let clamped = TokenUsage::new(100, 250, 40);
        assert_eq!(clamped.input, 100);
        assert_eq!(clamped.cache_hit, 100);
        assert_eq!(clamped.cache_miss(), 0);
        assert_eq!(clamped.total(), 140);

        let price = ModelPrice {
            input_miss: MicroUsd::from_micros(300_000),
            input_hit: MicroUsd::from_micros(30_000),
            output: MicroUsd::from_micros(1_200_000),
        };
        let usage = TokenUsage::new(50_000, 40_000, 2_000);
        let entry = LedgerEntry {
            mission: "m-summary".into(),
            task: "t-summary".into(),
            attempt: 1,
            model: "deepseek-chat".into(),
            provider: "deepseek".into(),
            usage,
            price,
            cost: price.cost(usage),
            cached: false,
            reported: false,
            started_unix_ms: 1,
            ended_unix_ms: 2,
            outcome: Outcome::Completed,
        };
        let (report, _) = RoiReport::from_entries(&[entry]);
        let summary = report.summary_line();
        assert!(!summary.is_empty());
        assert!(summary.contains("saved "), "{summary}");
        assert!(summary.contains("frontier"), "{summary}");
        assert!(summary.contains("ROI)"), "{summary}");
        assert!(summary.contains("prompt cache"), "{summary}");
        assert!(summary.contains("resp cache"), "{summary}");
        assert!(summary.contains("SOVA arbitrage"), "{summary}");
    }

    #[test]
    fn reliability_permille_decays_monotonically_with_consecutive_failures_and_clamps_bounds() {
        let r0 = SovaArbitrageEngine::reliability_permille(18, 2, 0);
        let r1 = SovaArbitrageEngine::reliability_permille(18, 2, 1);
        let r2 = SovaArbitrageEngine::reliability_permille(18, 2, 2);
        let r3 = SovaArbitrageEngine::reliability_permille(18, 2, 3);
        assert!(r0 > r1 && r1 > r2 && r2 > r3);
        assert_eq!(SovaArbitrageEngine::reliability_permille(0, 10_000, 5), 50);
        assert_eq!(
            SovaArbitrageEngine::reliability_permille(100_000, 0, 0),
            995
        );
    }

    #[test]
    fn roi_report_worker_roi_and_sova_decision_verify_invariants_reject_corrupt_state() {
        let price = ModelPrice {
            input_miss: MicroUsd::from_micros(300_000),
            input_hit: MicroUsd::from_micros(30_000),
            output: MicroUsd::from_micros(1_200_000),
        };
        let usage = TokenUsage::new(100_000, 80_000, 5_000);
        let entry = LedgerEntry {
            mission: "m1".into(),
            task: "t1".into(),
            attempt: 1,
            model: "deepseek-chat".into(),
            provider: "deepseek".into(),
            usage,
            price,
            cost: price.cost(usage),
            cached: false,
            reported: false,
            started_unix_ms: 10,
            ended_unix_ms: 20,
            outcome: Outcome::Completed,
        };
        let (roi, workers) = RoiReport::from_entries(&[entry]);
        assert!(roi.verify_roi_report_invariants().is_ok());
        assert!(workers["deepseek-chat"]
            .verify_worker_roi_invariants()
            .is_ok());

        let mut bad_roi = roi.clone();
        bad_roi.total_saved_micro_usd += 1;
        assert!(bad_roi.verify_roi_report_invariants().is_err());

        let mut bad_bps = roi.clone();
        bad_bps.cost_reduction_bps = 10_001;
        assert!(bad_bps.verify_roi_report_invariants().is_err());

        let mut bad_mult = roi;
        bad_mult.roi_multiplier_centi = 99;
        assert!(bad_mult.verify_roi_report_invariants().is_err());

        let candidates = vec![
            SovaCandidate {
                model: "deepseek-chat".into(),
                price,
                successes: 18,
                failures: 2,
                consecutive_task_failures: 0,
                meets_capability_floor: true,
            },
            SovaCandidate {
                model: "gemini-2.5-flash".into(),
                price,
                successes: 10,
                failures: 5,
                consecutive_task_failures: 1,
                meets_capability_floor: true,
            },
        ];
        let decision = SovaArbitrageEngine::select_optimal_worker(
            &candidates,
            Some("deepseek-chat"),
            100_000,
            80_000,
            5_000,
        )
        .unwrap();
        assert!(decision.verify_decision_invariants().is_ok());

        let mut swapped_order = decision.clone();
        swapped_order.ranked.reverse();
        assert!(swapped_order.verify_decision_invariants().is_err());

        let mut bad_candidate = decision;
        bad_candidate.ranked[0].reliability_permille = 10;
        assert!(bad_candidate.verify_decision_invariants().is_err());
    }
}
