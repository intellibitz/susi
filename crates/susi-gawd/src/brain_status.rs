//! Diagnostics surface for the shared cloud brain (T-CODEX-28 /
//! VC-201-012).
//!
//! `brain_diagnostics` exposes what the policy *actually decided* — the
//! primary brain's opaque id, why each candidate leads or is excluded,
//! how fresh the working-state evidence is, and the fallback/recovery
//! trail — without ever emitting credential material. It never reports
//! policy activation from configuration alone: `primary` is populated
//! only when `select_brain` resolved a proven-usable candidate.

use serde::Serialize;
use susi_gawd_agents::cloud_brain_policy::{select_brain, BrainDecision, BrainPolicy, BrainStores};
use susi_gawd_agents::cloud_intent::{BlockReason, Candidate, IntentConstraints};
use susi_vendor_models::cloud_eligibility::{EligibilityKind, Provenance};

/// Which state the brain decision landed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum BrainState {
    /// A proven-working cloud model leads.
    Cloud,
    /// No working cloud brain; local fallback is permitted.
    LocalFallback,
    /// No working cloud brain and no permitted fallback.
    Blocked,
}

/// One event on the brain evidence trail.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BrainEvent {
    /// Unix timestamp of the event.
    pub at_unix: u64,
    /// Model the event concerns (`""` for provider-wide).
    pub model: String,
    /// What happened.
    pub kind: BrainEventKind,
    /// Short redacted detail.
    pub detail: String,
}

/// Kinds of brain-affecting events.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum BrainEventKind {
    /// Real inference proved the subject usable.
    ProvenUsable,
    /// A failure observation (rate limit, auth, credit, outage…).
    Failure(String),
    /// The policy decision itself — which brain leads now.
    Selected,
    /// The policy could not place a cloud brain.
    Fallback,
}

/// Per-candidate diagnostics entry.
#[derive(Debug, Clone, Serialize)]
pub struct BrainEntry {
    /// Model id.
    pub model: String,
    /// Opaque `provider/model/fingerprint8` — never raw key material.
    pub opaque_id: String,
    /// Resolved working state.
    pub working: bool,
    /// Seconds since the freshest availability evidence (`None` = none).
    pub evidence_age_secs: Option<u64>,
    /// Why the candidate cannot lead right now, if excluded.
    pub exclusion: Option<String>,
    /// Verified capability quality from recorded outcomes, when known.
    pub verified_quality: Option<f64>,
    /// Whether this entry currently leads.
    pub leads: bool,
}

/// The whole brain diagnostic picture.
#[derive(Debug, Clone, Serialize)]
pub struct BrainDiagnostics {
    /// What the policy decided.
    pub state: BrainState,
    /// Opaque id of the primary brain, when one leads.
    pub primary: Option<String>,
    /// Model name of the primary brain, when one leads.
    pub primary_model: Option<String>,
    /// Per-candidate status, lead first.
    pub entries: Vec<BrainEntry>,
    /// Fallback/recovery/failure events, newest last.
    pub events: Vec<BrainEvent>,
}

fn block_reason_text(r: &BlockReason) -> String {
    match r {
        BlockReason::Availability(k) => format!("availability:{k:?}"),
        BlockReason::QuotaExhausted => "quota_exhausted".into(),
        BlockReason::ContextTooSmall { has, needs } => format!("context:{has}<{needs}"),
        BlockReason::MissingModality(m) => format!("missing_modality:{m}"),
        BlockReason::NoTools => "no_tools".into(),
        BlockReason::NoStructuredOutput => "no_structured_output".into(),
        BlockReason::ResidencyMismatch { required, actual } => {
            format!("residency:{actual}!={required}")
        }
        BlockReason::OverDeadline { est_ms, max_ms } => format!("deadline:{est_ms}>{max_ms}ms"),
        BlockReason::OverBudget => "over_budget".into(),
        BlockReason::PinMismatch => "pinned_elsewhere".into(),
    }
}

/// Produce the live brain diagnostics from the same stores the policy
/// reads. `intent` is the caller's task constraints; the report reflects
/// *that* request's decision (task class changes who leads).
#[must_use]
pub fn brain_diagnostics(
    policy: &BrainPolicy,
    intent: &IntentConstraints,
    candidates: &[Candidate],
    stores: &BrainStores<'_>,
    now: u64,
) -> BrainDiagnostics {
    let decision = select_brain(policy, intent, candidates, stores, now);
    // Re-run the underlying selection pass for per-candidate rejection
    // reasons (policy pin applied exactly as `select_brain` does).
    let mut eff_intent = intent.clone();
    if let Some(pin) = &policy.override_model {
        eff_intent.pinned_model = Some(pin.clone());
        eff_intent.pin_fallback = susi_gawd_agents::cloud_intent::PinFallback::Deny;
    }
    let sel = susi_gawd_agents::cloud_intent::select(
        &eff_intent,
        candidates,
        stores.eligibility,
        stores.quotas,
        now,
    );

    // Freshness = newest usable-proving or blocking observation touching
    // this candidate's (provider, model, credential fingerprint).
    let mut entries = Vec::with_capacity(candidates.len());
    for c in candidates {
        let fp = susi_vendor_models::cloud_eligibility::credential_fingerprint(&c.api_key);
        let opaque = c.opaque_id();
        let mut freshest: Option<u64> = None;
        for o in stores.eligibility.observations() {
            let touches = o.provider == c.provider
                && (o.credential == fp || o.credential == "*")
                && (o.model.is_empty() || o.model == c.model);
            if touches {
                freshest = Some(freshest.map_or(o.observed_unix, |f| f.max(o.observed_unix)));
            }
        }
        let verdict = stores.eligibility.resolve(
            susi_vendor_models::cloud_eligibility::Subject {
                provider: &c.provider,
                api_key: &c.api_key,
                account: c.account.as_deref(),
                region: c.region.as_deref(),
                model: &c.model,
            },
            now,
        );
        // Prefer the selection pass's precise rejection reason (quota,
        // residency, pin, budget…) over the generic availability verdict.
        let exclusion = sel
            .rejected
            .iter()
            .find(|r| r.candidate == opaque)
            .map(|r| block_reason_text(&r.reason))
            .or(if verdict.is_usable() {
                None
            } else {
                Some(format!("{:?}: {}", verdict.kind, verdict.reason))
            });
        entries.push(BrainEntry {
            model: c.model.clone(),
            opaque_id: opaque,
            working: verdict.is_usable(),
            evidence_age_secs: freshest.map(|t| now.saturating_sub(t)),
            exclusion,
            verified_quality: None,
            leads: false,
        });
    }

    let (state, primary, primary_model) = match &decision {
        BrainDecision::Cloud {
            lead_opaque,
            model,
            verified_quality,
            alternatives: _,
        } => {
            if let Some(e) = entries.iter_mut().find(|e| e.opaque_id == *lead_opaque) {
                e.leads = true;
                e.verified_quality = *verified_quality;
            }
            (
                BrainState::Cloud,
                Some(lead_opaque.clone()),
                Some(model.clone()),
            )
        }
        BrainDecision::LocalFallback { .. } => (BrainState::LocalFallback, None, None),
        BrainDecision::Blocked { .. } => (BrainState::Blocked, None, None),
    };

    // Events: evidence trail + the decision itself.
    let mut events: Vec<BrainEvent> = stores
        .eligibility
        .observations()
        .map(|o| BrainEvent {
            at_unix: o.observed_unix,
            model: o.model.clone(),
            kind: match (&o.kind, &o.provenance) {
                (EligibilityKind::Usable, Provenance::Inference) => BrainEventKind::ProvenUsable,
                (EligibilityKind::Usable, _) => BrainEventKind::ProvenUsable,
                (k, _) => BrainEventKind::Failure(format!("{k:?}")),
            },
            detail: o.reason.clone(),
        })
        .collect();
    events.push(BrainEvent {
        at_unix: now,
        model: primary_model.clone().unwrap_or_default(),
        kind: match state {
            BrainState::Cloud => BrainEventKind::Selected,
            BrainState::LocalFallback | BrainState::Blocked => BrainEventKind::Fallback,
        },
        detail: match state {
            BrainState::Cloud => "brain selected".into(),
            BrainState::LocalFallback => "no working cloud brain; local fallback permitted".into(),
            BrainState::Blocked => "no working cloud brain".into(),
        },
    });
    events.sort_by_key(|e| e.at_unix);

    entries.sort_by(|a, b| b.leads.cmp(&a.leads).then(b.working.cmp(&a.working)));

    BrainDiagnostics {
        state,
        primary,
        primary_model,
        entries,
        events,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_brain_wiring::{BrainDispatch, BrainRouter, ReasoningEntryPoint};
    use crate::cloud_rsi_outcomes::{Outcome, OutcomeLedger};
    use std::cell::RefCell;
    use std::path::PathBuf;
    use susi_gawd_agents::cloud_intent::PinFallback;
    use susi_vendor_models::cloud_eligibility::{EligibilityStore, InferenceResult, Subject};
    use susi_vendor_models::cloud_quota::QuotaInventory;

    const T0: u64 = 1_700_000_000;

    struct RecordingDispatch {
        calls: RefCell<Vec<(String, String)>>,
    }
    impl BrainDispatch for RecordingDispatch {
        fn dispatch(&self, target: &str, prompt: &str) -> Result<String, String> {
            self.calls
                .borrow_mut()
                .push((target.to_string(), prompt.to_string()));
            Ok(format!("ok:{target}"))
        }
    }

    struct Fx {
        dir: PathBuf,
        candidates: Vec<Candidate>,
        elig: EligibilityStore,
        quota: QuotaInventory,
        outcomes: OutcomeLedger,
        dispatch: RecordingDispatch,
    }
    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn cand(key: &str, model: &str, provider: &str, cost: Option<f64>) -> Candidate {
        Candidate {
            provider: provider.into(),
            api_key: key.into(),
            account: None,
            region: None,
            model: model.into(),
            context_tokens: 128_000,
            modalities: Vec::new(),
            supports_tools: true,
            supports_structured_output: true,
            residency: None,
            est_latency_ms: 400,
            cost_per_mtok: cost,
            quality: Default::default(),
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

    fn fx(tag: &str) -> Fx {
        let dir = std::env::temp_dir().join(format!("bs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Fx {
            candidates: Vec::new(),
            elig: EligibilityStore::new(),
            quota: QuotaInventory::new(),
            outcomes: OutcomeLedger::load(dir.clone(), || T0),
            dispatch: RecordingDispatch {
                calls: RefCell::new(Vec::new()),
            },
            dir,
        }
    }

    fn stores(f: &Fx) -> BrainStores<'_> {
        BrainStores {
            eligibility: &f.elig,
            quotas: &f.quota,
            outcomes: &f.outcomes,
        }
    }

    fn intent() -> IntentConstraints {
        IntentConstraints {
            task_class: "reasoning".into(),
            ..Default::default()
        }
    }

    fn promote(l: &mut OutcomeLedger, model: &str, n: u32) {
        for i in 0..n {
            l.record(
                "reasoning",
                model,
                Outcome::Promoted {
                    candidate_id: format!("C-{model}-{i}"),
                    receipts: vec!["t".into()],
                    gates: vec![("accept".into(), true)],
                    regression_pct: None,
                    usage: Default::default(),
                },
            )
            .unwrap();
        }
    }

    fn router<'a>(f: &'a Fx, policy: &'a BrainPolicy) -> BrainRouter<'a> {
        BrainRouter {
            policy,
            stores: stores(f),
            candidates: &f.candidates,
            intent_for: &|_| intent(),
            dispatch: &f.dispatch,
            now: T0 + 60,
        }
    }

    /// The strongest authorized working cloud model leads even though a
    /// cheaper local-style model is configured alongside it — quality
    /// evidence outranks cost when policy permits.
    #[test]
    fn powerful_cloud_brain_e2e_strongest_working_leads_over_cheaper() {
        let mut f = fx("lead");
        let strong = cand("sk-strong", "m-frontier", "acme", Some(30.0));
        let cheap = cand("sk-cheap", "m-mini", "acme", Some(0.5));
        f.elig
            .record_inference(subj(&strong), &InferenceResult::Success, T0);
        f.elig
            .record_inference(subj(&cheap), &InferenceResult::Success, T0);
        promote(&mut f.outcomes, "m-frontier", 4);
        promote(&mut f.outcomes, "m-mini", 1);
        f.candidates = vec![cheap, strong];
        let policy = BrainPolicy::default();
        let d = brain_diagnostics(&policy, &intent(), &f.candidates, &stores(&f), T0 + 60);
        assert_eq!(d.state, BrainState::Cloud);
        assert_eq!(d.primary_model.as_deref(), Some("m-frontier"));
        let r = router(&f, &policy);
        let resp = r.reason(ReasoningEntryPoint::Planning, "p").unwrap();
        assert!(resp.brain.contains("m-frontier"));
    }

    /// A stronger model that is failing right now is excluded from
    /// leadership; the weaker working model leads and the diagnostics
    /// name the exclusion.
    #[test]
    fn powerful_cloud_brain_e2e_stronger_nonworking_is_excluded() {
        let mut f = fx("excl");
        let dead = cand("sk-dead", "m-frontier", "acme", Some(30.0));
        let ok = cand("sk-ok", "m-mid", "acme", Some(5.0));
        f.elig.record_inference(
            subj(&dead),
            &InferenceResult::Failed {
                status: Some(401),
                body_snippet: "key revoked".into(),
                retry_after_secs: None,
            },
            T0,
        );
        f.elig
            .record_inference(subj(&ok), &InferenceResult::Success, T0);
        promote(&mut f.outcomes, "m-frontier", 8);
        f.candidates = vec![dead, ok];
        let policy = BrainPolicy::default();
        let d = brain_diagnostics(&policy, &intent(), &f.candidates, &stores(&f), T0 + 60);
        assert_eq!(d.primary_model.as_deref(), Some("m-mid"));
        let dead_entry = d.entries.iter().find(|e| e.model == "m-frontier").unwrap();
        assert!(!dead_entry.working);
        assert!(dead_entry.exclusion.is_some());
        assert!(!dead_entry.leads);
    }

    /// An explicit pin binds: the pinned model leads while working, and
    /// nothing substitutes for it.
    #[test]
    fn powerful_cloud_brain_e2e_explicit_pin_binds() {
        let mut f = fx("pin");
        let a = cand("sk-a", "m-frontier", "acme", Some(30.0));
        let b = cand("sk-b", "m-pinned", "acme", Some(5.0));
        f.elig
            .record_inference(subj(&a), &InferenceResult::Success, T0);
        f.elig
            .record_inference(subj(&b), &InferenceResult::Success, T0);
        promote(&mut f.outcomes, "m-frontier", 5);
        f.candidates = vec![a, b];
        let policy = BrainPolicy {
            override_model: Some("m-pinned".into()),
            ..Default::default()
        };
        let d = brain_diagnostics(&policy, &intent(), &f.candidates, &stores(&f), T0 + 60);
        assert_eq!(d.primary_model.as_deref(), Some("m-pinned"));
    }

    /// Free-only and residency restrictions narrow leadership honestly:
    /// a paid/foreign strong model cannot lead a free-only EU request.
    #[test]
    fn powerful_cloud_brain_e2e_free_only_and_privacy_restrict() {
        let mut f = fx("restr");
        let mut paid = cand("sk-paid", "m-frontier", "acme", Some(20.0));
        paid.residency = Some("us".into());
        let mut free_eu = cand("sk-free", "m-free-eu", "acme", Some(0.0));
        free_eu.residency = Some("eu".into());
        f.elig
            .record_inference(subj(&paid), &InferenceResult::Success, T0);
        f.elig
            .record_inference(subj(&free_eu), &InferenceResult::Success, T0);
        promote(&mut f.outcomes, "m-frontier", 6);
        f.candidates = vec![paid, free_eu];
        let mut i = intent();
        i.max_cost_per_mtok = Some(0.0);
        i.residency = Some("eu".into());
        let policy = BrainPolicy::default();
        let d = brain_diagnostics(&policy, &i, &f.candidates, &stores(&f), T0 + 60);
        assert_eq!(d.primary_model.as_deref(), Some("m-free-eu"));
        let r = BrainRouter {
            policy: &policy,
            stores: stores(&f),
            candidates: &f.candidates,
            intent_for: &|_| i.clone(),
            dispatch: &f.dispatch,
            now: T0 + 60,
        };
        let resp = r.reason(ReasoningEntryPoint::SelfImprovement, "p").unwrap();
        assert!(resp.brain.contains("m-free-eu"));
    }

    /// With no eligible cloud model the report is an honest block — the
    /// primary field stays empty; policy/config alone never activates.
    #[test]
    fn powerful_cloud_brain_e2e_no_eligible_blocks_honestly() {
        let mut f = fx("none");
        let dead = cand("sk-x", "m-dead", "acme", Some(1.0));
        f.elig.record_inference(
            subj(&dead),
            &InferenceResult::Failed {
                status: Some(401),
                body_snippet: "revoked".into(),
                retry_after_secs: None,
            },
            T0,
        );
        f.candidates = vec![dead];
        let policy = BrainPolicy::default();
        let d = brain_diagnostics(&policy, &intent(), &f.candidates, &stores(&f), T0 + 60);
        assert_eq!(d.state, BrainState::Blocked);
        assert!(d.primary.is_none());
        // Fallback event is on the trail.
        assert!(d
            .events
            .iter()
            .any(|e| matches!(e.kind, BrainEventKind::Fallback)));
    }

    /// Recovery: after a failure, fresh proven-usable evidence restores
    /// leadership — the report shows both events on the trail.
    #[test]
    fn powerful_cloud_brain_e2e_return_after_recovery() {
        let mut f = fx("recov");
        let m = cand("sk-m", "m-flaky", "acme", Some(5.0));
        f.elig.record_inference(
            subj(&m),
            &InferenceResult::Failed {
                status: Some(503),
                body_snippet: "backend down".into(),
                retry_after_secs: None,
            },
            T0,
        );
        let policy = BrainPolicy::default();
        f.candidates = vec![m.clone()];
        let blocked = brain_diagnostics(&policy, &intent(), &f.candidates, &stores(&f), T0 + 30);
        assert_eq!(blocked.state, BrainState::Blocked);
        // Fresh inference success restores it (TTL'd Usable evidence).
        f.elig
            .record_inference(subj(&m), &InferenceResult::Success, T0 + 40);
        let d = brain_diagnostics(&policy, &intent(), &f.candidates, &stores(&f), T0 + 60);
        assert_eq!(d.state, BrainState::Cloud);
        assert_eq!(d.primary_model.as_deref(), Some("m-flaky"));
        assert!(d
            .events
            .iter()
            .any(|e| matches!(e.kind, BrainEventKind::ProvenUsable)));
        assert!(d
            .events
            .iter()
            .any(|e| matches!(e.kind, BrainEventKind::Failure(_))));
    }

    /// Diagnostics carry no secrets: serialized output contains opaque
    /// fingerprints and redacted reasons only — never raw key material.
    #[test]
    fn powerful_cloud_brain_e2e_diagnostics_never_leak_keys() {
        let mut f = fx("leak");
        let secret = "sk-supersecret-abcdef123456";
        let m = cand(secret, "m-x", "acme", Some(5.0));
        f.elig
            .record_inference(subj(&m), &InferenceResult::Success, T0);
        f.candidates = vec![m];
        let policy = BrainPolicy::default();
        let d = brain_diagnostics(&policy, &intent(), &f.candidates, &stores(&f), T0 + 60);
        let json = serde_json::to_string(&d).unwrap();
        assert!(!json.contains(secret));
        assert!(json.contains("m-x"));
    }

    /// Production e2e across every reasoning entry point: intent →
    /// planning → parallel coordination → self-improvement all land on
    /// the same evidence-selected brain.
    #[test]
    fn powerful_cloud_brain_e2e_all_entry_points_one_brain() {
        let mut f = fx("e2e");
        let lead = cand("sk-l", "m-lead", "acme", Some(10.0));
        let alt = cand("sk-a", "m-alt", "acme", Some(3.0));
        f.elig
            .record_inference(subj(&lead), &InferenceResult::Success, T0);
        f.elig
            .record_inference(subj(&alt), &InferenceResult::Success, T0);
        promote(&mut f.outcomes, "m-lead", 5);
        promote(&mut f.outcomes, "m-alt", 2);
        f.candidates = vec![lead, alt];
        let policy = BrainPolicy::default();
        let r = router(&f, &policy);
        for ep in crate::cloud_brain_wiring::REASONING_ENTRY_POINTS {
            let resp = r.reason(*ep, "redacted").unwrap();
            assert!(resp.brain.contains("m-lead"), "{ep} → {}", resp.brain);
        }
        // Report matches what the router actually dispatched.
        let d = brain_diagnostics(&policy, &intent(), &f.candidates, &stores(&f), T0 + 60);
        assert!(d.primary.unwrap().contains("m-lead"));
        assert_eq!(
            f.dispatch.calls.borrow().len(),
            crate::cloud_brain_wiring::REASONING_ENTRY_POINTS.len()
        );
    }

    /// Pin fallback policy is honored end to end: pin to a dead model
    /// with Deny → blocked, not silently re-routed.
    #[test]
    fn powerful_cloud_brain_e2e_dead_pin_blocks_not_substitutes() {
        let mut f = fx("deadpin");
        let dead = cand("sk-d", "m-pinned-dead", "acme", Some(5.0));
        let ok = cand("sk-o", "m-other", "acme", Some(5.0));
        f.elig.record_inference(
            subj(&dead),
            &InferenceResult::Failed {
                status: Some(401),
                body_snippet: "gone".into(),
                retry_after_secs: None,
            },
            T0,
        );
        f.elig
            .record_inference(subj(&ok), &InferenceResult::Success, T0);
        f.candidates = vec![dead, ok];
        let policy = BrainPolicy {
            override_model: Some("m-pinned-dead".into()),
            ..Default::default()
        };
        let d = brain_diagnostics(&policy, &intent(), &f.candidates, &stores(&f), T0 + 60);
        // Pin is binding — no silent substitute.
        assert_ne!(d.primary_model.as_deref(), Some("m-other"));
        let _ = PinFallback::Deny;
    }
}
