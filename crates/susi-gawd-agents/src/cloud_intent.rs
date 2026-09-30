//! Intent-driven selection of cloud (credential, model) candidates.
//!
//! Before each real intent invocation, candidates are **filtered** by what
//! the request actually needs (context size, modality, tools, structured
//! output, residency, deadline, budget) and by *current* availability
//! evidence — eligibility state and quota headroom recorded from real
//! outcomes. Only then are the survivors **ranked** by recorded per-task-class
//! quality, latency, and allowed cost.
//!
//! Invariants (mandate 56):
//! - A preferred or historically strong model never overrides a current
//!   availability block — nonworking targets are filtered before ranking.
//! - Free status alone never overrides a required capability or better
//!   recorded quality.
//! - `Unknown` availability is not assumed availability: unknown candidates
//!   rank below proven ones and are bounded by a discovery budget.
//! - Explicit model pins are honored; fallback off a pin is a declared
//!   policy, not a silent default.
//! - No credential material ever appears in outcomes or rejections — only
//!   opaque fingerprints.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use susi_vendor_models::cloud_eligibility::{
    credential_fingerprint, EligibilityKind, EligibilityStore, InferenceResult, Subject,
};
use susi_vendor_models::cloud_quota::{self};

/// What a user intent needs from a model. Every field is a hard filter;
/// `None`/`false`/0 means "not required".
#[derive(Debug, Clone)]
pub struct IntentConstraints {
    /// Task class used for quality-evidence lookup (`coding`, `math`, …).
    pub task_class: String,
    /// Minimum context window in tokens.
    pub min_context_tokens: u64,
    /// Required input modalities beyond text (`vision`, `audio`, …).
    pub modalities: Vec<String>,
    /// Tool/function calling required.
    pub needs_tools: bool,
    /// Structured (JSON-schema) output required.
    pub needs_structured_output: bool,
    /// Required residency/region tag (e.g. `eu`).
    pub residency: Option<String>,
    /// Deadline: maximum acceptable estimated latency, ms.
    pub max_latency_ms: Option<u64>,
    /// Cost ceiling per million tokens; `Some(0.0)` means free-only.
    pub max_cost_per_mtok: Option<f64>,
    /// Model the user explicitly asked for.
    pub pinned_model: Option<String>,
    /// Whether falling back off a pin is permitted.
    pub pin_fallback: PinFallback,
    /// How many `Unknown`-availability candidates may be tried in one
    /// dispatch round (bounded discovery — unknown is not assumed usable).
    pub discovery_budget: usize,
}

impl Default for IntentConstraints {
    fn default() -> Self {
        Self {
            task_class: String::new(),
            min_context_tokens: 0,
            modalities: Vec::new(),
            needs_tools: false,
            needs_structured_output: false,
            residency: None,
            max_latency_ms: None,
            max_cost_per_mtok: None,
            pinned_model: None,
            pin_fallback: PinFallback::Allow,
            // Two unknown candidates may be probed per round: enough to
            // discover working targets without a retry storm.
            discovery_budget: 2,
        }
    }
}

/// Fallback policy when the pinned model cannot serve the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PinFallback {
    /// Rank the pin first; fall back to other eligible candidates.
    #[default]
    Allow,
    /// The pinned model or nothing.
    Deny,
}

/// One (credential, model) dispatch candidate.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub provider: String,
    /// Raw key material — held only to compute fingerprints for lookups;
    /// never copied into outcomes.
    pub api_key: String,
    pub account: Option<String>,
    pub region: Option<String>,
    pub model: String,
    /// Advertised context window, tokens.
    pub context_tokens: u64,
    /// Input modalities beyond text.
    pub modalities: Vec<String>,
    pub supports_tools: bool,
    pub supports_structured_output: bool,
    /// Residency/region tag the credential+model runs in.
    pub residency: Option<String>,
    /// Estimated latency for this task class, ms.
    pub est_latency_ms: u64,
    /// Cost per million tokens; `None` = free.
    pub cost_per_mtok: Option<f64>,
    /// Recorded outcomes per task class: `(successes, attempts)`.
    pub quality: BTreeMap<String, (u32, u32)>,
}

impl Candidate {
    fn subject(&self) -> Subject<'_> {
        Subject {
            provider: &self.provider,
            api_key: &self.api_key,
            account: self.account.as_deref(),
            region: self.region.as_deref(),
            model: &self.model,
        }
    }

    /// Opaque identifier for outcomes/logs — never the key.
    #[must_use]
    pub fn opaque_id(&self) -> String {
        format!(
            "{}/{}/{}",
            self.provider,
            self.model,
            &credential_fingerprint(&self.api_key)[..8]
        )
    }

    /// Recorded success rate for a task class; `None` when never tried.
    fn quality_score(&self, task_class: &str) -> Option<f64> {
        let (ok, n) = self.quality.get(task_class)?;
        (*n > 0).then(|| *ok as f64 / *n as f64)
    }
}

/// Why a candidate was filtered out — a capability or availability fact,
/// carrying no secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    /// Current eligibility state blocks this credential/model.
    Availability(EligibilityKind),
    /// Quota inventory reports the subject spent or throttled.
    QuotaExhausted,
    ContextTooSmall {
        has: u64,
        needs: u64,
    },
    MissingModality(String),
    NoTools,
    NoStructuredOutput,
    ResidencyMismatch {
        required: String,
        actual: String,
    },
    OverDeadline {
        est_ms: u64,
        max_ms: u64,
    },
    OverBudget,
    /// Pinned to another model and pin fallback is denied.
    PinMismatch,
}

/// A candidate rejected during filtering, with its reason.
#[derive(Debug, Clone)]
pub struct Rejection {
    /// Opaque `provider/model/fingerprint8` — safe to log.
    pub candidate: String,
    pub reason: BlockReason,
}

/// One dispatchable candidate in rank order.
#[derive(Debug, Clone)]
pub struct Ranked {
    /// Index into the input `candidates` slice.
    pub index: usize,
    /// Opaque identifier — safe to log.
    pub candidate: String,
    /// Whether this candidate's availability is only `Unknown` (a bounded
    /// discovery slot) rather than proven.
    pub discovery: bool,
    /// This candidate is the user's explicit pin and leads when fallback
    /// is permitted.
    pub pinned: bool,
    pub score: f64,
}

/// Selection result: the ranked dispatch order plus every rejection.
#[derive(Debug, Default)]
pub struct Selection {
    /// Ranked candidates. Entries with `discovery = true` count against
    /// `IntentConstraints::discovery_budget` and always follow proven ones.
    pub ranked: Vec<Ranked>,
    pub rejected: Vec<Rejection>,
}

impl Selection {
    /// The next dispatch target after a failure at position `failed_pos`
    /// (`None` before the first attempt). Respects the discovery budget.
    #[must_use]
    pub fn next_after(&self, attempted: &[usize]) -> Option<&Ranked> {
        self.ranked.iter().find(|r| !attempted.contains(&r.index))
    }
}

/// Whether an availability verdict permits dispatch at all.
fn dispatchable(kind: &EligibilityKind) -> bool {
    matches!(kind, EligibilityKind::Usable | EligibilityKind::Unknown)
}

/// Filter + rank `candidates` for `intent` against live `eligibility` and
/// `quota` evidence at `now`.
#[must_use]
pub fn select(
    intent: &IntentConstraints,
    candidates: &[Candidate],
    eligibility: &EligibilityStore,
    quotas: &cloud_quota::QuotaInventory,
    now: u64,
) -> Selection {
    let mut sel = Selection::default();
    for (index, c) in candidates.iter().enumerate() {
        if let Some(pin) = &intent.pinned_model {
            if &c.model != pin && intent.pin_fallback == PinFallback::Deny {
                sel.rejected.push(Rejection {
                    candidate: c.opaque_id(),
                    reason: BlockReason::PinMismatch,
                });
                continue;
            }
        }
        if let Some(reason) = capability_block(intent, c) {
            sel.rejected.push(Rejection {
                candidate: c.opaque_id(),
                reason,
            });
            continue;
        }
        // Availability filter — a preferred model never overrides a block.
        let verdict = eligibility.resolve(c.subject(), now);
        if !dispatchable(&verdict.kind) {
            sel.rejected.push(Rejection {
                candidate: c.opaque_id(),
                reason: BlockReason::Availability(verdict.kind),
            });
            continue;
        }
        if let cloud_quota::Headroom::Exhausted { .. } = quotas.resolve(c.subject(), now) {
            sel.rejected.push(Rejection {
                candidate: c.opaque_id(),
                reason: BlockReason::QuotaExhausted,
            });
            continue;
        }
        let discovery = verdict.kind == EligibilityKind::Unknown;
        let pinned = intent
            .pinned_model
            .as_deref()
            .is_some_and(|pin| c.model == pin);
        sel.ranked.push(Ranked {
            index,
            candidate: c.opaque_id(),
            discovery,
            pinned,
            score: score(intent, c),
        });
    }
    // A permitted pin leads; then proven-usable candidates; then bounded
    // discovery slots. Within each class rank by score (quality first,
    // then latency, then cost — see `score`).
    sel.ranked.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then(a.discovery.cmp(&b.discovery))
            .then(b.score.total_cmp(&a.score))
            .then_with(|| a.candidate.cmp(&b.candidate))
    });
    // Bound the discovery tail: at most `discovery_budget` unknown
    // candidates may be dispatched in one round.
    let mut seen_discovery = 0usize;
    sel.ranked.retain(|r| {
        if r.discovery {
            seen_discovery += 1;
            seen_discovery <= intent.discovery_budget
        } else {
            true
        }
    });
    sel
}

/// Hard capability filters, evaluated before availability.
fn capability_block(intent: &IntentConstraints, c: &Candidate) -> Option<BlockReason> {
    if c.context_tokens < intent.min_context_tokens {
        return Some(BlockReason::ContextTooSmall {
            has: c.context_tokens,
            needs: intent.min_context_tokens,
        });
    }
    for m in &intent.modalities {
        if !c.modalities.iter().any(|have| have == m) {
            return Some(BlockReason::MissingModality(m.clone()));
        }
    }
    if intent.needs_tools && !c.supports_tools {
        return Some(BlockReason::NoTools);
    }
    if intent.needs_structured_output && !c.supports_structured_output {
        return Some(BlockReason::NoStructuredOutput);
    }
    if let Some(req) = &intent.residency {
        let actual = c.residency.clone().unwrap_or_default();
        if &actual != req {
            return Some(BlockReason::ResidencyMismatch {
                required: req.clone(),
                actual,
            });
        }
    }
    if let Some(max_ms) = intent.max_latency_ms {
        if c.est_latency_ms > max_ms {
            return Some(BlockReason::OverDeadline {
                est_ms: c.est_latency_ms,
                max_ms,
            });
        }
    }
    if let Some(cap) = intent.max_cost_per_mtok {
        if c.cost_per_mtok.unwrap_or(0.0) > cap {
            return Some(BlockReason::OverBudget);
        }
    }
    None
}

/// Rank score for a surviving candidate: recorded per-task-class quality
/// dominates; latency and allowed cost are tiebreakers. Free status alone
/// never beats better evidence — cost is only a preference *within* equal
/// quality, and only insofar as the budget filter already permits it.
fn score(intent: &IntentConstraints, c: &Candidate) -> f64 {
    let quality = c.quality_score(&intent.task_class).unwrap_or(0.5);
    // Latency normalized against the deadline (or a soft 60s horizon).
    let horizon = intent.max_latency_ms.unwrap_or(60_000).max(1) as f64;
    let latency = 1.0 - (c.est_latency_ms as f64 / horizon).clamp(0.0, 1.0);
    // Cost: free is a mild preference, not a trump card.
    let cost = match c.cost_per_mtok {
        None => 1.0,
        Some(v) => 1.0 / (1.0 + v),
    };
    quality * 100.0 + latency * 10.0 + cost
}

/// Record the result of a dispatch attempt so availability is updated
/// immediately — a failed attempt blocks the target before the next
/// fallback, and a success lifts an `Unknown` candidate to `Usable`.
pub fn record_dispatch_outcome(
    c: &Candidate,
    store: &mut EligibilityStore,
    result: &InferenceResult,
    now: u64,
) {
    store.record_inference(c.subject(), result, now);
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_vendor_models::cloud_eligibility::{EligibilityStore, Subject};
    use susi_vendor_models::cloud_quota::{
        QuotaAmount, QuotaInventory, QuotaKind, QuotaObservation, QuotaSource,
    };

    const T0: u64 = 1_700_000_000;

    fn cand(key: &str, model: &str) -> Candidate {
        Candidate {
            provider: "acme".into(),
            api_key: key.into(),
            account: None,
            region: None,
            model: model.into(),
            context_tokens: 128_000,
            modalities: vec![],
            supports_tools: true,
            supports_structured_output: true,
            residency: None,
            est_latency_ms: 800,
            cost_per_mtok: Some(1.0),
            quality: BTreeMap::new(),
        }
    }

    fn intent() -> IntentConstraints {
        IntentConstraints {
            task_class: "coding".into(),
            ..Default::default()
        }
    }

    fn fail(status: u16, body: &str) -> InferenceResult {
        InferenceResult::Failed {
            status: Some(status),
            body_snippet: body.into(),
            retry_after_secs: None,
        }
    }

    fn subj_of(c: &Candidate) -> Subject<'_> {
        Subject {
            provider: &c.provider,
            api_key: &c.api_key,
            account: c.account.as_deref(),
            region: c.region.as_deref(),
            model: &c.model,
        }
    }

    /// Ten credentials exercising every availability class at once.
    fn ten_key_fixture() -> (Vec<Candidate>, EligibilityStore, QuotaInventory) {
        let mut store = EligibilityStore::new();
        let mut quota = QuotaInventory::new();
        let mut cs = Vec::new();
        // k0 proven usable with strong quality on "coding".
        let c0 = {
            let mut c = cand("sk-0", "m-good");
            c.quality.insert("coding".into(), (9, 10));
            c.est_latency_ms = 400;
            c
        };
        store.record_inference(subj_of(&c0), &InferenceResult::Success, T0);
        cs.push(c0);
        // k1 proven usable, weaker quality.
        let c1 = {
            let mut c = cand("sk-1", "m-ok");
            c.quality.insert("coding".into(), (5, 10));
            c
        };
        store.record_inference(subj_of(&c1), &InferenceResult::Success, T0);
        cs.push(c1);
        // k2 invalid credential.
        let c2 = cand("sk-2", "m-dead");
        store.record_inference(subj_of(&c2), &fail(401, "invalid api key"), T0);
        cs.push(c2);
        // k3 insufficient credit.
        let c3 = cand("sk-3", "m-broke");
        store.record_inference(subj_of(&c3), &fail(402, "insufficient credit balance"), T0);
        cs.push(c3);
        // k4 quota exhausted (via inventory).
        let c4 = cand("sk-4", "m-spent");
        quota.record(
            QuotaObservation {
                provider: "acme".into(),
                credential: credential_fingerprint("sk-4"),
                account: String::new(),
                model: String::new(),
                kind: QuotaKind::Requests,
                amount: QuotaAmount::Limited(0),
                resets_at_unix: Some(T0 + 3600),
                retry_after_secs: None,
                observed_unix: T0,
                stale_after_secs: 3600,
                source: QuotaSource::Headers,
            },
            T0,
        );
        cs.push(c4);
        // k5 rate limited with a retry-after still in force.
        let c5 = cand("sk-5", "m-throttled");
        store.record_inference(
            subj_of(&c5),
            &InferenceResult::Failed {
                status: Some(429),
                body_snippet: "rate limited".into(),
                retry_after_secs: Some(600),
            },
            T0,
        );
        cs.push(c5);
        // k6 service unavailable — provider-scoped outage, so it lives on
        // its own provider or it would correctly block the whole fixture.
        let mut c6 = cand("sk-6", "m-down");
        c6.provider = "acme-down".into();
        store.record_inference(subj_of(&c6), &fail(503, "service unavailable"), T0);
        cs.push(c6);
        // k7 model access denied.
        let c7 = cand("sk-7", "m-gated");
        store.record_inference(
            subj_of(&c7),
            &fail(403, "you do not have access to this model"),
            T0,
        );
        cs.push(c7);
        // k8 unknown (never tried).
        cs.push(cand("sk-8", "m-unknown"));
        // k9 unknown, second discovery slot candidate.
        cs.push(cand("sk-9", "m-unknown2"));
        (cs, store, quota)
    }

    #[test]
    fn cloud_intent_selection_filters_nonworking_before_ranking() {
        let (cs, elig, quota) = ten_key_fixture();
        let sel = select(&intent(), &cs, &elig, &quota, T0 + 1);
        // Winner: the proven usable key with the best coding evidence.
        assert_eq!(sel.ranked[0].index, 0);
        assert_eq!(sel.ranked[1].index, 1);
        // Every hard-blocked state is rejected, never ranked.
        for blocked in [2usize, 3, 4, 5, 6, 7] {
            assert!(
                sel.ranked.iter().all(|r| r.index != blocked),
                "blocked candidate {blocked} must not rank"
            );
        }
        // Unknown keys occupy the bounded discovery tail.
        let discovery: Vec<usize> = sel
            .ranked
            .iter()
            .filter(|r| r.discovery)
            .map(|r| r.index)
            .collect();
        assert_eq!(discovery.len(), 2, "discovery budget bounds unknowns");
        assert!(discovery.iter().all(|i| *i >= 8));
    }

    #[test]
    fn cloud_intent_selection_capability_constraints_are_hard() {
        let (mut cs, elig, quota) = ten_key_fixture();
        // The quality leader lacks vision; a weaker model has it.
        cs[0].modalities.clear();
        cs[1].modalities = vec!["vision".into()];
        let mut i = intent();
        i.modalities = vec!["vision".into()];
        let sel = select(&i, &cs, &elig, &quota, T0 + 1);
        assert_eq!(
            sel.ranked[0].index, 1,
            "required modality beats quality lead"
        );
        assert!(sel
            .rejected
            .iter()
            .any(|r| matches!(r.reason, BlockReason::MissingModality(_))));

        // Context floor.
        let mut i = intent();
        i.min_context_tokens = 200_000;
        let sel = select(&i, &cs, &elig, &quota, T0 + 1);
        assert!(sel.ranked.is_empty() || sel.ranked.iter().all(|r| r.discovery));
        assert!(sel
            .rejected
            .iter()
            .any(|r| matches!(r.reason, BlockReason::ContextTooSmall { .. })));

        // Free-only budget rejects paid survivors.
        let mut i = intent();
        i.max_cost_per_mtok = Some(0.0);
        let sel = select(&i, &cs, &elig, &quota, T0 + 1);
        assert!(sel
            .rejected
            .iter()
            .any(|r| r.reason == BlockReason::OverBudget));

        // Residency requirement.
        cs[0].residency = Some("us".into());
        cs[1].residency = Some("eu".into());
        let mut i = intent();
        i.residency = Some("eu".into());
        let sel = select(&i, &cs, &elig, &quota, T0 + 1);
        assert!(sel
            .rejected
            .iter()
            .any(|r| matches!(r.reason, BlockReason::ResidencyMismatch { .. })));
    }

    #[test]
    fn cloud_intent_selection_free_status_never_trumps_capability() {
        // A free model missing required structured output must not beat a
        // paid capable one even under a free-only preference that fails.
        let mut free_bad = cand("sk-free", "m-free");
        free_bad.cost_per_mtok = None;
        free_bad.supports_structured_output = false;
        let mut store = EligibilityStore::new();
        store.record_inference(subj_of(&free_bad), &InferenceResult::Success, T0);
        let mut paid_good = cand("sk-paid", "m-paid");
        paid_good.cost_per_mtok = Some(5.0);
        paid_good.quality.insert("coding".into(), (9, 10));
        store.record_inference(subj_of(&paid_good), &InferenceResult::Success, T0);
        let quota = QuotaInventory::new();
        let mut i = intent();
        i.needs_structured_output = true;
        let sel = select(&i, &[free_bad, paid_good], &store, &quota, T0 + 1);
        assert_eq!(sel.ranked.len(), 1);
        assert_eq!(sel.ranked[0].candidate.split('/').nth(1), Some("m-paid"));
    }

    #[test]
    fn cloud_intent_selection_pin_policy() {
        let (cs, elig, quota) = ten_key_fixture();
        // Pin to the weaker but working model, fallback allowed: it leads.
        let mut i = intent();
        i.pinned_model = Some("m-ok".into());
        let sel = select(&i, &cs, &elig, &quota, T0 + 1);
        assert_eq!(sel.ranked[0].index, 1);
        assert!(sel.ranked[0].pinned);
        assert!(sel.ranked.len() > 1, "fallback remains available");

        // Deny fallback: only the pin survives.
        i.pin_fallback = PinFallback::Deny;
        let sel = select(&i, &cs, &elig, &quota, T0 + 1);
        assert_eq!(sel.ranked.len(), 1);
        assert!(sel
            .rejected
            .iter()
            .any(|r| r.reason == BlockReason::PinMismatch));

        // Pin to a dead model: denial yields nothing, never a silent fallback.
        let mut i = intent();
        i.pinned_model = Some("m-dead".into());
        i.pin_fallback = PinFallback::Deny;
        let sel = select(&i, &cs, &elig, &quota, T0 + 1);
        assert!(sel.ranked.is_empty());
    }

    #[test]
    fn cloud_intent_selection_recovery_via_fresh_evidence() {
        let (cs, mut elig, quota) = ten_key_fixture();
        let down = &cs[6];
        // The unavailable model succeeds now — fresh evidence restores it.
        record_dispatch_outcome(down, &mut elig, &InferenceResult::Success, T0 + 10);
        let sel = select(&intent(), &cs, &elig, &quota, T0 + 11);
        assert!(sel.ranked.iter().any(|r| r.index == 6 && !r.discovery));
    }

    #[test]
    fn cloud_intent_selection_failed_dispatch_never_leads_again() {
        let (cs, mut elig, quota) = ten_key_fixture();
        // The quality leader fails mid-dispatch with a hard block; the next
        // selection must exclude it immediately.
        record_dispatch_outcome(&cs[0], &mut elig, &fail(401, "key revoked"), T0 + 5);
        let sel = select(&intent(), &cs, &elig, &quota, T0 + 6);
        assert!(sel.ranked.iter().all(|r| r.index != 0));
        assert_eq!(sel.ranked[0].index, 1);
        // Bounded fallback walks the rank order without repeats.
        let next = sel.next_after(&[1]).map(|r| r.index);
        assert!(next.is_some_and(|i| i != 1));
    }

    #[test]
    fn cloud_intent_selection_never_leaks_key_material() {
        let (cs, elig, quota) = ten_key_fixture();
        let mut i = intent();
        i.pinned_model = Some("m-dead".into());
        i.pin_fallback = PinFallback::Deny;
        let sel = select(&i, &cs, &elig, &quota, T0 + 1);
        let ser =
            serde_json::to_string(&sel.rejected.iter().map(|r| &r.reason).collect::<Vec<_>>())
                .unwrap();
        let ids: Vec<String> = sel
            .ranked
            .iter()
            .map(|r| r.candidate.clone())
            .chain(sel.rejected.iter().map(|r| r.candidate.clone()))
            .collect();
        let blob = format!("{ids:?}{ser}");
        for n in 0..10 {
            assert!(!blob.contains(&format!("sk-{n}")), "key material leaked");
        }
    }

    #[test]
    fn cloud_intent_selection_quality_latency_cost_ranking() {
        // Among working, capable candidates: quality dominates, then
        // latency, then cost as a mild tiebreak.
        let mut a = cand("sk-a", "m-slow-best");
        a.quality.insert("coding".into(), (10, 10));
        a.est_latency_ms = 5000;
        a.cost_per_mtok = Some(10.0);
        let mut b = cand("sk-b", "m-fast-ok");
        b.quality.insert("coding".into(), (6, 10));
        b.est_latency_ms = 100;
        b.cost_per_mtok = None;
        let mut c = cand("sk-c", "m-free-mid");
        c.quality.insert("coding".into(), (6, 10));
        c.est_latency_ms = 5000;
        c.cost_per_mtok = None;
        let mut store = EligibilityStore::new();
        for x in [&a, &b, &c] {
            store.record_inference(subj_of(x), &InferenceResult::Success, T0);
        }
        let quota = QuotaInventory::new();
        let sel = select(&intent(), &[a, b, c], &store, &quota, T0 + 1);
        let order: Vec<&str> = sel
            .ranked
            .iter()
            .map(|r| r.candidate.split('/').nth(1).unwrap())
            .collect();
        assert_eq!(order[0], "m-slow-best", "quality dominates latency");
        // Equal quality: latency dominates — fast+free beats slow+free.
        assert_eq!(order[1], "m-fast-ok");
        assert_eq!(order[2], "m-free-mid");
    }
}
