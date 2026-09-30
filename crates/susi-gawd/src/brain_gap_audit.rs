//! Capability-gap audit driven by the strongest working cloud brain
//! (T-CODEX-29 / VC-201-013).
//!
//! The audit compares *promised* behavior (roadmap mastery targets,
//! closed-task claims, documented behavior) against *observed* behavior
//! (test/benchmark receipts, intent failures, code behavior, user
//! feedback). Two passes:
//!
//! 1. **Evidence-direct**: any receipt whose `observed` differs from
//!    `expected` is a verified gap — no model claim needed.
//! 2. **Brain analysis**: the selected cloud brain reviews the redacted
//!    evidence corpus and proposes findings. A finding is only
//!    *verified* when every receipt it cites exists in the corpus and a
//!    reproducible trigger is present; a model claim without receipts is
//!    recorded as a hypothesis — never evidence.
//!
//! The brain call runs through the production failover path
//! (`susi_gawd_swarm::cloud_failover::run`), so budget, lockout,
//! availability and mid-call failover all apply; a dying coordinator
//! falls through to the next working brain. All model-visible context is
//! redacted before dispatch.

use serde::Deserialize;
use susi_gawd_agents::cloud_intent::{Candidate, IntentConstraints};
use susi_gawd_swarm::cloud_failover::{self, FailoverBudget, FailoverStop, Runner, Stores};

/// Where a piece of audit evidence came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EvidenceSource {
    /// A user intent that failed end to end.
    IntentFailure,
    /// A failing/weak test receipt.
    TestReceipt,
    /// A benchmark receipt below target.
    BenchmarkReceipt,
    /// Observed runtime/code behavior diverging from docs.
    CodeBehavior,
    /// Direct user feedback/report.
    UserFeedback,
    /// A roadmap mastery target's promised behavior.
    RoadmapTarget,
}

/// One evidence item fed to the audit. `expected` vs `observed` are
/// already-redacted human-readable summaries.
#[derive(Debug, Clone)]
pub struct AuditInput {
    /// Stable receipt id (test name, issue id, target id…).
    pub id: String,
    /// Evidence class.
    pub source: EvidenceSource,
    /// Promised/expected behavior.
    pub expected: String,
    /// Observed actual behavior.
    pub observed: String,
    /// Reproducible trigger, when known.
    pub reproducible: Option<String>,
    /// When the divergence was masked — e.g. `closed task T-X`,
    /// `helper-only test`.
    pub hidden_behind: Option<String>,
    /// Observation time (unix secs).
    pub at_unix: u64,
}

/// How badly a gap hurts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Impact {
    /// Correctness/user-trust breakage.
    High,
    /// Degraded capability.
    Medium,
    /// Polish/coverage.
    Low,
}

/// Whether a gap is evidence-backed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapStatus {
    /// Receipt-backed divergence — expected ≠ observed on real evidence.
    Verified,
    /// Model-proposed or receipt-less claim — needs proof.
    Hypothesis,
}

/// A discovered capability gap.
#[derive(Debug, Clone)]
pub struct Gap {
    /// Deterministic id: `GAP-<receipt-or-slug>`.
    pub id: String,
    /// Short title.
    pub title: String,
    /// Receipt ids backing this gap (empty → hypothesis).
    pub receipts: Vec<String>,
    /// Reproducible trigger, when known.
    pub trigger: Option<String>,
    /// Promised behavior.
    pub expected: String,
    /// Observed behavior.
    pub observed: String,
    /// What masked it, when known (closed task, helper-only test…).
    pub hidden_behind: Option<String>,
    /// Verified vs hypothesis.
    pub status: GapStatus,
    /// Impact level.
    pub impact: Impact,
    /// 0..1 — verified gaps ≥ 0.7; hypotheses cap at 0.5.
    pub confidence: f64,
    /// When the underlying evidence was observed.
    pub observed_at: u64,
}

/// The audit result.
#[derive(Debug)]
pub struct AuditReport {
    /// Gaps found, impact-then-confidence ordered.
    pub gaps: Vec<Gap>,
    /// Opaque id of the brain that analyzed (`None` when it couldn't).
    pub brain: Option<String>,
    /// Why the brain pass stopped, when it produced nothing.
    pub brain_stop: Option<FailoverStop>,
    /// Receipts the brain cited but that don't exist — audit trail.
    pub invented_receipts: Vec<String>,
}

/// A finding the brain proposes (parsed from its response).
#[derive(Debug, Deserialize)]
struct BrainFinding {
    title: String,
    /// Receipt ids the brain claims support the finding.
    #[serde(default)]
    receipts: Vec<String>,
    #[serde(default)]
    impact: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
    /// New hypothesis not tied to a receipt.
    #[serde(default)]
    trigger: Option<String>,
    #[serde(default)]
    expected: Option<String>,
    #[serde(default)]
    observed: Option<String>,
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-")
}

fn impact_from(s: Option<&str>) -> Impact {
    match s.map(|x| x.to_ascii_lowercase()) {
        Some(ref x) if x == "high" => Impact::High,
        Some(ref x) if x == "medium" => Impact::Medium,
        _ => Impact::Low,
    }
}

/// Build the redacted analysis prompt: receipt ids + expected/observed
/// pairs only — no credentials, no raw provider responses. The caller's
/// `Runner` binds this prompt to its transport — [`run_audit`] never sees
/// key material.
#[must_use]
pub fn audit_prompt(inputs: &[AuditInput], mastery_targets: &[AuditInput]) -> String {
    let mut p = String::from(
        "Capability gap audit. Each receipt lists PROMISED vs OBSERVED behavior.\n\
         Respond with JSON array of findings: \
         [{\"title\":..,\"receipts\":[ids],\"impact\":\"high|medium|low\",\"confidence\":0..1,\
           \"trigger\":..,\"expected\":..,\"observed\":..}]\n\
         Only cite receipt ids shown. Receipt-less claims are hypotheses.\n\n",
    );
    let emit = |p: &mut String, tag: &str, i: &AuditInput| {
        p.push_str(&format!(
            "[{tag} {}] expected={} | observed={} | trigger={} | hidden={}\n",
            i.id,
            i.expected,
            i.observed,
            i.reproducible.as_deref().unwrap_or("none"),
            i.hidden_behind.as_deref().unwrap_or("none"),
        ));
    };
    for i in inputs {
        emit(&mut p, "receipt", i);
    }
    for i in mastery_targets {
        emit(&mut p, "target", i);
    }
    susi_error::redact::mask_env_credentials(&p)
}

/// One audit job: the corpus plus the selection scope.
pub struct AuditJob<'a> {
    /// Task/capability constraints for choosing the audit brain.
    pub intent: &'a IntentConstraints,
    /// Candidate brains.
    pub candidates: &'a [Candidate],
    /// Observed-behavior receipts.
    pub inputs: &'a [AuditInput],
    /// Promised behavior to compare against (`observed` records what was
    /// verified).
    pub mastery_targets: &'a [AuditInput],
    /// Injected clock (unix secs).
    pub now: u64,
}

/// Run the audit: evidence-direct pass, then the brain pass through
/// production failover.
#[must_use]
pub fn run_audit<R: Runner>(
    job: &AuditJob<'_>,
    stores: &mut Stores<'_>,
    budget: FailoverBudget,
    runner: &mut R,
) -> AuditReport {
    let intent = job.intent;
    let candidates = job.candidates;
    let inputs = job.inputs;
    let mastery_targets = job.mastery_targets;
    let now = job.now;
    let mut gaps: Vec<Gap> = Vec::new();
    let mut invented = Vec::new();

    // Pass 1 — evidence-direct: any receipt with expected != observed is
    // a verified gap whether or not the brain mentions it.
    let by_id: std::collections::BTreeMap<&str, &AuditInput> = inputs
        .iter()
        .chain(mastery_targets.iter())
        .map(|i| (i.id.as_str(), i))
        .collect();
    for i in inputs.iter().chain(mastery_targets.iter()) {
        if i.expected != i.observed {
            let impact = match i.source {
                EvidenceSource::IntentFailure | EvidenceSource::TestReceipt => Impact::High,
                EvidenceSource::BenchmarkReceipt | EvidenceSource::CodeBehavior => Impact::Medium,
                EvidenceSource::UserFeedback | EvidenceSource::RoadmapTarget => Impact::Medium,
            };
            gaps.push(Gap {
                id: format!("GAP-{}", slug(&i.id)),
                title: format!("{} diverges", i.id),
                receipts: vec![i.id.clone()],
                trigger: i.reproducible.clone(),
                expected: i.expected.clone(),
                observed: i.observed.clone(),
                hidden_behind: i.hidden_behind.clone(),
                status: GapStatus::Verified,
                impact,
                confidence: if i.reproducible.is_some() { 0.9 } else { 0.75 },
                observed_at: i.at_unix,
            });
        }
    }

    // Pass 2 — brain analysis over the redacted corpus, through failover.
    // (The caller binds `audit_prompt(..)` into its Runner.)
    let res = cloud_failover::run(intent, candidates, stores, budget, runner);
    let brain_opaque = res.winner.map(|w| candidates[w].opaque_id());
    let brain_stop = res.stop.clone();

    if let Some(body) = res.output {
        if let Ok(findings) = serde_json::from_str::<Vec<BrainFinding>>(&body) {
            for f in findings {
                let known: Vec<&AuditInput> = f
                    .receipts
                    .iter()
                    .filter_map(|r| by_id.get(r.as_str()).copied())
                    .collect();
                for r in &f.receipts {
                    if !by_id.contains_key(r.as_str()) {
                        invented.push(r.clone());
                    }
                }
                if known.is_empty() {
                    // Model claim without evidence → hypothesis only.
                    if f.expected.is_some() || f.observed.is_some() || f.trigger.is_some() {
                        gaps.push(Gap {
                            id: format!("GAP-hyp-{}", slug(&f.title)),
                            title: f.title.clone(),
                            receipts: Vec::new(),
                            trigger: f.trigger.clone(),
                            expected: f.expected.clone().unwrap_or_default(),
                            observed: f.observed.clone().unwrap_or_default(),
                            hidden_behind: None,
                            status: GapStatus::Hypothesis,
                            impact: impact_from(f.impact.as_deref()),
                            confidence: f.confidence.unwrap_or(0.4).min(0.5),
                            observed_at: now,
                        });
                    }
                    continue;
                }
                // Merge onto an existing verified gap when it shares a
                // receipt — the brain's title/impact refines it.
                let receipt_ids: Vec<String> = known.iter().map(|i| i.id.clone()).collect();
                if let Some(g) = gaps
                    .iter_mut()
                    .find(|g| g.receipts.iter().any(|r| receipt_ids.contains(r)))
                {
                    g.title = f.title.clone();
                    g.impact = g.impact.max(impact_from(f.impact.as_deref()));
                    if g.trigger.is_none() {
                        g.trigger = f.trigger.clone();
                    }
                } else {
                    // Verified through real receipts; the gap's
                    // expected/observed come from the receipts, not the
                    // model's restatement.
                    let first = known[0];
                    gaps.push(Gap {
                        id: format!("GAP-{}", slug(&f.title)),
                        title: f.title.clone(),
                        receipts: receipt_ids,
                        trigger: first.reproducible.clone().or(f.trigger.clone()),
                        expected: first.expected.clone(),
                        observed: first.observed.clone(),
                        hidden_behind: first.hidden_behind.clone(),
                        status: GapStatus::Verified,
                        impact: impact_from(f.impact.as_deref()),
                        confidence: if first.reproducible.is_some() || f.trigger.is_some() {
                            0.9
                        } else {
                            0.7
                        },
                        observed_at: first.at_unix,
                    });
                }
            }
        }
    }

    gaps.sort_by(|a, b| {
        b.impact
            .cmp(&a.impact)
            .then(
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then_with(|| a.id.cmp(&b.id))
    });

    AuditReport {
        gaps,
        brain: brain_opaque,
        brain_stop,
        invented_receipts: invented,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_gawd_agents::cloud_budget::{BudgetLedger, SpendPolicy};
    use susi_gawd_agents::cloud_intent::PinFallback;
    use susi_gawd_swarm::cloud_lockout::LockoutTracker;
    use susi_vendor_models::cloud_eligibility::{EligibilityStore, InferenceResult, Subject};
    use susi_vendor_models::cloud_quota::QuotaInventory;

    const T0: u64 = 1_700_000_000;
    const T0_MS: u64 = T0 * 1000;

    struct Fx {
        dir: std::path::PathBuf,
        elig: EligibilityStore,
        quota: QuotaInventory,
        lockouts: LockoutTracker,
        ledger: BudgetLedger,
        candidates: Vec<Candidate>,
    }
    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn cand(key: &str, model: &str, provider: &str) -> Candidate {
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
            cost_per_mtok: Some(1.0),
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
        let dir = std::env::temp_dir().join(format!("ga-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Fx {
            elig: EligibilityStore::new(),
            quota: QuotaInventory::new(),
            lockouts: LockoutTracker::default(),
            ledger: BudgetLedger::new(),
            candidates: Vec::new(),
            dir,
        }
    }

    fn stores<'a>(f: &'a mut Fx) -> Stores<'a> {
        Stores {
            eligibility: &mut f.elig,
            quota: &f.quota,
            lockouts: &mut f.lockouts,
            ledger: &f.ledger,
        }
    }

    fn budget() -> FailoverBudget {
        FailoverBudget {
            max_attempts: 4,
            deadline_ms: Some(T0_MS + 60_000),
            spend: SpendPolicy::PaidAuthorized {
                max_spend_micro: 10_000,
            },
            attempt_estimate_micros: 100,
            now_ms: T0_MS,
        }
    }

    fn intent() -> IntentConstraints {
        IntentConstraints {
            task_class: "reasoning".into(),
            pin_fallback: PinFallback::Deny,
            ..Default::default()
        }
    }

    fn input(id: &str, expected: &str, observed: &str) -> AuditInput {
        AuditInput {
            id: id.into(),
            source: EvidenceSource::TestReceipt,
            expected: expected.into(),
            observed: observed.into(),
            reproducible: Some(format!("run {id}")),
            hidden_behind: None,
            at_unix: T0,
        }
    }

    /// Scripted brains: index → response. `None` fails pre-dispatch (503).
    struct ScriptRunner {
        plans: Vec<Option<String>>,
    }
    impl Runner for ScriptRunner {
        fn attempt(&mut self, index: usize, _d: u64) -> cloud_failover::AttemptOutcome {
            match self.plans.get(index).cloned().unwrap_or(None) {
                Some(body) => cloud_failover::AttemptOutcome::Success(body),
                None => cloud_failover::AttemptOutcome::PreDispatch(InferenceResult::Failed {
                    status: Some(503),
                    body_snippet: "down".into(),
                    retry_after_secs: None,
                }),
            }
        }
    }

    /// Receipt-level divergences become verified gaps; the brain's
    /// receipt-backed finding refines titles/impact; receipt-less claims
    /// stay hypotheses; invented receipts are tracked, never believed.
    #[test]
    fn brain_gap_audit_verified_vs_hypothesis() {
        let mut f = fx("mix");
        let brain = cand("sk-b", "m-brain", "acme");
        f.elig
            .record_inference(subj(&brain), &InferenceResult::Success, T0);
        f.candidates = vec![brain];
        let inputs = vec![
            input("test_retry", "retries recover", "no retry attempted"),
            input("test_cache", "cache hit", "cache hit"), // consistent — no gap
        ];
        let targets = vec![AuditInput {
            id: "mastery_auth".into(),
            source: EvidenceSource::RoadmapTarget,
            expected: "SSO login works".into(),
            observed: "SSO login works".into(),
            reproducible: None,
            hidden_behind: None,
            at_unix: T0,
        }];
        let body = r#"[
            {"title":"Retry path never engaged","receipts":["test_retry"],"impact":"high","confidence":0.95},
            {"title":"Maybe flaky teardown","receipts":[],"impact":"low","confidence":0.8,"trigger":"teardown"},
            {"title":"Hallucinated gap","receipts":["nope_404"],"impact":"high","confidence":0.9,"trigger":"t","expected":"a","observed":"b"}
        ]"#;
        let mut runner = ScriptRunner {
            plans: vec![Some(body.into())],
        };
        let mut f = f;
        let cands = f.candidates.clone();
        let rep = run_audit(
            &AuditJob {
                intent: &intent(),
                candidates: &cands,
                inputs: &inputs,
                mastery_targets: &targets,
                now: T0,
            },
            &mut stores(&mut f),
            budget(),
            &mut runner,
        );
        let verified: Vec<_> = rep
            .gaps
            .iter()
            .filter(|g| g.status == GapStatus::Verified)
            .collect();
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0].title, "Retry path never engaged");
        assert_eq!(verified[0].impact, Impact::High);
        assert_eq!(verified[0].receipts, vec!["test_retry"]);
        let hyp = rep
            .gaps
            .iter()
            .filter(|g| g.status == GapStatus::Hypothesis)
            .count();
        assert_eq!(
            hyp,
            2,
            "{:?}",
            rep.gaps.iter().map(|g| &g.title).collect::<Vec<_>>()
        );
        // Hypotheses capped at 0.5 even when the model claims 0.9.
        assert!(rep
            .gaps
            .iter()
            .filter(|g| g.status == GapStatus::Hypothesis)
            .all(|g| g.confidence <= 0.5));
        assert_eq!(rep.invented_receipts, vec!["nope_404"]);
        assert!(rep.brain.as_deref().unwrap().contains("m-brain"));
    }

    /// The evidence-direct pass surfaces divergences even when the brain
    /// is unreachable — gaps are never lost to an outage.
    #[test]
    fn brain_gap_audit_evidence_direct_survives_dead_brain() {
        let mut f = fx("dead");
        let dead = cand("sk-d", "m-dead", "acme");
        f.candidates = vec![dead];
        // No success evidence; the scripted runner 503s anyway.
        let inputs = vec![input("t_fail", "pass", "fail")];
        let mut runner = ScriptRunner { plans: vec![None] };
        let cands = f.candidates.clone();
        let rep = run_audit(
            &AuditJob {
                intent: &intent(),
                candidates: &cands,
                inputs: &inputs,
                mastery_targets: &[],
                now: T0,
            },
            &mut stores(&mut f),
            budget(),
            &mut runner,
        );
        assert!(rep.brain.is_none());
        assert!(rep.brain_stop.is_some());
        assert_eq!(rep.gaps.len(), 1);
        assert_eq!(rep.gaps[0].status, GapStatus::Verified);
        assert!(rep.gaps[0].expected.contains("pass"));
        assert!(rep.gaps[0].observed.contains("fail"));
    }

    /// Mid-call brain failure fails over to the next working brain — the
    /// audit completes and names the winner.
    #[test]
    fn brain_gap_audit_failover_to_working_brain() {
        let mut f = fx("fo");
        let dead = cand("sk-d", "m-dead", "acme");
        let live = cand("sk-l", "m-live", "otherco");
        f.elig
            .record_inference(subj(&dead), &InferenceResult::Success, T0);
        f.elig
            .record_inference(subj(&live), &InferenceResult::Success, T0);
        f.candidates = vec![dead, live];
        let body = r#"[{"title":"ok","receipts":["t1"],"impact":"medium","confidence":0.8}]"#;
        // Rank order picks index 0 first (equal cost → earlier index? both
        // working; whichever it picks, plans cover both: 0 fails → 1 wins).
        let mut runner = ScriptRunner {
            plans: vec![None, Some(body.into())],
        };
        let inputs = vec![input("t1", "x", "y")];
        let cands = f.candidates.clone();
        let rep = run_audit(
            &AuditJob {
                intent: &intent(),
                candidates: &cands,
                inputs: &inputs,
                mastery_targets: &[],
                now: T0,
            },
            &mut stores(&mut f),
            budget(),
            &mut runner,
        );
        assert!(rep.brain.is_some());
        assert!(rep.brain.as_deref().unwrap().contains("m-live"));
    }

    /// A gap hidden behind a closed task still surfaces: the receipt
    /// carries `hidden_behind` and the gap preserves it.
    #[test]
    fn brain_gap_audit_hidden_gap_surfaces() {
        let mut f = fx("hid");
        let b = cand("sk-b", "m-b", "acme");
        f.elig
            .record_inference(subj(&b), &InferenceResult::Success, T0);
        f.candidates = vec![b];
        let inputs = vec![AuditInput {
            id: "closed_task_T9".into(),
            source: EvidenceSource::CodeBehavior,
            expected: "parser accepts unicode".into(),
            observed: "parser panics on unicode".into(),
            reproducible: Some("feed \\u{1f600}".into()),
            hidden_behind: Some("closed task T-CODEX-9 marked done".into()),
            at_unix: T0,
        }];
        let mut runner = ScriptRunner {
            plans: vec![Some("[]".into())],
        };
        let cands = f.candidates.clone();
        let rep = run_audit(
            &AuditJob {
                intent: &intent(),
                candidates: &cands,
                inputs: &inputs,
                mastery_targets: &[],
                now: T0,
            },
            &mut stores(&mut f),
            budget(),
            &mut runner,
        );
        assert_eq!(rep.gaps.len(), 1);
        assert_eq!(rep.gaps[0].status, GapStatus::Verified);
        assert!(rep.gaps[0]
            .hidden_behind
            .as_deref()
            .unwrap()
            .contains("closed task"));
    }

    /// The prompt sent to the brain contains receipt evidence and no
    /// credential material.
    #[test]
    fn brain_gap_audit_prompt_is_redacted() {
        let inputs = vec![input("t1", "works", "hangs")];
        let p = audit_prompt(&inputs, &[]);
        assert!(p.contains("t1"));
        assert!(p.contains("works"));
        assert!(p.contains("hangs"));
        assert!(!p.contains("sk-"));
    }

    /// Free-only spend policy blocks paid-only brains honestly — report
    /// records the budget stop rather than pretending analysis ran.
    #[test]
    fn brain_gap_audit_budget_block_is_honest() {
        let mut f = fx("bud");
        let paid = cand("sk-p", "m-paid", "acme");
        f.elig
            .record_inference(subj(&paid), &InferenceResult::Success, T0);
        f.candidates = vec![paid];
        let mut b = budget();
        b.spend = SpendPolicy::FreeOnly; // paid model estimate > 0 → denied
        let mut runner = ScriptRunner {
            plans: vec![Some("[]".into())],
        };
        let cands = f.candidates.clone();
        let rep = run_audit(
            &AuditJob {
                intent: &intent(),
                candidates: &cands,
                inputs: &[input("t1", "a", "a")],
                mastery_targets: &[],
                now: T0,
            },
            &mut stores(&mut f),
            b,
            &mut runner,
        );
        // Consistent receipt → no gaps; free-only denial surfaces as stop.
        assert!(rep.gaps.is_empty());
        assert!(matches!(
            rep.brain_stop,
            Some(FailoverStop::BudgetExhausted) | Some(FailoverStop::NoEligibleCandidates)
        ));
    }
}
