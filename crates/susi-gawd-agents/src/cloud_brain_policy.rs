//! Default powerful-brain policy (T-CODEX-23 / VC-201-005).
//!
//! Whenever an authorized cloud model is **currently working** and satisfies
//! the request, that model is Susi's primary reasoning brain — this is the
//! default path for supported intelligent workflows, not an opt-in helper.
//!
//! Ranking rules:
//! - Capability order comes from **verified task-class outcomes**
//!   (`cloud_rsi_outcomes::OutcomeLedger`) — never model name, price, or
//!   vendor marketing. `Candidate::quality` marketing/static hints do not
//!   override verified evidence; a model with no verified outcomes ranks as
//!   unproven, between verified-good and verified-bad.
//! - "Currently working" is fresh evidence: a `Usable` eligibility verdict
//!   plus quota headroom. Unknown availability never leads the brain (it
//!   may only occupy bounded discovery slots downstream).
//! - Quality beats cheaper/local candidates within authorized budget,
//!   deadline, privacy and capability constraints — `cloud_intent::select`
//!   enforces those hard filters before ranking.
//! - An explicit user override is binding: the pinned model leads while it
//!   is working; if it is not, the blocker is explained instead of silently
//!   falling back.
//! - When nothing qualifies the decision is `LocalFallback` (only if
//!   permitted) or `Blocked` with redacted reasons — never a silent pick.

use std::collections::BTreeSet;

use crate::cloud_intent::{select, Candidate, IntentConstraints, PinFallback, Ranked, Rejection};
use susi_vendor_models::cloud_eligibility::EligibilityStore;
use susi_vendor_models::cloud_quota::QuotaInventory;

/// Verified task-class outcomes feeding capability ranking. The production
/// implementation is `susi_gawd::cloud_rsi_outcomes::OutcomeLedger`; the
/// seam exists so this policy crate stays dependency-correct.
pub trait OutcomeRanking {
    /// Order `models` best-first for `task_class` by verified outcomes;
    /// `is_working` excludes currently-nonworking models from leading.
    fn rank(
        &self,
        task_class: &str,
        models: &[String],
        is_working: &dyn Fn(&str) -> bool,
    ) -> Vec<String>;
    /// Verified quality score of one model on a task class, if any.
    fn quality(&self, task_class: &str, model: &str) -> Option<f64>;
}

/// The shared evidence stores the policy consults.
pub struct BrainStores<'a> {
    /// Credential/model availability evidence.
    pub eligibility: &'a EligibilityStore,
    /// Quota/credit headroom.
    pub quotas: &'a QuotaInventory,
    /// Verified self-dev outcomes per (task-class, model).
    pub outcomes: &'a dyn OutcomeRanking,
}

/// How the brain choice was resolved.
#[derive(Debug)]
pub enum BrainDecision {
    /// A proven-working cloud model leads.
    Cloud {
        /// Opaque credential+model id of the lead candidate.
        lead_opaque: String,
        /// Model name of the lead.
        model: String,
        /// Verified quality score, if the model has outcome evidence.
        verified_quality: Option<f64>,
        /// Opaque ids of the remaining proven-usable candidates,
        /// outcome-ordered.
        alternatives: Vec<String>,
    },
    /// No working cloud brain qualified; local fallback is permitted.
    LocalFallback {
        /// Why no cloud brain qualified (redacted).
        reason: String,
    },
    /// Nothing qualifies and no fallback is permitted.
    Blocked {
        /// Redacted rejection reasons from the selection pass.
        reasons: Vec<Rejection>,
    },
}

/// User/default policy for brain selection.
#[derive(Debug, Default)]
pub struct BrainPolicy {
    /// Permit falling back to local inference when no cloud brain qualifies.
    pub allow_local_fallback: bool,
    /// Explicit override — binding while the named model is working.
    pub override_model: Option<String>,
}

/// Choose the primary reasoning brain for `request`. This is the default
/// path — callers do not opt in.
#[must_use]
pub fn select_brain(
    policy: &BrainPolicy,
    request: &IntentConstraints,
    candidates: &[Candidate],
    stores: &BrainStores,
    now: u64,
) -> BrainDecision {
    let mut intent = request.clone();
    if let Some(pin) = &policy.override_model {
        // Explicit override is binding — no silent fallback off the pin.
        intent.pinned_model = Some(pin.clone());
        intent.pin_fallback = PinFallback::Deny;
    }
    let sel = select(&intent, candidates, stores.eligibility, stores.quotas, now);
    let ranked = &sel.ranked;
    // Only proven-usable candidates may lead the brain.
    let usable: Vec<&Ranked> = ranked.iter().filter(|r| !r.discovery).collect();
    if usable.is_empty() {
        return fallback_or_blocked(policy, sel.rejected);
    }
    // Distinct model names among the usable set, for outcome ordering.
    let models: BTreeSet<String> = usable
        .iter()
        .filter_map(|r| candidates.get(r.index).map(|c| c.model.clone()))
        .collect();
    let ordered_models = stores.outcomes.rank(
        &intent.task_class,
        &models.iter().cloned().collect::<Vec<_>>(),
        &|m| models.contains(m),
    );
    // Lead = best outcome-ranked model; pick that model's best candidate.
    let mut alternatives = Vec::new();
    let mut lead: Option<(&Ranked, String)> = None;
    for m in &ordered_models {
        let best = usable
            .iter()
            .filter(|r| candidates.get(r.index).is_some_and(|c| &c.model == m))
            .max_by(|a, b| a.score.total_cmp(&b.score));
        if let Some(b) = best {
            if lead.is_none() {
                lead = Some((*b, m.clone()));
            } else {
                alternatives.push(b.candidate.clone());
            }
        }
    }
    match lead {
        Some((r, m)) => BrainDecision::Cloud {
            lead_opaque: r.candidate.clone(),
            verified_quality: stores.outcomes.quality(&intent.task_class, &m),
            model: m,
            alternatives,
        },
        None => fallback_or_blocked(policy, sel.rejected),
    }
}

fn fallback_or_blocked(policy: &BrainPolicy, rejected: Vec<Rejection>) -> BrainDecision {
    if policy.allow_local_fallback {
        let reason = if rejected.is_empty() {
            "no cloud candidates supplied".to_string()
        } else {
            format!("{} candidate(s) blocked", rejected.len())
        };
        BrainDecision::LocalFallback { reason }
    } else {
        BrainDecision::Blocked { reasons: rejected }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_intent::Candidate;
    use std::collections::BTreeMap;
    use susi_vendor_models::cloud_eligibility::{credential_fingerprint, InferenceResult, Subject};
    use susi_vendor_models::cloud_quota::{
        QuotaAmount, QuotaInventory, QuotaKind, QuotaObservation,
    };

    /// Map-backed fake verified-outcome store: accepted/(accepted+rejected)
    /// with Laplace smoothing, mirroring the production ledger's ordering.
    #[derive(Default)]
    struct FakeOutcomes {
        // (task_class, model) -> (accepted, rejected)
        q: BTreeMap<(String, String), (u32, u32)>,
    }

    impl FakeOutcomes {
        fn promote(&mut self, class: &str, model: &str, n: u32) {
            self.q.entry((class.into(), model.into())).or_default().0 += n;
        }
        fn reject(&mut self, class: &str, model: &str, n: u32) {
            self.q.entry((class.into(), model.into())).or_default().1 += n;
        }
    }

    impl OutcomeRanking for FakeOutcomes {
        fn rank(
            &self,
            task_class: &str,
            models: &[String],
            is_working: &dyn Fn(&str) -> bool,
        ) -> Vec<String> {
            let mut v: Vec<String> = models.to_vec();
            let q = |m: &String| {
                self.q
                    .get(&(task_class.to_string(), m.clone()))
                    .map(|(a, r)| f64::from(*a + 1) / f64::from(*a + *r + 2))
                    .unwrap_or(0.5)
            };
            v.sort_by(|a, b| {
                is_working(b)
                    .cmp(&is_working(a))
                    .then(q(b).partial_cmp(&q(a)).unwrap_or(std::cmp::Ordering::Equal))
            });
            v
        }
        fn quality(&self, task_class: &str, model: &str) -> Option<f64> {
            self.q
                .get(&(task_class.to_string(), model.to_string()))
                .map(|(a, r)| f64::from(*a + 1) / f64::from(*a + *r + 2))
        }
    }

    const T0: u64 = 1_700_000_000;

    fn cand(key: &str, model: &str) -> Candidate {
        Candidate {
            provider: "acme".into(),
            api_key: key.into(),
            account: None,
            region: None,
            model: model.into(),
            context_tokens: 128_000,
            modalities: Vec::new(),
            supports_tools: true,
            supports_structured_output: true,
            residency: None,
            est_latency_ms: 500,
            cost_per_mtok: Some(10.0),
            quality: Default::default(),
        }
    }

    fn subj(c: &Candidate) -> Subject<'_> {
        Subject {
            provider: &c.provider,
            api_key: &c.api_key,
            account: c.account.as_deref(),
            region: c.region.as_deref(),
            model: &c.model,
        }
    }

    fn fail(status: u16, body: &str) -> InferenceResult {
        InferenceResult::Failed {
            status: Some(status),
            body_snippet: body.into(),
            retry_after_secs: None,
        }
    }

    struct Fx {
        candidates: Vec<Candidate>,
        elig: EligibilityStore,
        quota: QuotaInventory,
        outcomes: FakeOutcomes,
    }

    impl Fx {
        fn stores(&self) -> BrainStores<'_> {
            BrainStores {
                eligibility: &self.elig,
                quotas: &self.quota,
                outcomes: &self.outcomes,
            }
        }
        fn decide(&self, policy: &BrainPolicy) -> BrainDecision {
            select_brain(
                policy,
                &IntentConstraints {
                    task_class: "coding".into(),
                    ..Default::default()
                },
                &self.candidates,
                &self.stores(),
                T0 + 60,
            )
        }
    }

    fn fx(_tag: &str) -> Fx {
        Fx {
            candidates: Vec::new(),
            elig: EligibilityStore::new(),
            quota: QuotaInventory::new(),
            outcomes: FakeOutcomes::default(),
        }
    }

    fn promote(l: &mut FakeOutcomes, model: &str, n: u32) {
        l.promote("coding", model, n);
    }

    fn reject(l: &mut FakeOutcomes, model: &str, n: u32) {
        l.reject("coding", model, n);
    }

    /// A verified-strong working model beats a verified-weak one even when
    /// the weak model is cheaper — quality leads within authorization.
    #[test]
    fn powerful_cloud_brain_policy_stronger_working_leads() {
        let mut f = fx("strong");
        let mut weak = cand("sk-w", "m-weak");
        weak.cost_per_mtok = None; // free — cheaper, must not lead
        let strong = cand("sk-s", "m-strong");
        f.elig
            .record_inference(subj(&weak), &InferenceResult::Success, T0);
        f.elig
            .record_inference(subj(&strong), &InferenceResult::Success, T0);
        f.candidates = vec![weak, strong];
        promote(&mut f.outcomes, "m-strong", 3);
        reject(&mut f.outcomes, "m-weak", 3);
        match f.decide(&BrainPolicy::default()) {
            BrainDecision::Cloud { model, .. } => assert_eq!(model, "m-strong"),
            _ => panic!("expected a cloud brain"),
        }
    }

    /// A stronger model that is currently blocked never leads.
    #[test]
    fn powerful_cloud_brain_policy_stronger_blocked_does_not_lead() {
        let mut f = fx("blocked");
        let weak = cand("sk-w", "m-weak");
        let strong = cand("sk-s", "m-strong");
        f.elig
            .record_inference(subj(&weak), &InferenceResult::Success, T0);
        f.elig
            .record_inference(subj(&strong), &fail(402, "insufficient credit"), T0);
        f.candidates = vec![weak, strong];
        promote(&mut f.outcomes, "m-strong", 5);
        match f.decide(&BrainPolicy::default()) {
            BrainDecision::Cloud { model, .. } => assert_eq!(model, "m-weak"),
            _ => panic!("expected a cloud brain"),
        }
    }

    /// An explicit user override leads while working; when it is blocked the
    /// decision is an explained block, not a silent fallback.
    #[test]
    fn powerful_cloud_brain_policy_override_is_binding() {
        let mut f = fx("override");
        let a = cand("sk-a", "m-best");
        let b = cand("sk-b", "m-pinned");
        f.elig
            .record_inference(subj(&a), &InferenceResult::Success, T0);
        f.elig
            .record_inference(subj(&b), &InferenceResult::Success, T0);
        f.candidates = vec![a, b];
        promote(&mut f.outcomes, "m-best", 4);
        let d = f.decide(&BrainPolicy {
            override_model: Some("m-pinned".into()),
            ..Default::default()
        });
        match d {
            BrainDecision::Cloud { model, .. } => assert_eq!(model, "m-pinned"),
            _ => panic!("pin must lead while working"),
        }
        // Blocked pin → Blocked, no silent fallback.
        let mut f2 = fx("override-blocked");
        let a = cand("sk-a", "m-best");
        let b = cand("sk-b", "m-pinned");
        f2.elig
            .record_inference(subj(&a), &InferenceResult::Success, T0);
        f2.elig
            .record_inference(subj(&b), &fail(401, "revoked"), T0);
        f2.candidates = vec![a, b];
        promote(&mut f2.outcomes, "m-best", 4);
        match f2.decide(&BrainPolicy {
            override_model: Some("m-pinned".into()),
            ..Default::default()
        }) {
            BrainDecision::Blocked { reasons } => assert!(!reasons.is_empty()),
            _ => panic!("blocked pin must be explained, not silently replaced"),
        }
    }

    /// Nothing working: permitted fallback engages, otherwise the blocker
    /// is explained with redacted reasons.
    #[test]
    fn powerful_cloud_brain_policy_no_qualifying_falls_back_or_explains() {
        let mut f = fx("none");
        let dead = cand("sk-d", "m-dead");
        f.elig
            .record_inference(subj(&dead), &fail(401, "invalid api key"), T0);
        f.candidates = vec![dead];
        match f.decide(&BrainPolicy {
            allow_local_fallback: true,
            ..Default::default()
        }) {
            BrainDecision::LocalFallback { reason } => assert!(!reason.is_empty()),
            _ => panic!("expected local fallback"),
        }
        match f.decide(&BrainPolicy::default()) {
            BrainDecision::Blocked { reasons } => assert_eq!(reasons.len(), 1),
            _ => panic!("expected blocked"),
        }
    }

    /// Marketing — price/context/name — never outranks verified outcomes:
    /// an expensive "flagship" with no evidence loses to a cheap verified
    /// model.
    #[test]
    fn powerful_cloud_brain_policy_marketing_never_leads() {
        let mut f = fx("marketing");
        let mut flagship = cand("sk-f", "m-flagship-ultra");
        flagship.cost_per_mtok = Some(100.0);
        flagship.context_tokens = 1_000_000;
        flagship.est_latency_ms = 50;
        let mut plain = cand("sk-p", "m-plain");
        plain.cost_per_mtok = Some(1.0);
        f.elig
            .record_inference(subj(&flagship), &InferenceResult::Success, T0);
        f.elig
            .record_inference(subj(&plain), &InferenceResult::Success, T0);
        f.candidates = vec![flagship, plain];
        promote(&mut f.outcomes, "m-plain", 4);
        match f.decide(&BrainPolicy::default()) {
            BrainDecision::Cloud { model, .. } => assert_eq!(model, "m-plain"),
            _ => panic!("expected a cloud brain"),
        }
    }

    /// A quota-exhausted account cannot lead even with top verified quality.
    #[test]
    fn powerful_cloud_brain_policy_quota_exhausted_cannot_lead() {
        let mut f = fx("quota");
        let spent = cand("sk-x", "m-spent");
        let ok = cand("sk-o", "m-ok");
        f.elig
            .record_inference(subj(&spent), &InferenceResult::Success, T0);
        f.elig
            .record_inference(subj(&ok), &InferenceResult::Success, T0);
        f.quota.record(
            QuotaObservation {
                provider: "acme".into(),
                credential: credential_fingerprint("sk-x"),
                account: String::new(),
                model: String::new(),
                kind: QuotaKind::Requests,
                amount: QuotaAmount::Limited(0),
                resets_at_unix: Some(T0 + 3600),
                retry_after_secs: None,
                observed_unix: T0,
                stale_after_secs: 3600,
                source: susi_vendor_models::cloud_quota::QuotaSource::Headers,
            },
            T0,
        );
        f.candidates = vec![spent, ok];
        promote(&mut f.outcomes, "m-spent", 5);
        match f.decide(&BrainPolicy::default()) {
            BrainDecision::Cloud { model, .. } => assert_eq!(model, "m-ok"),
            _ => panic!("expected a cloud brain"),
        }
    }

    /// Unproven (unknown-outcome) models never outrank verified-good ones,
    /// and unknown-availability candidates never lead the brain at all.
    #[test]
    fn powerful_cloud_brain_policy_unproven_and_unknown_never_lead() {
        let mut f = fx("unknown");
        let mystery = cand("sk-m", "m-mystery"); // no evidence at all
        let good = cand("sk-g", "m-good");
        f.elig
            .record_inference(subj(&good), &InferenceResult::Success, T0);
        f.candidates = vec![mystery, good];
        promote(&mut f.outcomes, "m-good", 2);
        match f.decide(&BrainPolicy::default()) {
            BrainDecision::Cloud {
                model,
                alternatives,
                ..
            } => {
                assert_eq!(model, "m-good");
                // Unknown-availability candidate doesn't even appear as a
                // proven alternative.
                assert!(alternatives.is_empty());
            }
            _ => panic!("expected a cloud brain"),
        }
    }
}
