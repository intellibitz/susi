//! Explain cloud selection: why a model was picked, why others were
//! excluded, and what a user can actually do when nothing is eligible.
//!
//! Everything here is safe to print: candidates are identified by the opaque
//! `provider/model/credfp8` id — key material never appears. Advice is
//! honest: it says what *may* unblock dispatch, never that completion is
//! guaranteed.

use serde::{Deserialize, Serialize};

use susi_vendor_models::cloud_eligibility::{EligibilityKind, EligibilityStore};
use susi_vendor_models::cloud_quota::QuotaInventory;

use crate::cloud_intent::{select, BlockReason, Candidate, IntentConstraints};

/// A dispatchable candidate with the evidence that ranked it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainedPick {
    /// Opaque candidate id.
    pub candidate: String,
    /// Rank position (0 = would dispatch first).
    pub rank: usize,
    /// Why it ranks here — evidence-based.
    pub reason: String,
    /// Availability is unproven (bounded discovery slot).
    pub discovery: bool,
    /// The user's explicit pin.
    pub pinned: bool,
}

/// A blocked candidate with its cause.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exclusion {
    /// Opaque candidate id.
    pub candidate: String,
    /// Human-readable exclusion cause.
    pub cause: String,
    /// The eligibility kind when the block came from availability.
    pub kind: Option<EligibilityKind>,
}

/// What the user could do to unblock dispatch — *may help*, never "will".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Advice {
    /// Cooldown/reset window clears at this time (unix secs, if known).
    WaitForCooldown { until: Option<u64> },
    /// The account is out of credit/quota — adding credit may unblock it.
    /// (A valid out-of-credit key is never asked to be *replaced*.)
    ProvideCredit { account: Option<String> },
    /// The credential itself is invalid — replace it.
    ReplaceCredential { provider: String },
    /// Free-only policy is blocking paid candidates — consenting to paid
    /// fallback may unblock.
    ConsentPaidFallback,
    /// An eligible local model may serve the intent.
    UseLocalModel,
    /// Unproven candidates exist — bounded discovery attempts may unblock.
    DiscoveryPending,
    /// The pinned model is unavailable and fallback is denied.
    PinBlocksAll,
}

/// The full explanation of one selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainReport {
    /// Selected dispatch order with reasons.
    pub ranked: Vec<ExplainedPick>,
    /// Every excluded candidate and why.
    pub excluded: Vec<Exclusion>,
    /// Actionable advice — populated when nothing is dispatchable.
    pub actions: Vec<Advice>,
    /// Whether ANY candidate can dispatch right now.
    pub dispatchable: bool,
}

/// Explain `select()` for an intent — the ranking rationale plus
/// per-candidate exclusion causes and advice for the all-blocked case.
#[must_use]
pub fn explain(
    intent: &IntentConstraints,
    candidates: &[Candidate],
    eligibility: &EligibilityStore,
    quota: &QuotaInventory,
    now: u64,
) -> ExplainReport {
    let sel = select(intent, candidates, eligibility, quota, now);
    let mut ranked = Vec::new();
    for (rank, r) in sel.ranked.iter().enumerate() {
        let c = &candidates[r.index];
        ranked.push(ExplainedPick {
            candidate: r.candidate.clone(),
            rank,
            reason: pick_reason(c, r.discovery, r.pinned),
            discovery: r.discovery,
            pinned: r.pinned,
        });
    }
    let mut excluded = Vec::new();
    let mut actions: Vec<Advice> = Vec::new();
    for rej in &sel.rejected {
        let (cause, kind) = describe_block(&rej.reason);
        excluded.push(Exclusion {
            candidate: rej.candidate.clone(),
            cause,
            kind,
        });
        if let Some(a) = advice_for(&rej.reason, candidates) {
            if !actions.contains(&a) {
                actions.push(a);
            }
        }
    }
    if sel.ranked.is_empty() {
        // Candidates that were neither ranked nor rejected were dropped by
        // the discovery budget — probing them may unblock dispatch.
        let rejected_ids: std::collections::BTreeSet<&str> =
            sel.rejected.iter().map(|r| r.candidate.as_str()).collect();
        if candidates
            .iter()
            .any(|c| !rejected_ids.contains(c.opaque_id().as_str()))
        {
            push(&mut actions, Advice::DiscoveryPending);
        }
        if actions.is_empty() {
            push(&mut actions, Advice::UseLocalModel);
        }
    }
    // A free-only intent whose only remaining candidates are paid → advise
    // consenting to paid fallback.
    if sel.ranked.is_empty() {
        let any_paid = candidates
            .iter()
            .any(|c| c.cost_per_mtok.unwrap_or(0.0) > 0.0);
        if any_paid && intent.max_cost_per_mtok == Some(0.0) {
            push(&mut actions, Advice::ConsentPaidFallback);
        }
    }
    ExplainReport {
        ranked,
        excluded,
        actions,
        dispatchable: !sel.ranked.is_empty(),
    }
}

fn pick_reason(c: &Candidate, discovery: bool, pinned: bool) -> String {
    let mut parts = Vec::new();
    if pinned {
        parts.push("explicitly pinned".to_string());
    }
    if discovery {
        parts.push("unproven — bounded discovery slot".to_string());
    } else {
        parts.push("fresh inference evidence: usable".to_string());
    }
    if let Some(q) = c.quality.values().next() {
        parts.push(format!("{} verified wins/{}", q.0, q.1));
    }
    parts.push(format!("~{}ms", c.est_latency_ms));
    match c.cost_per_mtok {
        Some(0.0) => parts.push("free".to_string()),
        Some(m) => parts.push(format!("{m} micros/Mtok")),
        None => parts.push("unknown cost".to_string()),
    }
    parts.join("; ")
}

fn describe_block(reason: &BlockReason) -> (String, Option<EligibilityKind>) {
    match reason {
        BlockReason::Availability(k) => {
            let text = match k {
                EligibilityKind::InvalidCredential => "credential rejected by provider",
                EligibilityKind::AccessDenied => "credential lacks model access",
                EligibilityKind::RateLimited => "rate limited",
                EligibilityKind::QuotaExhausted => "quota window exhausted",
                EligibilityKind::InsufficientCredit => "account out of credit",
                EligibilityKind::ServiceUnavailable => "service unavailable",
                EligibilityKind::UnsupportedRequest => "model rejects this request shape",
                EligibilityKind::Usable => "usable",
                EligibilityKind::Unknown => "unproven",
            };
            (text.to_string(), Some(*k))
        }
        BlockReason::QuotaExhausted => ("quota window exhausted".into(), None),
        BlockReason::ContextTooSmall { has, needs } => {
            (format!("context {has} < required {needs}"), None)
        }
        BlockReason::MissingModality(m) => (format!("missing modality {m}"), None),
        BlockReason::NoTools => ("no tool support".into(), None),
        BlockReason::NoStructuredOutput => ("no structured output".into(), None),
        BlockReason::ResidencyMismatch { required, actual } => {
            (format!("residency {actual} ≠ required {required}"), None)
        }
        BlockReason::OverDeadline { est_ms, max_ms } => {
            (format!("est {est_ms}ms > deadline {max_ms}ms"), None)
        }
        BlockReason::OverBudget => ("over budget policy".into(), None),
        BlockReason::PinMismatch => ("not the pinned model".into(), None),
    }
}

fn advice_for(reason: &BlockReason, candidates: &[Candidate]) -> Option<Advice> {
    match reason {
        BlockReason::Availability(EligibilityKind::InvalidCredential)
        | BlockReason::Availability(EligibilityKind::AccessDenied) => {
            let provider = candidates
                .iter()
                .map(|c| c.provider.clone())
                .next()
                .unwrap_or_default();
            Some(Advice::ReplaceCredential { provider })
        }
        BlockReason::Availability(EligibilityKind::InsufficientCredit)
        | BlockReason::Availability(EligibilityKind::QuotaExhausted)
        | BlockReason::QuotaExhausted => {
            let account = candidates.iter().find_map(|c| c.account.clone());
            Some(Advice::ProvideCredit { account })
        }
        BlockReason::Availability(EligibilityKind::RateLimited)
        | BlockReason::Availability(EligibilityKind::ServiceUnavailable) => {
            Some(Advice::WaitForCooldown { until: None })
        }
        BlockReason::OverBudget => Some(Advice::ConsentPaidFallback),
        BlockReason::PinMismatch => Some(Advice::PinBlocksAll),
        BlockReason::Availability(EligibilityKind::Usable)
        | BlockReason::Availability(EligibilityKind::Unknown)
        | BlockReason::Availability(EligibilityKind::UnsupportedRequest)
        | BlockReason::ContextTooSmall { .. }
        | BlockReason::MissingModality(_)
        | BlockReason::NoTools
        | BlockReason::NoStructuredOutput
        | BlockReason::ResidencyMismatch { .. }
        | BlockReason::OverDeadline { .. } => None,
    }
}

fn push(actions: &mut Vec<Advice>, a: Advice) {
    if !actions.contains(&a) {
        actions.push(a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use susi_vendor_models::cloud_eligibility::{InferenceResult, Subject};

    const T0: u64 = 1_700_000_000;

    fn cand(key: &str, model: &str) -> Candidate {
        Candidate {
            provider: "acme".into(),
            api_key: key.into(),
            account: Some("acct-1".into()),
            region: None,
            model: model.into(),
            context_tokens: 128_000,
            modalities: vec![],
            supports_tools: true,
            supports_structured_output: true,
            residency: None,
            est_latency_ms: 500,
            cost_per_mtok: Some(1.0),
            quality: BTreeMap::new(),
        }
    }

    fn intent() -> IntentConstraints {
        IntentConstraints {
            task_class: "coding".into(),
            discovery_budget: 0,
            ..Default::default()
        }
    }

    fn subj<'a>(c: &'a Candidate) -> Subject<'a> {
        Subject {
            provider: &c.provider,
            api_key: &c.api_key,
            account: c.account.as_deref(),
            region: c.region.as_deref(),
            model: &c.model,
        }
    }

    fn fail(c: &Candidate, status: u16, body: &str, s: &mut EligibilityStore) {
        s.record_inference(
            subj(c),
            &InferenceResult::Failed {
                status: Some(status),
                body_snippet: body.into(),
                retry_after_secs: None,
            },
            T0,
        );
    }

    #[test]
    fn cloud_selection_explain_ranks_with_evidence_reasons() {
        let mut cs = vec![cand("sk-1", "m1"), cand("sk-2", "m2")];
        cs[0].quality.insert("coding".into(), (9, 10));
        let mut e = EligibilityStore::new();
        e.record_inference(subj(&cs[0]), &InferenceResult::Success, T0);
        e.record_inference(subj(&cs[1]), &InferenceResult::Success, T0);
        let q = QuotaInventory::new();
        let rep = explain(&intent(), &cs, &e, &q, T0);
        assert!(rep.dispatchable);
        assert_eq!(rep.ranked.len(), 2);
        assert!(rep.ranked[0].reason.contains("usable"));
        assert!(rep.ranked[0].reason.contains("verified"));
    }

    #[test]
    fn cloud_selection_explain_all_blocked_gives_actionable_advice() {
        let cs = vec![cand("sk-1", "m1"), cand("sk-2", "m2")];
        let mut e = EligibilityStore::new();
        fail(&cs[0], 402, "insufficient credit", &mut e);
        fail(&cs[1], 402, "insufficient credit", &mut e);
        let q = QuotaInventory::new();
        let rep = explain(&intent(), &cs, &e, &q, T0);
        assert!(!rep.dispatchable);
        assert!(rep.actions.contains(&Advice::ProvideCredit {
            account: Some("acct-1".into())
        }));
        // A valid out-of-credit key must NOT be told to replace itself.
        assert!(!rep
            .actions
            .iter()
            .any(|a| matches!(a, Advice::ReplaceCredential { .. })));
        // No key material anywhere.
        let ser = serde_json::to_string(&rep).unwrap();
        assert!(!ser.contains("sk-1"));
        assert!(!ser.contains("sk-2"));
    }

    #[test]
    fn cloud_selection_explain_invalid_key_advises_replacement() {
        let cs = vec![cand("sk-bad", "m1")];
        let mut e = EligibilityStore::new();
        fail(&cs[0], 401, "invalid api key", &mut e);
        let q = QuotaInventory::new();
        let rep = explain(&intent(), &cs, &e, &q, T0);
        assert!(!rep.dispatchable);
        assert!(rep
            .actions
            .iter()
            .any(|a| matches!(a, Advice::ReplaceCredential { .. })));
    }

    #[test]
    fn cloud_selection_explain_cooldown_advice_for_throttled() {
        let cs = vec![cand("sk-1", "m1")];
        let mut e = EligibilityStore::new();
        fail(&cs[0], 429, "rate limited", &mut e);
        let q = QuotaInventory::new();
        let rep = explain(&intent(), &cs, &e, &q, T0);
        assert!(!rep.dispatchable);
        assert!(rep
            .actions
            .iter()
            .any(|a| matches!(a, Advice::WaitForCooldown { .. })));
    }

    #[test]
    fn cloud_selection_explain_free_only_suggests_paid_consent() {
        // Free-only intent, only paid candidates exist → advise consent.
        let cs = vec![cand("sk-1", "m1")];
        let e = EligibilityStore::new();
        let q = QuotaInventory::new();
        let mut i = intent();
        i.max_cost_per_mtok = Some(0.0);
        let rep = explain(&i, &cs, &e, &q, T0);
        assert!(
            rep.actions.contains(&Advice::ConsentPaidFallback)
                || rep.actions.contains(&Advice::DiscoveryPending)
        );
    }

    #[test]
    fn cloud_selection_explain_report_contains_no_secrets() {
        let cs = vec![cand("super-secret-key-12345", "m1"), cand("sk-2", "m2")];
        let mut e = EligibilityStore::new();
        fail(&cs[0], 401, "bad key", &mut e);
        fail(&cs[1], 503, "down", &mut e);
        let q = QuotaInventory::new();
        let rep = explain(&intent(), &cs, &e, &q, T0);
        let ser = serde_json::to_string(&rep).unwrap();
        assert!(!ser.contains("super-secret-key-12345"));
        assert!(!ser.contains("sk-2"));
    }
}
