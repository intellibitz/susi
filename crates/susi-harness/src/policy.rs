//! Model ranking and the escalation ladder.
//!
//! Two decisions, both about money:
//!
//! 1. **Which model should try a task first?** Models are ranked by their
//!    expected cost *per accepted task*, not by their sticker price. The
//!    evidence is the ledger: a model that is cheap but leaves half its tasks
//!    rejected is not cheap. A model with no history is ranked at its list
//!    price, so a new cheap model gets its chance.
//! 2. **When does a failure deserve another attempt?** A transient provider
//!    error (rate limit, overload, timeout) gets one retry on the same model. A
//!    semantic failure — the agent produced work that did not pass — never
//!    retries the same model: the ladder escalates immediately. The ladder is
//!    capped, and it stops entirely when two different models fail the same
//!    way, because that is evidence about the task or the environment, not
//!    about model capability. Repeating it would only spend money.
//!
//! The ladder is deterministic and independent of the executor, so the policy
//! is testable without running a model.

use crate::catalog::PriceCatalogue;
use crate::ledger::{LedgerEntry, Outcome};
use crate::money::MicroUsd;
use std::collections::{BTreeMap, BTreeSet};
use susi_error::{EaiError, EaiResult};

/// The token profile used to compare models before a task has run.
///
/// A coding turn reads much more than it writes, so pricing a model on input
/// alone would rank a verbose model too cheaply. The profile is a rough but
/// fixed yardstick: every model is compared on the same one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageProfile {
    /// Input tokens one typical turn consumes.
    pub input: u64,
    /// Output tokens one typical turn produces.
    pub output: u64,
}

impl UsageProfile {
    /// A typical agent turn: 200k input, 20k output.
    #[must_use]
    pub const fn agent() -> Self {
        Self {
            input: 200_000,
            output: 20_000,
        }
    }
}

impl Default for UsageProfile {
    fn default() -> Self {
        Self::agent()
    }
}

/// What the ledger has taught us about one model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelHistory {
    /// Priced calls.
    pub calls: u64,
    /// Calls the provider or the acceptance rejected.
    pub failures: u64,
    /// Total spend on this model.
    pub cost: MicroUsd,
}

impl ModelHistory {
    /// Calls that were not rejected.
    #[must_use]
    pub const fn successes(self) -> u64 {
        self.calls.saturating_sub(self.failures)
    }
}

/// Learn per-model history from every settled row.
#[must_use]
pub fn history(entries: &[LedgerEntry]) -> BTreeMap<String, ModelHistory> {
    let mut history: BTreeMap<String, ModelHistory> = BTreeMap::new();
    for entry in entries {
        let slot = history.entry(entry.model.clone()).or_default();
        slot.calls = slot.calls.saturating_add(1);
        slot.cost = slot
            .cost
            .checked_add(entry.cost)
            .unwrap_or(MicroUsd::from_micros(u64::MAX));
        if matches!(
            entry.outcome,
            Outcome::ModelFailed { .. } | Outcome::Rejected { .. }
        ) {
            slot.failures = slot.failures.saturating_add(1);
        }
    }
    history
}

/// One ranked candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Model id.
    pub model: String,
    /// Expected cost of getting one task accepted with this model.
    pub expected_cost: MicroUsd,
}

/// Rank candidate models by expected cost per accepted task, cheapest first.
///
/// A model the catalogue cannot price is refused here rather than at the
/// gateway, because an unpriced model cannot be ranked or budgeted.
///
/// # Errors
///
/// Returns `EaiError::config` when a candidate has no price, or when there is
/// no candidate at all.
pub fn rank_models(
    catalogue: &PriceCatalogue,
    candidates: &[String],
    profile: UsageProfile,
    history: &BTreeMap<String, ModelHistory>,
) -> EaiResult<Vec<Candidate>> {
    if candidates.is_empty() {
        return Err(EaiError::config("no candidate models"));
    }
    let mut ranked = Vec::with_capacity(candidates.len());
    let mut seen = BTreeSet::new();
    for model in candidates {
        if !seen.insert(model.as_str()) {
            continue;
        }
        let price = catalogue.price(model).ok_or_else(|| {
            EaiError::config(format!("model {model} has no price in the catalogue"))
        })?;
        let base = price.cost(crate::pricing::TokenUsage::new(
            profile.input,
            0,
            profile.output,
        ));
        let expected = expected_cost(base, history.get(model).copied().unwrap_or_default());
        ranked.push(Candidate {
            model: model.clone(),
            expected_cost: expected,
        });
    }
    ranked.sort_by(|left, right| {
        left.expected_cost
            .cmp(&right.expected_cost)
            .then_with(|| left.model.cmp(&right.model))
    });
    Ok(ranked)
}

/// A model's list-price cost inflated by its observed record.
///
/// `calls / successes` is the average number of attempts one accepted task
/// needed; multiplied by the average price of an attempt it becomes the true
/// unit cost. A model that has never succeeded is pushed behind every model
/// that has: a failure costs money without buying a result.
fn expected_cost(base: MicroUsd, history: ModelHistory) -> MicroUsd {
    if history.calls == 0 {
        return base;
    }
    let successes = history.successes();
    if successes == 0 {
        return base.saturating_mul(history.calls.saturating_add(1));
    }
    let average = history.cost.micros().div_ceil(history.calls);
    let attempts = history.calls.div_ceil(successes);
    MicroUsd::from_micros(
        u64::try_from(
            u128::from(average)
                .saturating_mul(u128::from(attempts))
                .min(u128::from(u64::MAX)),
        )
        .unwrap_or(u64::MAX),
    )
}

/// Why an attempt failed, as far as the next decision is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The provider refused temporarily (rate limit, overload, connection).
    Transient,
    /// The turn ran past its deadline.
    Timeout,
    /// The agent ran and produced work that did not pass.
    Semantic,
    /// The executor itself could not run (spawn, I/O).
    Executor,
}

impl FailureKind {
    /// Classify a failure message from an agent envelope.
    #[must_use]
    pub fn classify(message: &str) -> Self {
        let text = message.to_ascii_lowercase();
        const TRANSIENT: &[&str] = &[
            "rate limit",
            "rate_limit",
            "429",
            "overloaded",
            "temporarily",
            "unavailable",
            "connection",
            "network",
            "502",
            "503",
            "504",
        ];
        const TIMEOUT: &[&str] = &["timed out", "timeout", "deadline exceeded"];
        if TIMEOUT.iter().any(|needle| text.contains(needle)) {
            return Self::Timeout;
        }
        if TRANSIENT.iter().any(|needle| text.contains(needle)) {
            return Self::Transient;
        }
        Self::Semantic
    }
}

/// How many attempts a task may make, and when it stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Hard cap on attempts for one task.
    pub max_attempts: usize,
    /// Attempts one model may make in a row (a transient retry is the second).
    pub max_same_model: usize,
    /// Retry a transient provider failure on the same model once.
    pub retry_transient: bool,
    /// Stop when two models fail with the same signature.
    pub stop_on_repeat: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            max_same_model: 2,
            retry_transient: true,
            stop_on_repeat: true,
        }
    }
}

/// One attempt the ladder selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    /// Model to run.
    pub model: String,
    /// 1-based attempt number within the task.
    pub number: u32,
}

/// Why the ladder stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The attempt cap or the candidate list ran out.
    Exhausted,
    /// Two models failed the same way; the task is stuck.
    Stuck {
        /// The repeated failure, as normalised.
        signature: String,
    },
}

/// What the ladder wants next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextAttempt {
    /// Run this attempt.
    Attempt(Attempt),
    /// Stop; nothing more will be tried.
    Stop(StopReason),
}

/// The escalation ladder for one task.
#[derive(Debug, Clone)]
pub struct Ladder {
    ranked: Vec<String>,
    policy: Policy,
    index: usize,
    used: usize,
    same_model: usize,
    pending_same_model: bool,
    signatures: BTreeMap<String, String>,
    stuck: Option<String>,
}

impl Ladder {
    /// Build a ladder over `ranked` (cheapest expected cost first).
    #[must_use]
    pub fn new(ranked: Vec<String>, policy: Policy) -> Self {
        Self {
            ranked,
            policy,
            index: 0,
            used: 0,
            same_model: 0,
            pending_same_model: false,
            signatures: BTreeMap::new(),
            stuck: None,
        }
    }

    /// Build a ladder that tries the candidates in the given order.
    #[must_use]
    pub fn from_candidates(candidates: &[String], policy: Policy) -> Self {
        Self::new(candidates.to_vec(), policy)
    }

    /// The model currently selected, if any.
    #[must_use]
    pub fn current_model(&self) -> Option<&str> {
        self.ranked.get(self.index).map(String::as_str)
    }

    /// Attempts used so far.
    #[must_use]
    pub const fn used(&self) -> usize {
        self.used
    }

    /// The next attempt, or why the ladder stopped.
    pub fn next_attempt(&mut self) -> NextAttempt {
        if let Some(signature) = &self.stuck {
            return NextAttempt::Stop(StopReason::Stuck {
                signature: signature.clone(),
            });
        }
        if self.used >= self.policy.max_attempts || self.index >= self.ranked.len() {
            return NextAttempt::Stop(StopReason::Exhausted);
        }
        let model = self.ranked[self.index].clone();
        self.used = self.used.saturating_add(1);
        if self.pending_same_model {
            self.same_model = self.same_model.saturating_add(1);
            self.pending_same_model = false;
        } else {
            self.same_model = 1;
        }
        NextAttempt::Attempt(Attempt {
            model,
            number: u32::try_from(self.used).unwrap_or(u32::MAX),
        })
    }

    /// Record how an attempt failed and decide what that means for the ladder.
    pub fn record(&mut self, model: &str, kind: FailureKind, message: &str) {
        if kind == FailureKind::Executor {
            // The executor could not run at all; a different model would hit
            // the same wall.
            self.stuck = Some("executor".to_string());
            return;
        }
        let signature = signature(kind, message);
        if self.policy.stop_on_repeat {
            match self.signatures.get(&signature) {
                Some(previous) if previous != model => {
                    self.stuck = Some(signature);
                    return;
                }
                Some(_) => {}
                None => {
                    self.signatures.insert(signature, model.to_string());
                }
            }
        }
        let retry_same = kind == FailureKind::Transient
            && self.policy.retry_transient
            && self.same_model < self.policy.max_same_model;
        if retry_same {
            self.pending_same_model = true;
        } else {
            self.index = self.index.saturating_add(1);
            self.same_model = 0;
        }
    }
}

/// A stable, human-readable failure signature.
///
/// Two different models can word the same failure differently; the signature
/// keeps enough of the message to recognise a repeat without pretending to
/// understand it.
#[must_use]
pub fn signature(kind: FailureKind, message: &str) -> String {
    let prefix = match kind {
        FailureKind::Transient => "transient",
        FailureKind::Timeout => "timeout",
        FailureKind::Semantic => "semantic",
        FailureKind::Executor => "executor",
    };
    if kind == FailureKind::Timeout {
        return prefix.to_string();
    }
    let normalised: String = message
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == ' ' {
                character
            } else {
                ' '
            }
        })
        .collect();
    let condensed = normalised.split_whitespace().collect::<Vec<_>>().join(" ");
    let tail: String = condensed.chars().take(160).collect();
    if tail.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}: {tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::ModelPrice;

    fn catalogue() -> PriceCatalogue {
        let mut models = BTreeMap::new();
        models.insert(
            "cheap".to_string(),
            ModelPrice {
                input_miss: MicroUsd::from_micros(100_000),
                input_hit: MicroUsd::from_micros(10_000),
                output: MicroUsd::from_micros(200_000),
            },
        );
        models.insert(
            "mid".to_string(),
            ModelPrice {
                input_miss: MicroUsd::from_micros(300_000),
                input_hit: MicroUsd::from_micros(30_000),
                output: MicroUsd::from_micros(600_000),
            },
        );
        models.insert(
            "capable".to_string(),
            ModelPrice {
                input_miss: MicroUsd::from_micros(1_000_000),
                input_hit: MicroUsd::from_micros(100_000),
                output: MicroUsd::from_micros(2_000_000),
            },
        );
        PriceCatalogue {
            schema: crate::catalog::PRICE_SCHEMA.to_string(),
            models,
        }
    }

    fn candidates() -> Vec<String> {
        vec!["capable".into(), "cheap".into(), "mid".into()]
    }

    #[test]
    fn candidates_rank_cheapest_first_without_history() {
        let ranked = rank_models(
            &catalogue(),
            &candidates(),
            UsageProfile::agent(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            ranked.iter().map(|c| c.model.as_str()).collect::<Vec<_>>(),
            vec!["cheap", "mid", "capable"]
        );
        assert!(ranked[0].expected_cost < ranked[1].expected_cost);
    }

    #[test]
    fn a_cheap_model_that_fails_sinks_below_a_reliable_one() {
        let mut history = BTreeMap::new();
        history.insert(
            "cheap".to_string(),
            ModelHistory {
                calls: 4,
                failures: 4,
                cost: MicroUsd::from_micros(4_000),
            },
        );
        history.insert(
            "capable".to_string(),
            ModelHistory {
                calls: 4,
                failures: 0,
                cost: MicroUsd::from_micros(40_000),
            },
        );
        let ranked =
            rank_models(&catalogue(), &candidates(), UsageProfile::agent(), &history).unwrap();
        let order: Vec<&str> = ranked.iter().map(|c| c.model.as_str()).collect();
        assert!(
            order.iter().position(|m| *m == "capable") < order.iter().position(|m| *m == "cheap"),
            "a model that never succeeds must rank behind a reliable one: {order:?}"
        );
    }

    #[test]
    fn a_cheap_reliable_model_outranks_an_expensive_one() {
        let mut history = BTreeMap::new();
        history.insert(
            "cheap".to_string(),
            ModelHistory {
                calls: 2,
                failures: 0,
                cost: MicroUsd::from_micros(1_000),
            },
        );
        history.insert(
            "capable".to_string(),
            ModelHistory {
                calls: 2,
                failures: 0,
                cost: MicroUsd::from_micros(4_000),
            },
        );
        let ranked =
            rank_models(&catalogue(), &candidates(), UsageProfile::agent(), &history).unwrap();
        assert_eq!(ranked[0].model, "cheap");
    }

    #[test]
    fn an_unpriced_candidate_is_refused() {
        let error = rank_models(
            &catalogue(),
            &["cheap".into(), "mystery".into()],
            UsageProfile::agent(),
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("mystery"));
    }

    #[test]
    fn no_candidates_is_an_error() {
        assert!(rank_models(&catalogue(), &[], UsageProfile::agent(), &BTreeMap::new()).is_err());
    }

    #[test]
    fn a_semantic_failure_escalates_immediately() {
        let mut ladder = Ladder::new(vec!["cheap".into(), "capable".into()], Policy::default());
        let first = ladder.next_attempt();
        assert!(matches!(
            first,
            NextAttempt::Attempt(Attempt { ref model, number: 1 }) if model == "cheap"
        ));
        ladder.record("cheap", FailureKind::Semantic, "tests failed");
        let second = ladder.next_attempt();
        assert!(matches!(
            second,
            NextAttempt::Attempt(Attempt { ref model, number: 2 }) if model == "capable"
        ));
    }

    #[test]
    fn a_transient_failure_retries_the_same_model_once() {
        let mut ladder = Ladder::new(vec!["cheap".into(), "capable".into()], Policy::default());
        ladder.next_attempt();
        ladder.record("cheap", FailureKind::Transient, "429 rate limit");
        let second = ladder.next_attempt();
        assert!(matches!(
            second,
            NextAttempt::Attempt(Attempt { ref model, number: 2 }) if model == "cheap"
        ));
        ladder.record("cheap", FailureKind::Transient, "429 rate limit");
        let third = ladder.next_attempt();
        assert!(matches!(
            third,
            NextAttempt::Attempt(Attempt { ref model, number: 3 }) if model == "capable"
        ));
    }

    #[test]
    fn the_same_failure_on_a_different_model_stops_the_ladder() {
        let mut ladder = Ladder::new(
            vec!["cheap".into(), "capable".into(), "third".into()],
            Policy::default(),
        );
        ladder.next_attempt();
        ladder.record("cheap", FailureKind::Semantic, "acceptance failed");
        ladder.next_attempt();
        ladder.record("capable", FailureKind::Semantic, "acceptance failed");
        assert!(matches!(
            ladder.next_attempt(),
            NextAttempt::Stop(StopReason::Stuck { .. })
        ));
        assert_eq!(ladder.used(), 2);
    }

    #[test]
    fn the_attempt_cap_is_a_hard_stop() {
        let policy = Policy {
            max_attempts: 2,
            ..Policy::default()
        };
        let mut ladder = Ladder::new(vec!["cheap".into(), "mid".into(), "capable".into()], policy);
        assert!(matches!(ladder.next_attempt(), NextAttempt::Attempt(_)));
        assert!(matches!(ladder.next_attempt(), NextAttempt::Attempt(_)));
        ladder.record("mid", FailureKind::Semantic, "no");
        assert_eq!(
            ladder.next_attempt(),
            NextAttempt::Stop(StopReason::Exhausted)
        );
    }

    #[test]
    fn a_timeout_repeat_across_models_stops() {
        let mut ladder = Ladder::new(vec!["cheap".into(), "capable".into()], Policy::default());
        ladder.next_attempt();
        ladder.record("cheap", FailureKind::Timeout, "agent timed out");
        ladder.next_attempt();
        ladder.record("capable", FailureKind::Timeout, "agent timed out");
        assert!(matches!(
            ladder.next_attempt(),
            NextAttempt::Stop(StopReason::Stuck { .. })
        ));
    }

    #[test]
    fn an_executor_error_stops_immediately() {
        let mut ladder = Ladder::new(vec!["cheap".into()], Policy::default());
        ladder.next_attempt();
        ladder.record("cheap", FailureKind::Executor, "spawn failed");
        assert!(matches!(
            ladder.next_attempt(),
            NextAttempt::Stop(StopReason::Stuck { .. })
        ));
    }

    #[test]
    fn failure_messages_are_classified() {
        assert_eq!(
            FailureKind::classify("429 rate limit exceeded"),
            FailureKind::Transient
        );
        assert_eq!(
            FailureKind::classify("provider overloaded"),
            FailureKind::Transient
        );
        assert_eq!(
            FailureKind::classify("request timed out"),
            FailureKind::Timeout
        );
        assert_eq!(
            FailureKind::classify("acceptance failed: 2 tests red"),
            FailureKind::Semantic
        );
    }

    #[test]
    fn signatures_are_stable() {
        assert_eq!(
            signature(FailureKind::Semantic, "Tests failed: 2 red!"),
            signature(FailureKind::Semantic, "tests failed 2 red")
        );
        assert_eq!(signature(FailureKind::Timeout, "a"), "timeout");
    }
}
