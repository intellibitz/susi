//! Route every supported reasoning workflow through the shared
//! strongest-working-cloud brain selector (T-CODEX-24 / VC-201-006).
//!
//! `REASONING_ENTRY_POINTS` is the testable inventory of reasoning
//! surfaces. Each is dispatched by [`BrainRouter::reason`], which runs
//! [`select_brain`](crate::cloud_brain_policy::select_brain) first — the
//! same default policy for every entry point, so no workflow can silently
//! fall back to a local or weaker model outside the policy. Deterministic
//! control-plane commands and native tool execution are *not* reasoning
//! workflows and are deliberately absent from the inventory.
//!
//! A routed call carries the selected brain's opaque id plus the entry
//! point, so receipts prove the brain actually received the request —
//! not merely that a preference was recorded.

use std::fmt;

use susi_gawd_agents::cloud_brain_policy::{select_brain, BrainDecision, BrainPolicy, BrainStores};
use susi_gawd_agents::cloud_intent::{Candidate, IntentConstraints};

/// A reasoning workflow routed through the cloud brain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEntryPoint {
    /// Interpreting a user intent into structured goals.
    IntentInterpretation,
    /// Plan construction.
    Planning,
    /// Decomposing a plan into tasks.
    TaskDecomposition,
    /// Choosing tools for a step.
    ToolSelection,
    /// Reasoning during execution.
    ExecutionReasoning,
    /// Diagnosing errors/failures.
    ErrorDiagnosis,
    /// Coordinating swarm workers.
    SwarmCoordination,
    /// Synthesizing memory/context.
    MemorySynthesis,
    /// Independent review of a candidate change.
    IndependentVerification,
    /// Self-improvement proposal/reflection.
    SelfImprovement,
}

impl fmt::Display for ReasoningEntryPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::IntentInterpretation => "intent_interpretation",
            Self::Planning => "planning",
            Self::TaskDecomposition => "task_decomposition",
            Self::ToolSelection => "tool_selection",
            Self::ExecutionReasoning => "execution_reasoning",
            Self::ErrorDiagnosis => "error_diagnosis",
            Self::SwarmCoordination => "swarm_coordination",
            Self::MemorySynthesis => "memory_synthesis",
            Self::IndependentVerification => "independent_verification",
            Self::SelfImprovement => "self_improvement",
        };
        f.write_str(s)
    }
}

/// The complete inventory of reasoning entry points — tests iterate this
/// to prove every surface reaches the selected brain.
pub const REASONING_ENTRY_POINTS: &[ReasoningEntryPoint] = &[
    ReasoningEntryPoint::IntentInterpretation,
    ReasoningEntryPoint::Planning,
    ReasoningEntryPoint::TaskDecomposition,
    ReasoningEntryPoint::ToolSelection,
    ReasoningEntryPoint::ExecutionReasoning,
    ReasoningEntryPoint::ErrorDiagnosis,
    ReasoningEntryPoint::SwarmCoordination,
    ReasoningEntryPoint::MemorySynthesis,
    ReasoningEntryPoint::IndependentVerification,
    ReasoningEntryPoint::SelfImprovement,
];

/// Dispatch one redacted prompt to the selected brain's opaque credential
/// target. Production wiring binds this to the real provider transport;
/// tests bind a recording fake.
pub trait BrainDispatch {
    /// Send `prompt` to `target` (opaque `provider/model/fingerprint8`).
    /// Must never receive raw key material.
    fn dispatch(&self, target: &str, prompt: &str) -> Result<String, String>;
}

/// Result of a routed reasoning call.
#[derive(Debug)]
pub struct RoutedResponse {
    /// Which entry point ran.
    pub entry_point: ReasoningEntryPoint,
    /// Opaque brain that received the request — proof of routing.
    pub brain: String,
    /// The brain's response.
    pub body: String,
}

/// Why a reasoning call could not be routed.
#[derive(Debug)]
pub enum RouteError {
    /// No working cloud brain qualified; policy permits local fallback —
    /// callers decide whether a local path exists for this entry point.
    LocalFallbackOnly { reason: String },
    /// No working cloud brain and no permitted fallback — honest blocker.
    Blocked { reasons: usize },
    /// The selected brain's dispatch failed.
    DispatchFailed { brain: String, error: String },
}

/// Routes reasoning entry points through the default brain policy.
pub struct BrainRouter<'a> {
    /// Brain selection policy (override binding, local fallback).
    pub policy: &'a BrainPolicy,
    /// Live evidence stores.
    pub stores: BrainStores<'a>,
    /// Candidate inventory.
    pub candidates: &'a [Candidate],
    /// Per-entry-point intent constraints (task class, capabilities).
    pub intent_for: &'a dyn Fn(ReasoningEntryPoint) -> IntentConstraints,
    /// Dispatch to the selected brain.
    pub dispatch: &'a dyn BrainDispatch,
    /// Clock (unix secs) for evidence freshness.
    pub now: u64,
}

impl BrainRouter<'_> {
    /// Run one reasoning step: select the brain by the entry point's
    /// constraints, then dispatch the redacted prompt to it.
    pub fn reason(
        &self,
        entry_point: ReasoningEntryPoint,
        prompt: &str,
    ) -> Result<RoutedResponse, RouteError> {
        let intent = (self.intent_for)(entry_point);
        match select_brain(
            self.policy,
            &intent,
            self.candidates,
            &self.stores,
            self.now,
        ) {
            BrainDecision::Cloud { lead_opaque, .. } => self
                .dispatch
                .dispatch(&lead_opaque, prompt)
                .map(|body| RoutedResponse {
                    entry_point,
                    brain: lead_opaque.clone(),
                    body,
                })
                .map_err(|error| RouteError::DispatchFailed {
                    brain: lead_opaque,
                    error,
                }),
            BrainDecision::LocalFallback { reason } => {
                Err(RouteError::LocalFallbackOnly { reason })
            }
            BrainDecision::Blocked { reasons } => Err(RouteError::Blocked {
                reasons: reasons.len(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
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
        candidates: Vec<Candidate>,
        elig: EligibilityStore,
        quota: QuotaInventory,
        outcomes: crate::cloud_rsi_outcomes::OutcomeLedger,
        dir: std::path::PathBuf,
        dispatch: RecordingDispatch,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

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

    fn fx(tag: &str) -> Fx {
        let dir = std::env::temp_dir().join(format!("bw-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Fx {
            candidates: Vec::new(),
            elig: EligibilityStore::new(),
            quota: QuotaInventory::new(),
            outcomes: crate::cloud_rsi_outcomes::OutcomeLedger::load(dir.clone(), || T0),
            dir,
            dispatch: RecordingDispatch {
                calls: RefCell::new(Vec::new()),
            },
        }
    }

    fn router<'a>(f: &'a Fx, policy: &'a BrainPolicy) -> BrainRouter<'a> {
        BrainRouter {
            policy,
            stores: BrainStores {
                eligibility: &f.elig,
                quotas: &f.quota,
                outcomes: &f.outcomes,
            },
            candidates: &f.candidates,
            intent_for: &|_| IntentConstraints {
                task_class: "reasoning".into(),
                ..Default::default()
            },
            dispatch: &f.dispatch,
            now: T0 + 60,
        }
    }

    fn promote(l: &mut crate::cloud_rsi_outcomes::OutcomeLedger, model: &str, n: u32) {
        for i in 0..n {
            l.record(
                "reasoning",
                model,
                crate::cloud_rsi_outcomes::Outcome::Promoted {
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

    /// Every entry point in the inventory actually dispatches to the
    /// selected brain — not just a recorded preference.
    #[test]
    fn powerful_cloud_brain_wiring_every_entry_point_reaches_brain() {
        let mut f = fx("all");
        let a = cand("sk-a", "m-strong");
        let b = cand("sk-b", "m-weak");
        f.elig
            .record_inference(subj(&a), &InferenceResult::Success, T0);
        f.elig
            .record_inference(subj(&b), &InferenceResult::Success, T0);
        f.candidates = vec![a, b];
        promote(&mut f.outcomes, "m-strong", 3);
        let policy = BrainPolicy::default();
        let r = router(&f, &policy);
        for ep in REASONING_ENTRY_POINTS {
            let resp = r.reason(*ep, "redacted prompt").unwrap();
            assert_eq!(resp.entry_point, *ep);
            assert!(
                resp.brain.contains("m-strong"),
                "{} routed to {}",
                ep,
                resp.brain
            );
        }
        // Ten entry points → ten real dispatches to the lead brain.
        assert_eq!(
            f.dispatch.calls.borrow().len(),
            REASONING_ENTRY_POINTS.len()
        );
        assert!(f
            .dispatch
            .calls
            .borrow()
            .iter()
            .all(|(t, _)| t.contains("m-strong")));
    }

    /// With nothing working, routing is an honest block — no silent local
    /// dispatch, no fake success.
    #[test]
    fn powerful_cloud_brain_wiring_blocked_brain_never_dispatches() {
        let mut f = fx("blocked");
        let dead = cand("sk-d", "m-dead");
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
        let r = router(&f, &policy);
        match r.reason(ReasoningEntryPoint::Planning, "p") {
            Err(RouteError::Blocked { reasons }) => assert_eq!(reasons, 1),
            _ => panic!("expected blocked routing"),
        }
        assert!(f.dispatch.calls.borrow().is_empty());
    }

    /// Permitted local fallback is reported as such — it never masquerades
    /// as a cloud dispatch.
    #[test]
    fn powerful_cloud_brain_wiring_fallback_is_explicit() {
        let mut f = fx("fallback");
        let dead = cand("sk-d", "m-dead");
        f.elig.record_inference(
            subj(&dead),
            &InferenceResult::Failed {
                status: Some(402),
                body_snippet: "no credit".into(),
                retry_after_secs: None,
            },
            T0,
        );
        f.candidates = vec![dead];
        let policy = BrainPolicy {
            allow_local_fallback: true,
            ..Default::default()
        };
        let r = router(&f, &policy);
        match r.reason(ReasoningEntryPoint::ErrorDiagnosis, "p") {
            Err(RouteError::LocalFallbackOnly { reason }) => assert!(!reason.is_empty()),
            _ => panic!("expected explicit local fallback"),
        }
        assert!(f.dispatch.calls.borrow().is_empty());
    }

    /// The user's pinned brain receives the requests while working.
    #[test]
    fn powerful_cloud_brain_wiring_override_brain_receives_requests() {
        let mut f = fx("pin");
        let a = cand("sk-a", "m-best");
        let b = cand("sk-b", "m-pinned");
        f.elig
            .record_inference(subj(&a), &InferenceResult::Success, T0);
        f.elig
            .record_inference(subj(&b), &InferenceResult::Success, T0);
        f.candidates = vec![a, b];
        promote(&mut f.outcomes, "m-best", 3);
        let policy = BrainPolicy {
            override_model: Some("m-pinned".into()),
            ..Default::default()
        };
        let r = router(&f, &policy);
        let resp = r.reason(ReasoningEntryPoint::SelfImprovement, "p").unwrap();
        assert!(resp.brain.contains("m-pinned"));
        assert!(f.dispatch.calls.borrow()[0].0.contains("m-pinned"));
    }

    /// Dispatch failure surfaces with the brain identity — not retried
    /// silently onto another model inside the router.
    #[test]
    fn powerful_cloud_brain_wiring_dispatch_failure_is_attributed() {
        struct FailDispatch;
        impl BrainDispatch for FailDispatch {
            fn dispatch(&self, target: &str, _p: &str) -> Result<String, String> {
                Err(format!("timeout {target}"))
            }
        }
        let mut f = fx("derr");
        let a = cand("sk-a", "m-only");
        f.elig
            .record_inference(subj(&a), &InferenceResult::Success, T0);
        f.candidates = vec![a];
        let policy = BrainPolicy::default();
        let fd = FailDispatch;
        let r = BrainRouter {
            policy: &policy,
            stores: BrainStores {
                eligibility: &f.elig,
                quotas: &f.quota,
                outcomes: &f.outcomes,
            },
            candidates: &f.candidates,
            intent_for: &|_| IntentConstraints::default(),
            dispatch: &fd,
            now: T0 + 60,
        };
        match r.reason(ReasoningEntryPoint::ToolSelection, "p") {
            Err(RouteError::DispatchFailed { brain, error }) => {
                assert!(brain.contains("m-only"));
                assert!(error.contains("timeout"));
            }
            _ => panic!("expected dispatch failure"),
        }
    }
}
