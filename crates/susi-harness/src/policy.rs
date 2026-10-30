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
use crate::ledger::{FailureClass, LedgerEntry, Outcome};
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
    /// Of those, input tokens a caching model serves from its prompt cache.
    pub cached: u64,
    /// Output tokens one typical turn produces.
    pub output: u64,
}

impl UsageProfile {
    /// A typical agent turn: 200k input, 20k output, and three quarters of
    /// the input served from cache by a model that caches. The harness
    /// re-sends one growing prompt on every turn of a task, so most of it is
    /// a prefix the provider has already seen.
    #[must_use]
    pub const fn agent() -> Self {
        Self {
            input: 200_000,
            cached: 150_000,
            output: 20_000,
        }
    }

    /// The usage this profile stands for on a model that does, or does not,
    /// serve a cached prefix at the cache price.
    #[must_use]
    pub const fn usage(self, caches: bool) -> crate::pricing::TokenUsage {
        let cached = if caches { self.cached } else { 0 };
        crate::pricing::TokenUsage::new(self.input, cached, self.output)
    }
}

impl Default for UsageProfile {
    fn default() -> Self {
        Self::agent()
    }
}

/// What the ledger has taught us about one model, attempt by attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelHistory {
    /// Attempts: the ledger rows sharing a mission, task, attempt number and
    /// model are one attempt, however many calls it made.
    pub calls: u64,
    /// Attempts with a failure row: the provider failed, or the work the
    /// attempt produced was rejected.
    pub failures: u64,
    /// Total spend on this model.
    pub cost: MicroUsd,
    /// Input tokens the provider read, over every attempt.
    pub input: u64,
    /// Of those, input tokens the provider served from its prompt cache.
    pub cache_hit: u64,
    /// Wall time of the completed provider calls, in milliseconds, summed.
    pub latency_ms: u64,
    /// How many completed provider calls `latency_ms` sums.
    pub timed_calls: u64,
}

/// Attempts a model needs on record before its failure rate can demote it.
pub const RELIABILITY_EVIDENCE: u64 = 3;

impl ModelHistory {
    /// Attempts that did not fail.
    #[must_use]
    pub const fn successes(self) -> u64 {
        self.calls.saturating_sub(self.failures)
    }

    /// The share of input the model served from cache, in parts per
    /// thousand, once it has read any input.
    #[must_use]
    pub const fn cache_share_permille(self) -> Option<u64> {
        self.cache_hit.saturating_mul(1000).checked_div(self.input)
    }

    /// The mean wall time of one completed provider call, once any is on
    /// record. A call the gateway answered from its own cache is not timed.
    #[must_use]
    pub const fn mean_latency_ms(self) -> Option<u64> {
        self.latency_ms.checked_div(self.timed_calls)
    }

    /// Whether the record shows the model failing most of its attempts:
    /// at least [`RELIABILITY_EVIDENCE`] attempts, under half of them good.
    #[must_use]
    pub const fn unreliable(self) -> bool {
        self.calls >= RELIABILITY_EVIDENCE && self.successes().saturating_mul(2) < self.calls
    }
}

/// Learn per-model history from every settled row.
///
/// Rows are grouped into attempts first, so a model is judged on attempts
/// that paid off, not on how many calls each attempt happened to make. A row
/// the gateway answered from its own response cache did not reach the
/// provider and says nothing about the provider's prompt cache.
#[must_use]
pub fn history(entries: &[LedgerEntry]) -> BTreeMap<String, ModelHistory> {
    let mut attempts: BTreeMap<(&str, &str, u32, &str), ModelHistory> = BTreeMap::new();
    for entry in entries {
        let key = (
            entry.mission.as_str(),
            entry.task.as_str(),
            entry.attempt,
            entry.model.as_str(),
        );
        let slot = attempts.entry(key).or_default();
        slot.calls = 1;
        slot.cost = slot
            .cost
            .checked_add(entry.cost)
            .unwrap_or(MicroUsd::from_micros(u64::MAX));
        if matches!(
            entry.outcome,
            Outcome::ModelFailed { .. } | Outcome::Rejected { .. }
        ) {
            slot.failures = 1;
        }
        if !entry.cached {
            slot.input = slot.input.saturating_add(entry.usage.input);
            slot.cache_hit = slot.cache_hit.saturating_add(entry.usage.cache_hit);
            if entry.outcome == Outcome::Completed {
                let took = entry.ended_unix_ms.saturating_sub(entry.started_unix_ms);
                slot.latency_ms = slot.latency_ms.saturating_add(took);
                slot.timed_calls = slot.timed_calls.saturating_add(1);
            }
        }
    }
    let mut history: BTreeMap<String, ModelHistory> = BTreeMap::new();
    for ((_, _, _, model), attempt) in attempts {
        let slot = history.entry(model.to_string()).or_default();
        slot.calls = slot.calls.saturating_add(attempt.calls);
        slot.failures = slot.failures.saturating_add(attempt.failures);
        slot.cost = slot
            .cost
            .checked_add(attempt.cost)
            .unwrap_or(MicroUsd::from_micros(u64::MAX));
        slot.input = slot.input.saturating_add(attempt.input);
        slot.cache_hit = slot.cache_hit.saturating_add(attempt.cache_hit);
        slot.latency_ms = slot.latency_ms.saturating_add(attempt.latency_ms);
        slot.timed_calls = slot.timed_calls.saturating_add(attempt.timed_calls);
    }
    history
}

/// Each model's mean provider-call latency, from the ledger, keyed by the
/// registry id ([`canonical`]).
///
/// A client of the gateway may name a model by an alias (`coder`) or with the
/// `susi/` prefix, and the ledger keeps the name it used; the fleet ranks by
/// registry id, so the calls under every name of one model are one record.
#[must_use]
pub fn latencies(entries: &[LedgerEntry]) -> BTreeMap<String, u64> {
    let mut timed: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for (model, record) in history(entries) {
        let slot = timed.entry(canonical(&model)).or_default();
        slot.0 = slot.0.saturating_add(record.latency_ms);
        slot.1 = slot.1.saturating_add(record.timed_calls);
    }
    timed
        .into_iter()
        .filter_map(|(model, (total, calls))| Some((model, total.checked_div(calls)?)))
        .collect()
}

/// The registry id a ledger row's model name stands for: an alias is
/// followed and a `susi/` prefix dropped. A name the registry does not know
/// is kept as written.
#[must_use]
pub fn canonical(model: &str) -> String {
    crate::providers::Registry::builtin()
        .canonical_id(model)
        .to_string()
}

/// How long a provider failure is remembered for cooling down.
pub const COOLDOWN_MS: u64 = 10 * 60 * 1000;

/// Providers that failed recently and often enough that a new task should
/// not start on them.
///
/// Only provider failures count (`ModelFailed` rows, which the gateway
/// writes), never rejected work: a rejection is about the task. Out of
/// credit cools a provider at once, and so does a rate limit: the one is a
/// balance that will not refill inside this run, the other a window the
/// provider closed on us, and a retry that spends the rest of the attempt on
/// either is exactly how a live mission died — twenty-four served turns, one
/// 429, then the next attempt met the same refusal. Any other provider
/// failure cools it at the second in a row. A completed call clears the
/// record, because the provider has recovered. Only the last
/// [`COOLDOWN_MS`] are read.
#[must_use]
pub fn cooling_providers(entries: &[LedgerEntry], now_unix_ms: u64) -> BTreeSet<String> {
    let mut recent = entries
        .iter()
        .filter(|entry| entry.ended_unix_ms.saturating_add(COOLDOWN_MS) >= now_unix_ms)
        .collect::<Vec<_>>();
    recent.sort_by_key(|entry| entry.ended_unix_ms);
    let mut streaks: BTreeMap<&str, (u32, bool)> = BTreeMap::new();
    for entry in recent {
        match &entry.outcome {
            Outcome::ModelFailed { class } => {
                let streak = streaks.entry(entry.provider.as_str()).or_default();
                streak.0 = streak.0.saturating_add(1);
                streak.1 |= matches!(*class, FailureClass::NoCredit | FailureClass::RateLimited);
            }
            Outcome::Completed if !entry.cached => {
                streaks.remove(entry.provider.as_str());
            }
            Outcome::Completed
            | Outcome::Accepted
            | Outcome::Rejected { .. }
            | Outcome::Cancelled => {}
        }
    }
    streaks
        .into_iter()
        .filter(|(_, (failures, no_credit))| *no_credit || *failures >= 2)
        .map(|(provider, _)| provider.to_string())
        .collect()
}

/// How a model's most recent real call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFailure {
    /// When the call ended, in milliseconds since the Unix epoch.
    pub at_unix_ms: u64,
    /// The provider's answer, as the gateway or the probe classed it.
    pub class: FailureClass,
}

impl ModelFailure {
    /// Whether waiting cannot change the answer: the provider refused the key
    /// or has no credit for it (`NoCredit`, which the gateway writes for 401,
    /// 402 and 403 and for an "insufficient balance" body). Such a model is
    /// inactive until a later real call succeeds, which only a probe makes
    /// once the model is off the ladder.
    ///
    /// Every other class is transient: a rate limit, a timeout, an overloaded
    /// or failing provider (5xx), a dropped connection. One such answer used to
    /// take the model off the ladder for the rest of a mission, so a fleet of
    /// one model failed every remaining task without a call after a single
    /// 429. A transient failure now only moves the model back for
    /// [`COOLDOWN_MS`] ([`arrange`], [`in_force`]).
    #[must_use]
    pub fn definitive(&self) -> bool {
        self.class == FailureClass::NoCredit
    }
}

/// Models whose most recent real call failed, with when and how, keyed by the
/// registry id ([`canonical`]).
///
/// The actual API calls are the source of truth for whether a model works: a
/// model whose last call to its provider failed is judged on that call,
/// whatever its budget, balance or listing says, until a later real call
/// succeeds (a task's call, or [`crate::fleet`]'s probe). A definitive failure
/// ([`ModelFailure::definitive`]) makes it inactive; a transient one cools it
/// down. Only provider calls count: a gateway row (`Completed` or
/// `ModelFailed`), never a task verdict, since rejected work is about the
/// task, and never an answer the gateway's own cache gave.
///
/// Rows are grouped by registry id because a gateway client may call a model
/// by an alias or with the `susi/` prefix, and the gateway records the name
/// the client used: a refused `coder` call must take `deepseek-flash` off the
/// fleet, and a later good `coder` call must bring it back.
#[must_use]
pub fn failed_models(entries: &[LedgerEntry]) -> BTreeMap<String, ModelFailure> {
    let mut last: BTreeMap<String, (u64, Option<FailureClass>)> = BTreeMap::new();
    for entry in entries {
        let failure = match &entry.outcome {
            Outcome::Completed if !entry.cached => None,
            Outcome::ModelFailed { class } => Some(*class),
            Outcome::Completed
            | Outcome::Accepted
            | Outcome::Rejected { .. }
            | Outcome::Cancelled => continue,
        };
        let slot = last.entry(canonical(&entry.model)).or_insert((0, None));
        if entry.ended_unix_ms >= slot.0 {
            *slot = (entry.ended_unix_ms, failure);
        }
    }
    last.into_iter()
        .filter_map(|(model, (at_unix_ms, class))| {
            Some((
                model,
                ModelFailure {
                    at_unix_ms,
                    class: class?,
                },
            ))
        })
        .collect()
}

/// The failures that still decide a task's order at `now_unix_ms`: every
/// definitive one, and a transient one for [`COOLDOWN_MS`] after it ended. A
/// rate limit an hour ago says nothing about the model now, and a model at
/// the back of the ladder may not be called again to clear it.
#[must_use]
pub fn in_force(
    failures: BTreeMap<String, ModelFailure>,
    now_unix_ms: u64,
) -> BTreeMap<String, ModelFailure> {
    failures
        .into_iter()
        .filter(|(_, failure)| {
            failure.definitive() || failure.at_unix_ms.saturating_add(COOLDOWN_MS) >= now_unix_ms
        })
        .collect()
}

/// Where a task goes after its first model, from the fleet's ladder: the
/// strong models first, then the other light ones. A light worker that
/// failed, or produced work that did not pass (a hallucination shows up
/// exactly there), is followed by a model strong enough to fix it, not by
/// another light model of the same reach. The fleet's order holds within each
/// group, and the light models stay as a last resort.
#[must_use]
pub fn escalation_order(ladder: &[String], strong: &dyn Fn(&str) -> bool) -> Vec<String> {
    let (fixers, workers): (Vec<String>, Vec<String>) =
        ladder.iter().cloned().partition(|model| strong(model));
    fixers.into_iter().chain(workers).collect()
}

/// What the ledger says about each model and provider, for [`arrange`].
#[derive(Debug, Clone, Copy)]
pub struct Standing<'a> {
    /// Per-model attempt record ([`history`]).
    pub history: &'a BTreeMap<String, ModelHistory>,
    /// Providers cooling down ([`cooling_providers`]).
    pub cooling: &'a BTreeSet<String>,
    /// Models whose last real call failed ([`failed_models`]), keyed by
    /// registry id. A caller with a clock passes only the failures still in
    /// force ([`in_force`]).
    pub failed: &'a BTreeMap<String, ModelFailure>,
}

/// The order one task tries its models in.
///
/// The fleet's ladder is the policy: it already ranks expiring credit, cache,
/// price and capability. The task's own model goes first, then the ladder in
/// its order. A model whose last real call failed definitively
/// ([`ModelFailure::definitive`]: the key was refused or has no credit) is
/// left out: the calls decide. The rest only move back, and their order is
/// otherwise kept: a model whose record is [`ModelHistory::unreliable`] goes
/// behind the healthy ones, and a model whose provider is cooling down
/// ([`cooling_providers`]) or whose own last call failed transiently (a rate
/// limit, a timeout, a 5xx) goes to the end. A transient failure never takes
/// a model off: with one model, that model still runs.
#[must_use]
pub fn arrange(
    preferred: &str,
    ladder: &[String],
    standing: &Standing<'_>,
    provider_of: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let Standing {
        history,
        cooling,
        failed,
    } = *standing;
    let failure = |model: &str| failed.get(&canonical(model));
    let mut models = vec![preferred.to_string()];
    for model in ladder {
        if !models.contains(model) {
            models.push(model.clone());
        }
    }
    models.retain(|model| !failure(model).is_some_and(ModelFailure::definitive));
    let standing = |model: &String| -> u8 {
        if failure(model).is_some()
            || provider_of(model).is_some_and(|provider| cooling.contains(&provider))
        {
            2
        } else if history.get(model).is_some_and(|record| record.unreliable()) {
            1
        } else {
            0
        }
    };
    // A stable sort: within a standing, the fleet's order holds.
    models.sort_by_key(standing);
    models
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
/// A candidate the catalogue cannot price still ranks: it goes last, because
/// a model that works must not be dropped for lack of a price, and a missing
/// price must never make it the cheapest choice.
///
/// # Errors
///
/// Returns `EaiError::config` when there is no candidate at all.
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
        let record = history.get(model).copied().unwrap_or_default();
        let expected = match catalogue.price(model) {
            Some(price) => {
                // The cache share the model has shown, priced at its cache
                // rate; a model with no record is priced as all misses.
                let cached = record
                    .cache_share_permille()
                    .map_or(0, |share| profile.input.saturating_mul(share) / 1000);
                let base = price.cost(crate::pricing::TokenUsage::new(
                    profile.input,
                    cached,
                    profile.output,
                ));
                expected_cost(base, record)
            }
            None => MicroUsd::from_micros(u64::MAX),
        };
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
    /// The prompt did not fit the model's context window. Only a model with
    /// a larger window can take the task further.
    ContextOverflow,
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
        // How the providers word a prompt that is longer than the window:
        // OpenAI-compatible `context_length_exceeded` and "maximum context
        // length", Anthropic "prompt is too long", Gemini "exceeds the
        // maximum number of tokens" / "input token count".
        const CONTEXT: &[&str] = &[
            "context_length_exceeded",
            "context length",
            "maximum context",
            "context window",
            "prompt is too long",
            "input is too long",
            "exceeds the maximum number of tokens",
            "input token count",
        ];
        if CONTEXT.iter().any(|needle| text.contains(needle)) {
            return Self::ContextOverflow;
        }
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
    contexts: BTreeMap<String, u64>,
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
            contexts: BTreeMap::new(),
        }
    }

    /// Tell the ladder each model's context window, in tokens, so a prompt
    /// that overflowed one model goes next to a model that can hold more. A
    /// model left out has an unknown window and is not skipped.
    #[must_use]
    pub fn with_contexts(mut self, contexts: BTreeMap<String, u64>) -> Self {
        self.contexts = contexts;
        self
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
        if kind == FailureKind::ContextOverflow {
            // Retrying, or trying a model with no more room, would fail the
            // same way. Skip to the next model with a larger window, in the
            // ladder's order; a model with an unknown window may be larger.
            let held = self.contexts.get(model).copied().unwrap_or(0);
            let next = self
                .ranked
                .iter()
                .enumerate()
                .skip(self.index.saturating_add(1))
                .find(|(_, candidate)| {
                    self.contexts
                        .get(candidate.as_str())
                        .is_none_or(|window| *window > held)
                })
                .map(|(position, _)| position);
            match next {
                Some(position) => {
                    self.index = position;
                    self.same_model = 0;
                }
                None => {
                    self.stuck = Some(format!(
                        "context: no model on the ladder has a larger window than {model}"
                    ));
                }
            }
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
        FailureKind::ContextOverflow => "context",
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
                ..ModelHistory::default()
            },
        );
        history.insert(
            "capable".to_string(),
            ModelHistory {
                calls: 4,
                failures: 0,
                cost: MicroUsd::from_micros(40_000),
                ..ModelHistory::default()
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
                ..ModelHistory::default()
            },
        );
        history.insert(
            "capable".to_string(),
            ModelHistory {
                calls: 2,
                failures: 0,
                cost: MicroUsd::from_micros(4_000),
                ..ModelHistory::default()
            },
        );
        let ranked =
            rank_models(&catalogue(), &candidates(), UsageProfile::agent(), &history).unwrap();
        assert_eq!(ranked[0].model, "cheap");
    }

    #[test]
    fn an_unpriced_candidate_ranks_last_instead_of_failing_the_run() {
        let ranked = rank_models(
            &catalogue(),
            &["mystery".into(), "cheap".into()],
            UsageProfile::agent(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(ranked[0].model, "cheap");
        assert_eq!(ranked[1].model, "mystery");
    }

    fn row(
        (task, attempt): (&str, u32),
        model: &str,
        provider: &str,
        at_ms: u64,
        outcome: Outcome,
    ) -> LedgerEntry {
        LedgerEntry::settle(crate::ledger::Dispatch {
            mission: "m".to_string(),
            task: task.to_string(),
            attempt,
            model: model.to_string(),
            provider: provider.to_string(),
            usage: crate::pricing::TokenUsage::new(1000, 800, 100),
            price: ModelPrice::default(),
            started_unix_ms: at_ms,
            ended_unix_ms: at_ms,
            outcome,
        })
    }

    fn rejected() -> Outcome {
        Outcome::Rejected {
            reason: "tests red".to_string(),
        }
    }

    fn failed(class: FailureClass) -> Outcome {
        Outcome::ModelFailed { class }
    }

    #[test]
    fn provider_context_errors_are_classified_as_overflow() {
        for message in [
            "400: This model's maximum context length is 128000 tokens",
            r#"{"error":{"code":"context_length_exceeded"}}"#,
            "prompt is too long: 210000 tokens > 200000 maximum",
            "The input token count (1100000) exceeds the maximum number of tokens allowed",
        ] {
            assert_eq!(
                FailureKind::classify(message),
                FailureKind::ContextOverflow,
                "{message}"
            );
        }
        assert_eq!(
            FailureKind::classify("Rate limit reached for tokens per min"),
            FailureKind::Transient
        );
    }

    #[test]
    fn a_context_overflow_skips_to_a_larger_window() {
        let ranked: Vec<String> = ["small", "also-small", "unknown", "large"]
            .into_iter()
            .map(String::from)
            .collect();
        let contexts: BTreeMap<String, u64> = [
            ("small".to_string(), 128_000),
            ("also-small".to_string(), 128_000),
            ("large".to_string(), 1_000_000),
        ]
        .into_iter()
        .collect();
        let mut ladder =
            Ladder::new(ranked.clone(), Policy::default()).with_contexts(contexts.clone());
        let NextAttempt::Attempt(first) = ladder.next_attempt() else {
            panic!("expected an attempt");
        };
        ladder.record(&first.model, FailureKind::ContextOverflow, "context length");
        let NextAttempt::Attempt(second) = ladder.next_attempt() else {
            panic!("expected an attempt");
        };
        // `also-small` holds no more, so it is skipped; an unknown window may.
        assert_eq!(second.model, "unknown");
        ladder.record(
            &second.model,
            FailureKind::ContextOverflow,
            "context length",
        );
        let NextAttempt::Attempt(third) = ladder.next_attempt() else {
            panic!("expected an attempt");
        };
        assert_eq!(third.model, "large");
        ladder.record(&third.model, FailureKind::ContextOverflow, "context length");
        assert!(matches!(
            ladder.next_attempt(),
            NextAttempt::Stop(StopReason::Stuck { .. })
        ));
    }

    #[test]
    fn latency_is_the_mean_of_completed_provider_calls() {
        let timed = |at: u64, took: u64, outcome: Outcome| {
            let mut entry = row(("T", 1), "m", "p", at, outcome);
            entry.started_unix_ms = at;
            entry.ended_unix_ms = at + took;
            entry
        };
        let mut cached = timed(50, 1, Outcome::Completed);
        cached.cached = true;
        let entries = vec![
            timed(0, 300, Outcome::Completed),
            timed(1_000, 500, Outcome::Completed),
            // Neither a failure nor a gateway-cache answer is a timed call.
            timed(2_000, 90_000, failed(FailureClass::Timeout)),
            cached,
        ];
        assert_eq!(history(&entries)["m"].mean_latency_ms(), Some(400));
        assert_eq!(latencies(&entries).get("m"), Some(&400));
        assert_eq!(ModelHistory::default().mean_latency_ms(), None);
    }

    #[test]
    fn latency_under_an_alias_counts_for_the_registry_model() {
        let timed = |model: &str, task: &str, took: u64| {
            let mut entry = row((task, 1), model, "deepseek", 0, Outcome::Completed);
            entry.ended_unix_ms = took;
            entry
        };
        let entries = vec![
            timed("deepseek-flash", "A", 100),
            timed("coder", "B", 300),
            timed("susi/deepseek-flash", "C", 500),
        ];
        let measured = latencies(&entries);
        assert_eq!(measured.get("deepseek-flash"), Some(&300), "{measured:?}");
        assert!(!measured.contains_key("coder"), "{measured:?}");
    }

    #[test]
    fn a_light_workers_failure_goes_to_the_strong_fixers_first() {
        let ladder: Vec<String> = [
            "deepseek-flash",
            "gemini-flash",
            "kimi-flash",
            "deepseek-pro",
            "gemini-pro",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let strong = |model: &str| model.ends_with("-pro");
        let order = escalation_order(&ladder, &strong);
        assert_eq!(
            order,
            vec![
                "deepseek-pro",
                "gemini-pro",
                "deepseek-flash",
                "gemini-flash",
                "kimi-flash"
            ]
        );
        // A task on gemini-flash: its own light model first, then the fixers.
        let task = arrange(
            "gemini-flash",
            &order,
            &Standing {
                history: &BTreeMap::new(),
                cooling: &BTreeSet::new(),
                failed: &BTreeMap::new(),
            },
            &|_| None,
        );
        assert_eq!(&task[..3], &["gemini-flash", "deepseek-pro", "gemini-pro"]);
    }

    #[test]
    fn a_models_last_real_call_decides_whether_it_works() {
        let entries = vec![
            // Failed, then recovered: works.
            row(
                ("T", 1),
                "healed",
                "p",
                10,
                failed(FailureClass::RateLimited),
            ),
            row(("T", 1), "healed", "p", 20, Outcome::Completed),
            // Worked, then its key was refused: inactive, whatever else is
            // true of it.
            row(("T", 1), "broken", "p", 10, Outcome::Completed),
            row(("T", 1), "broken", "p", 30, failed(FailureClass::NoCredit)),
            // Worked, then hit an overload: it failed, but only for now.
            row(("T", 1), "busy", "p", 10, Outcome::Completed),
            row(("T", 1), "busy", "p", 35, failed(FailureClass::Other)),
            // A rejected task is about the task, not the model's API.
            row(("T", 1), "judged", "p", 10, Outcome::Completed),
            row(("T", 1), "judged", "p", 40, rejected()),
        ];
        let failed = failed_models(&entries);
        assert_eq!(failed.keys().collect::<Vec<_>>(), vec!["broken", "busy"]);
        assert_eq!(failed["broken"].at_unix_ms, 30);
        assert!(failed["broken"].definitive());
        assert!(!failed["busy"].definitive());
        // A model whose key was refused is left out of a task's order, even
        // the task's own; one that failed transiently only goes to the back.
        let ladder = vec![
            "broken".to_string(),
            "busy".to_string(),
            "healed".to_string(),
        ];
        let order = arrange(
            "broken",
            &ladder,
            &Standing {
                history: &BTreeMap::new(),
                cooling: &BTreeSet::new(),
                failed: &failed,
            },
            &|_| None,
        );
        assert_eq!(order, vec!["healed".to_string(), "busy".to_string()]);
    }

    /// The observed failure: one model, one 429 that outlived the same-model
    /// retry, and every later task of the mission failed with no call at all,
    /// because the model's last row was a failure and it was dropped.
    #[test]
    fn a_fleet_of_one_model_still_runs_it_after_a_transient_failure() {
        for class in [
            FailureClass::RateLimited,
            FailureClass::Timeout,
            FailureClass::Other,
        ] {
            let entries = vec![
                row(("T1", 1), "only", "p", 10, Outcome::Completed),
                row(("T2", 2), "only", "p", 20, failed(class)),
            ];
            let order = arrange(
                "only",
                &["only".to_string()],
                &Standing {
                    history: &history(&entries),
                    cooling: &cooling_providers(&entries, 20),
                    failed: &failed_models(&entries),
                },
                &|_| Some("p".to_string()),
            );
            assert_eq!(order, vec!["only".to_string()], "{class:?}");
        }
    }

    #[test]
    fn a_transient_failure_cools_for_the_cooldown_and_a_refusal_lasts() {
        let entries = vec![
            row(
                ("T", 1),
                "busy",
                "p",
                1_000,
                failed(FailureClass::RateLimited),
            ),
            row(
                ("T", 1),
                "refused",
                "q",
                1_000,
                failed(FailureClass::NoCredit),
            ),
        ];
        let soon = in_force(failed_models(&entries), 1_000 + COOLDOWN_MS);
        assert_eq!(soon.keys().collect::<Vec<_>>(), vec!["busy", "refused"]);
        let later = in_force(failed_models(&entries), 1_001 + COOLDOWN_MS);
        assert_eq!(later.keys().collect::<Vec<_>>(), vec!["refused"]);
    }

    /// A gateway client may call a model by an alias or with the `susi/`
    /// prefix; the ledger keeps that name, but the fleet asks by registry id.
    #[test]
    fn a_refused_call_through_an_alias_takes_the_registry_model_off() {
        let refused = vec![row(
            ("T", 1),
            "coder",
            "deepseek",
            10,
            failed(FailureClass::NoCredit),
        )];
        let failed = failed_models(&refused);
        assert!(failed["deepseek-flash"].definitive(), "{failed:?}");
        let order = arrange(
            "deepseek-flash",
            &["deepseek-flash".to_string(), "deepseek-pro".to_string()],
            &Standing {
                history: &BTreeMap::new(),
                cooling: &BTreeSet::new(),
                failed: &failed,
            },
            &|_| None,
        );
        assert_eq!(order, vec!["deepseek-pro".to_string()]);
        // A later good call under another name brings it back.
        let mut healed = refused.clone();
        healed.push(row(
            ("T", 2),
            "susi/deepseek-flash",
            "deepseek",
            20,
            Outcome::Completed,
        ));
        assert!(failed_models(&healed).is_empty());
    }

    #[test]
    fn history_counts_attempts_not_calls() {
        // Attempt 1 made three calls and was rejected; attempt 2 made two
        // calls and was accepted. Two attempts, one failure.
        let entries = vec![
            row(("T", 1), "cheap", "p", 1, Outcome::Completed),
            row(("T", 1), "cheap", "p", 2, Outcome::Completed),
            row(("T", 1), "cheap", "p", 3, Outcome::Completed),
            row(("T", 1), "cheap", "p", 4, rejected()),
            row(("T", 2), "cheap", "p", 5, Outcome::Completed),
            row(("T", 2), "cheap", "p", 6, Outcome::Completed),
            row(("T", 2), "cheap", "p", 7, Outcome::Accepted),
        ];
        let record = history(&entries)["cheap"];
        assert_eq!(record.calls, 2);
        assert_eq!(record.failures, 1);
        // Every row read 1000 input with 800 from cache.
        assert_eq!(record.cache_share_permille(), Some(800));
    }

    #[test]
    fn a_model_failing_most_attempts_is_unreliable_only_with_evidence() {
        let two_failures = vec![
            row(("A", 1), "m", "p", 1, rejected()),
            row(("B", 1), "m", "p", 2, rejected()),
        ];
        assert!(
            !history(&two_failures)["m"].unreliable(),
            "two attempts are too few"
        );
        let mut three = two_failures.clone();
        three.push(row(("C", 1), "m", "p", 3, Outcome::Accepted));
        assert!(history(&three)["m"].unreliable(), "one in three succeeded");
        three.push(row(("D", 1), "m", "p", 4, Outcome::Accepted));
        assert!(!history(&three)["m"].unreliable(), "half succeeded");
    }

    #[test]
    fn a_measured_cache_share_lowers_the_expected_cost() {
        let mut cached = BTreeMap::new();
        cached.insert(
            "cheap".to_string(),
            ModelHistory {
                input: 1000,
                cache_hit: 900,
                ..ModelHistory::default()
            },
        );
        let plain = rank_models(
            &catalogue(),
            &["cheap".into()],
            UsageProfile::agent(),
            &BTreeMap::new(),
        )
        .unwrap();
        let warm = rank_models(
            &catalogue(),
            &["cheap".into()],
            UsageProfile::agent(),
            &cached,
        )
        .unwrap();
        assert!(warm[0].expected_cost < plain[0].expected_cost);
    }

    #[test]
    fn providers_cool_down_at_once_on_no_credit_or_a_rate_limit_and_on_two_other_failures() {
        let now = 1_000_000_000;
        let entries = vec![
            row(
                ("T", 1),
                "a1",
                "broke",
                now - 1000,
                failed(FailureClass::NoCredit),
            ),
            row(
                ("T", 1),
                "b1",
                "busy",
                now - 3000,
                failed(FailureClass::RateLimited),
            ),
            row(
                ("T", 1),
                "b1",
                "busy",
                now - 2000,
                failed(FailureClass::RateLimited),
            ),
            // One rate limit is enough: the provider closed its window, and a
            // mission that kept retrying it lost every served turn before it.
            row(
                ("T", 1),
                "g1",
                "throttled",
                now - 500,
                failed(FailureClass::RateLimited),
            ),
            row(
                ("T", 1),
                "c1",
                "blip",
                now - 1000,
                failed(FailureClass::Other),
            ),
            row(
                ("T", 1),
                "d1",
                "healed",
                now - 3000,
                failed(FailureClass::Other),
            ),
            row(
                ("T", 1),
                "d1",
                "healed",
                now - 2000,
                failed(FailureClass::Other),
            ),
            row(("T", 1), "d1", "healed", now - 1000, Outcome::Completed),
            row(
                ("T", 1),
                "e1",
                "old",
                now - COOLDOWN_MS - 1,
                failed(FailureClass::NoCredit),
            ),
            row(("T", 1), "f1", "rejected", now - 1000, rejected()),
        ];
        let cooling = cooling_providers(&entries, now);
        assert_eq!(
            cooling.into_iter().collect::<Vec<_>>(),
            vec![
                "broke".to_string(),
                "busy".to_string(),
                "throttled".to_string()
            ]
        );
    }

    #[test]
    fn arrange_keeps_the_fleet_order_and_only_demotes() {
        let ladder: Vec<String> = ["fast", "cached", "rescue", "cooling", "flaky"]
            .into_iter()
            .map(String::from)
            .collect();
        let mut records = BTreeMap::new();
        records.insert(
            "flaky".to_string(),
            ModelHistory {
                calls: 4,
                failures: 3,
                ..ModelHistory::default()
            },
        );
        records.insert(
            "rescue".to_string(),
            ModelHistory {
                calls: 4,
                failures: 3,
                ..ModelHistory::default()
            },
        );
        let cooling: BTreeSet<String> = ["down".to_string()].into_iter().collect();
        let provider_of =
            |model: &str| Some(if model == "cooling" { "down" } else { "up" }.to_string());
        let order = arrange(
            "cached",
            &ladder,
            &Standing {
                history: &records,
                cooling: &cooling,
                failed: &BTreeMap::new(),
            },
            &provider_of,
        );
        // The preferred model leads, healthy models keep the fleet's order,
        // the unreliable ones follow in the fleet's order, cooling is last.
        assert_eq!(order, vec!["cached", "fast", "rescue", "flaky", "cooling"]);
        // A fleet of one cooling model still runs it.
        let alone = arrange(
            "cooling",
            &[],
            &Standing {
                history: &records,
                cooling: &cooling,
                failed: &BTreeMap::new(),
            },
            &provider_of,
        );
        assert_eq!(alone, vec!["cooling"]);
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
