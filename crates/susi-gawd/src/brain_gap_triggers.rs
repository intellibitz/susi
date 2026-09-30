//! Event-driven, bounded gap discovery (T-CODEX-32 / VC-201-016).
//!
//! Production events — failed user intents, recurring tool/model
//! failures, CI regressions, task closures (mastery re-checks) and
//! explicit audit requests — enqueue audit units on a persisted queue.
//! `flush` drains it through the production path: `run_audit` over the
//! current strongest working brain → `propose` (dedup) → `publish`.
//!
//! Bounds that keep discovery from amplifying incidents:
//! - **Debounce**: one unit per issue key per `debounce_secs` window.
//! - **Queue cap**: `max_pending` — beyond it, events merge into an
//!   existing unit or are refused honestly.
//! - **Spend/time**: the caller bounds the flush via `FailoverBudget`;
//!   `FreeOnly`/exhaustion leaves units pending, never fakes results.
//! - **Transient separation**: timeouts/5xx/network blips are classified
//!   operational, never minted into software-gap work.
//! - **Resume after outage**: when no brain is working, units stay
//!   queued; a later flush after recovery drains them without
//!   duplicating proposals (the dedup store already holds the issue).

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use susi_gawd_swarm::cloud_failover::{FailoverBudget, Runner, Stores};

use crate::brain_gap_audit::{run_audit, AuditInput, AuditJob, EvidenceSource};
use crate::brain_gap_dedup::{propose, ExistingTask, ProposalStore};
use crate::brain_gap_tasks::{publish, validate_draft, DraftCtx};
use susi_gawd_agents::cloud_intent::{Candidate, IntentConstraints};

/// A production event that may reveal a capability gap.
#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    /// A user intent failed end to end.
    IntentFailed {
        /// Intent/session id.
        id: String,
        /// Redacted detail.
        detail: String,
    },
    /// A tool/model call failed (recurring failures become gaps).
    ToolFailure {
        /// Tool or model name.
        tool: String,
        /// Failure class (`timeout`, `http_5xx`, `parse`, `crash`, …).
        kind: String,
    },
    /// An acceptance/CI check regressed.
    CiRegression {
        /// Check/test name.
        check: String,
    },
    /// A task closed — re-evaluate the mastery targets it advanced.
    TaskClosed {
        /// The closed task.
        task_id: String,
        /// Receipts to re-check (mastery targets / acceptance still weak).
        residual: Vec<AuditInput>,
    },
    /// Operator/agent explicitly requested an audit.
    ExplicitAudit {
        /// Who asked.
        by: String,
    },
}

/// Classification: operational noise vs actionable software gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventClass {
    /// Transient operational condition — never mints gap work.
    Transient,
    /// Actionable signal — may enqueue an audit unit.
    Actionable,
}

/// True when a failure kind is a transient operational condition.
#[must_use]
pub fn is_transient_kind(kind: &str) -> bool {
    matches!(
        kind,
        "timeout" | "http_5xx" | "http_429" | "network" | "connection" | "unavailable"
    )
}

/// Classify an event — recurring tool failures and regressions are
/// actionable; single transient blips are not.
#[must_use]
pub fn classify(ev: &DiscoveryEvent) -> EventClass {
    match ev {
        DiscoveryEvent::ToolFailure { kind, .. } if is_transient_kind(kind) => {
            EventClass::Transient
        }
        DiscoveryEvent::IntentFailed { detail, .. } if is_transient_kind(detail.as_str()) => {
            EventClass::Transient
        }
        DiscoveryEvent::ToolFailure { .. }
        | DiscoveryEvent::IntentFailed { .. }
        | DiscoveryEvent::CiRegression { .. }
        | DiscoveryEvent::TaskClosed { .. }
        | DiscoveryEvent::ExplicitAudit { .. } => EventClass::Actionable,
    }
}

/// Bounds on the discovery engine.
#[derive(Debug, Clone)]
pub struct TriggerPolicy {
    /// Seconds a repeated same-key event is debounced.
    pub debounce_secs: u64,
    /// Max pending audit units.
    pub max_pending: usize,
    /// Max events folded into one unit's evidence.
    pub max_receipts_per_unit: usize,
}

impl Default for TriggerPolicy {
    fn default() -> Self {
        Self {
            debounce_secs: 300,
            max_pending: 32,
            max_receipts_per_unit: 16,
        }
    }
}

/// One queued audit unit — an evidence bundle awaiting brain analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditUnit {
    /// Issue key used for debouncing/dedup.
    pub key: String,
    /// Evidence receipts.
    pub inputs: Vec<AuditInputSerde>,
    /// Event classes that produced it.
    pub sources: Vec<String>,
    /// First seen.
    pub first_unix: u64,
    /// Last folded-in event.
    pub last_unix: u64,
}

/// Serde-friendly mirror of `AuditInput` (it isn't serializable upstream).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditInputSerde {
    /// Receipt id.
    pub id: String,
    /// Evidence class tag.
    pub source: String,
    /// Promised behavior.
    pub expected: String,
    /// Observed behavior.
    pub observed: String,
    /// Repro trigger when known.
    pub reproducible: Option<String>,
    /// Masking record when known.
    pub hidden_behind: Option<String>,
    /// Observation time.
    pub at_unix: u64,
}

impl AuditInputSerde {
    /// Convert to the in-memory audit input.
    #[must_use]
    pub fn to_input(&self) -> AuditInput {
        AuditInput {
            id: self.id.clone(),
            source: match self.source.as_str() {
                "intent" => EvidenceSource::IntentFailure,
                "benchmark" => EvidenceSource::BenchmarkReceipt,
                "code" => EvidenceSource::CodeBehavior,
                "feedback" => EvidenceSource::UserFeedback,
                "target" => EvidenceSource::RoadmapTarget,
                _ => EvidenceSource::TestReceipt,
            },
            expected: self.expected.clone(),
            observed: self.observed.clone(),
            reproducible: self.reproducible.clone(),
            hidden_behind: self.hidden_behind.clone(),
            at_unix: self.at_unix,
        }
    }
}

/// What an ingested event did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerDecision {
    /// A new audit unit queued (index).
    Queued(usize),
    /// Folded into an existing pending unit.
    Folded(usize),
    /// Inside the debounce window — absorbed.
    Debounced {
        /// Issue key.
        key: String,
    },
    /// Transient operational signal — recorded but never queued.
    Transient,
    /// Queue limit reached — honestly refused.
    Bounded,
}

/// A pending-unit record plus the debounce ledger, persisted under `dir`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct QueueFile {
    /// Pending audit units.
    units: Vec<AuditUnit>,
    /// Issue key → last event time (debounce).
    last_seen: BTreeMap<String, u64>,
}

/// The discovery engine.
pub struct TriggerEngine {
    dir: PathBuf,
    q: QueueFile,
    /// Bounds.
    pub policy: TriggerPolicy,
}

fn ev_key(ev: &DiscoveryEvent) -> String {
    match ev {
        DiscoveryEvent::IntentFailed { id, .. } => format!("intent:{id}"),
        DiscoveryEvent::ToolFailure { tool, kind } => format!("tool:{tool}:{kind}"),
        DiscoveryEvent::CiRegression { check } => format!("ci:{check}"),
        DiscoveryEvent::TaskClosed { task_id, .. } => format!("closed:{task_id}"),
        DiscoveryEvent::ExplicitAudit { by } => format!("audit:{by}"),
    }
}

impl TriggerEngine {
    /// Open (or create) the engine state at `dir` — restart-safe.
    #[must_use]
    pub fn load(dir: PathBuf, policy: TriggerPolicy) -> Self {
        let q = fs::read_to_string(dir.join("queue.json"))
            .ok()
            .and_then(|b| serde_json::from_str(&b).ok())
            .unwrap_or_default();
        Self { dir, q, policy }
    }

    fn persist(&self) {
        let _ = fs::create_dir_all(&self.dir);
        if let Ok(body) = serde_json::to_string_pretty(&self.q) {
            let tmp = self.dir.join(".queue.json.tmp");
            let _ = fs::write(&tmp, body);
            let _ = fs::rename(&tmp, self.dir.join("queue.json"));
        }
    }

    /// Pending audit units.
    #[must_use]
    pub fn pending(&self) -> &[AuditUnit] {
        &self.q.units
    }

    /// Drain all pending units — the caller owns them and must `requeue`
    /// any it cannot process (brain outage, budget bound). Persisted.
    pub fn take_pending(&mut self) -> Vec<AuditUnit> {
        let units = std::mem::take(&mut self.q.units);
        self.persist();
        units
    }

    /// Re-queue a unit that could not be processed. Bounded by
    /// `policy.max_pending`; overflow drops with the audit trail intact
    /// (`last_seen` still debounces re-ingestion).
    pub fn requeue(&mut self, unit: AuditUnit) {
        if self.q.units.len() < self.policy.max_pending {
            self.q.units.push(unit);
            self.persist();
        }
    }

    /// Ingest one event: classify → debounce → fold/queue → bound.
    pub fn on_event(
        &mut self,
        ev: DiscoveryEvent,
        receipts: Vec<AuditInputSerde>,
        now: u64,
    ) -> TriggerDecision {
        let key = ev_key(&ev);
        if classify(&ev) == EventClass::Transient {
            self.q.last_seen.insert(key, now);
            self.persist();
            return TriggerDecision::Transient;
        }
        if let Some(last) = self.q.last_seen.get(&key) {
            if now.saturating_sub(*last) < self.policy.debounce_secs {
                return TriggerDecision::Debounced { key };
            }
        }
        self.q.last_seen.insert(key.clone(), now);
        // Fold into an existing pending unit for the same key.
        if let Some(i) = self.q.units.iter().position(|u| u.key == key) {
            let u = &mut self.q.units[i];
            for r in receipts {
                if u.inputs.len() < self.policy.max_receipts_per_unit {
                    u.inputs.push(r);
                }
            }
            u.last_unix = now;
            self.persist();
            return TriggerDecision::Folded(i);
        }
        if self.q.units.len() >= self.policy.max_pending {
            return TriggerDecision::Bounded;
        }
        self.q.units.push(AuditUnit {
            key: key.clone(),
            inputs: receipts,
            sources: vec![key.split(':').next().unwrap_or("?").to_string()],
            first_unix: now,
            last_unix: now,
        });
        self.persist();
        TriggerDecision::Queued(self.q.units.len() - 1)
    }
}

/// Flush outcome for one unit.
#[derive(Debug)]
pub enum FlushItem {
    /// Brain audited it; `n` task drafts published.
    Published(usize),
    /// Audit ran; dedup merged/linked everything (no new task).
    Absorbed,
    /// No working brain — unit stays queued for a later flush.
    BrainUnavailable,
    /// Flush spend/time budget exhausted — rest stay queued.
    BudgetBound,
}

/// The selection scope for a flush — what the audit may use.
pub struct AuditScope<'a> {
    /// Task/capability constraints.
    pub intent: &'a IntentConstraints,
    /// Candidate brains.
    pub candidates: &'a [Candidate],
    /// Clock (unix secs).
    pub now: u64,
}

/// The live brain call resources (stores, bounds, executor).
pub struct BrainRun<'a, R: Runner> {
    /// Shared evidence/budget stores.
    pub stores: &'a mut Stores<'a>,
    /// Attempt/deadline/spend bounds for this flush.
    pub budget: FailoverBudget,
    /// Executor.
    pub runner: &'a mut R,
}

/// The task-pipeline resources: dedup store, validation sets, mint ctx.
pub struct PipeCtx<'a> {
    /// Dedup proposal store.
    pub proposals: &'a ProposalStore,
    /// Existing task summaries for dedup.
    pub existing: &'a [ExistingTask],
    /// `.agents/tasks` directory.
    pub tasks_dir: &'a PathBuf,
    /// Known roadmap vector ids.
    pub vectors: &'a std::collections::BTreeSet<String>,
    /// Task id → deps graph.
    pub task_graph: &'a std::collections::BTreeMap<String, Vec<String>>,
    /// All known task ids.
    pub known_ids: &'a std::collections::BTreeSet<String>,
    /// Minting context (agent, roadmap, accept cmd).
    pub draft: &'a DraftCtx<'a>,
    /// Next task sequence number.
    pub next_seq: &'a mut u32,
}

/// Drain pending units through audit → dedup → publish. Returns one item
/// per processed unit; unprocessed units remain queued (bounded retry).
#[must_use]
pub fn flush<R: Runner>(
    engine: &mut TriggerEngine,
    scope: &AuditScope<'_>,
    brain: &mut BrainRun<'_, R>,
    pipe: &mut PipeCtx<'_>,
) -> Vec<FlushItem> {
    let mut out = Vec::new();
    let mut drained = Vec::new();
    for (i, unit) in engine.q.units.iter().enumerate() {
        let inputs: Vec<AuditInput> = unit.inputs.iter().map(AuditInputSerde::to_input).collect();
        let rep = run_audit(
            &AuditJob {
                intent: scope.intent,
                candidates: scope.candidates,
                inputs: &inputs,
                mastery_targets: &[],
                now: scope.now,
            },
            brain.stores,
            brain.budget,
            brain.runner,
        );
        if rep.brain.is_none() {
            // No working brain (or budget bound) — keep everything queued.
            out.push(match rep.brain_stop {
                Some(susi_gawd_swarm::cloud_failover::FailoverStop::BudgetExhausted) => {
                    FlushItem::BudgetBound
                }
                _ => FlushItem::BrainUnavailable,
            });
            continue;
        }
        let mut published = 0;
        for g in &rep.gaps {
            let c = DraftCtx {
                agent: pipe.draft.agent,
                roadmap: pipe.draft.roadmap,
                accept_cmd: pipe.draft.accept_cmd.clone(),
                deps: Vec::new(),
                now: pipe.draft.now,
            };
            let outcome = propose(pipe.proposals, g, pipe.existing, &c, *pipe.next_seq);
            let draft = match outcome {
                Ok(crate::brain_gap_dedup::ProposeOutcome::Created(d)) => Some(*d),
                Ok(crate::brain_gap_dedup::ProposeOutcome::LinkedFollowUp { draft: d, .. }) => {
                    Some(*d)
                }
                _ => None,
            };
            let Some(d) = draft else { continue };
            let ok = validate_draft(
                &d,
                pipe.known_ids,
                pipe.task_graph,
                pipe.vectors,
                &std::collections::BTreeSet::new(),
            )
            .is_ok()
                && publish(pipe.tasks_dir, &d).is_ok();
            if ok {
                *pipe.next_seq += 1;
                published += 1;
            }
        }
        drained.push(i);
        out.push(if published > 0 {
            FlushItem::Published(published)
        } else {
            FlushItem::Absorbed
        });
    }
    // Remove drained units (reverse order keeps indices valid).
    for i in drained.into_iter().rev() {
        engine.q.units.remove(i);
    }
    engine.persist();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_gawd_agents::cloud_budget::{BudgetLedger, SpendPolicy};
    use susi_gawd_agents::cloud_intent::PinFallback;
    use susi_gawd_swarm::cloud_failover::AttemptOutcome;
    use susi_gawd_swarm::cloud_lockout::LockoutTracker;
    use susi_vendor_models::cloud_eligibility::{EligibilityStore, InferenceResult, Subject};
    use susi_vendor_models::cloud_quota::QuotaInventory;

    const T0: u64 = 4_000_000_000;

    // Per-test-thread clock — parallel tests never race.
    thread_local! {
        static TEST_NOW: std::cell::Cell<u64> = const { std::cell::Cell::new(1_700_000_000) };
    }
    fn test_clock() -> u64 {
        TEST_NOW.get()
    }
    fn set_now(secs: u64) {
        TEST_NOW.with(|t| t.set(secs));
    }

    fn input(id: &str, expected: &str, observed: &str) -> AuditInputSerde {
        AuditInputSerde {
            id: id.into(),
            source: "test".into(),
            expected: expected.into(),
            observed: observed.into(),
            reproducible: Some("repro".into()),
            hidden_behind: None,
            at_unix: T0,
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
            est_latency_ms: 400,
            cost_per_mtok: Some(1.0),
            quality: Default::default(),
        }
    }

    struct Fx {
        dir: PathBuf,
        elig: EligibilityStore,
        quota: QuotaInventory,
        lockouts: LockoutTracker,
        ledger: BudgetLedger,
        proposals: ProposalStore,
        tasks_dir: PathBuf,
        vectors: std::collections::BTreeSet<String>,
    }
    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn fx(tag: &str) -> Fx {
        let dir = std::env::temp_dir().join(format!("tr-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Fx {
            proposals: ProposalStore::load(dir.join("prop")),
            tasks_dir: dir.join("tasks"),
            vectors: ["VC-201-016".to_string()].into_iter().collect(),
            elig: EligibilityStore::new(),
            quota: QuotaInventory::new(),
            lockouts: LockoutTracker::new(Default::default(), test_clock),
            ledger: BudgetLedger::new(),
            dir,
        }
    }

    struct ScriptRunner {
        plan: Option<String>,
    }
    impl Runner for ScriptRunner {
        fn attempt(&mut self, _i: usize, _d: u64) -> AttemptOutcome {
            match self.plan.clone() {
                Some(b) => AttemptOutcome::Success(b),
                None => AttemptOutcome::PreDispatch(InferenceResult::Failed {
                    status: Some(503),
                    body_snippet: "down".into(),
                    retry_after_secs: None,
                }),
            }
        }
    }

    fn intent() -> IntentConstraints {
        IntentConstraints {
            task_class: "reasoning".into(),
            pin_fallback: PinFallback::Deny,
            ..Default::default()
        }
    }

    fn budget(now_secs: u64) -> FailoverBudget {
        FailoverBudget {
            max_attempts: 3,
            deadline_ms: Some(now_secs * 1000 + 60_000),
            spend: SpendPolicy::PaidAuthorized {
                max_spend_micro: 10_000,
            },
            attempt_estimate_micros: 100,
            now_ms: now_secs * 1000,
        }
    }

    fn ctx<'a>() -> DraftCtx<'a> {
        DraftCtx {
            agent: "DEVIN",
            roadmap: "VC-201-016",
            accept_cmd: vec![
                "cargo".into(),
                "test".into(),
                "-p".into(),
                "susi-gawd".into(),
                "gap_fix".into(),
                "--locked".into(),
            ],
            deps: vec![],
            now: T0,
        }
    }

    static EMPTY_GRAPH: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();

    /// Per-flush run parameters bundled for the test helper.
    struct RunArgs<'a, R: Runner> {
        known: &'a std::collections::BTreeSet<String>,
        budget: FailoverBudget,
        runner: &'a mut R,
        seq: &'a mut u32,
        now: u64,
    }

    /// Wrap the verbose production `flush` into one call for tests.
    fn do_flush<R: Runner>(
        e: &mut TriggerEngine,
        f: &mut Fx,
        candidates: &[Candidate],
        args: RunArgs<'_, R>,
    ) -> Vec<FlushItem> {
        let RunArgs {
            known,
            budget,
            runner,
            seq,
            now,
        } = args;
        set_now(now);
        let i = intent();
        let c = ctx();
        flush(
            e,
            &AuditScope {
                intent: &i,
                candidates,
                now,
            },
            &mut BrainRun {
                stores: &mut Stores {
                    eligibility: &mut f.elig,
                    quota: &f.quota,
                    lockouts: &mut f.lockouts,
                    ledger: &f.ledger,
                },
                budget,
                runner,
            },
            &mut PipeCtx {
                proposals: &f.proposals,
                existing: &[],
                tasks_dir: &f.tasks_dir,
                vectors: &f.vectors,
                task_graph: &EMPTY_GRAPH,
                known_ids: known,
                draft: &c,
                next_seq: seq,
            },
        )
    }

    /// A burst of identical failures debounces into one audit unit;
    /// different keys queue separately.
    #[test]
    fn brain_gap_triggers_burst_debounced() {
        let mut e = TriggerEngine::load(
            std::env::temp_dir().join(format!("tr-b-{}", std::process::id())),
            TriggerPolicy::default(),
        );
        let ev = || DiscoveryEvent::ToolFailure {
            tool: "search".into(),
            kind: "parse".into(),
        };
        assert!(matches!(
            e.on_event(ev(), vec![input("r1", "a", "b")], T0),
            TriggerDecision::Queued(0)
        ));
        // Same key inside the window → debounced.
        assert!(matches!(
            e.on_event(ev(), vec![], T0 + 60),
            TriggerDecision::Debounced { .. }
        ));
        // After the window → folds into the same unit, not a second one.
        assert!(matches!(
            e.on_event(ev(), vec![input("r2", "a", "b")], T0 + 600),
            TriggerDecision::Folded(0)
        ));
        assert_eq!(e.pending().len(), 1);
        assert_eq!(e.pending()[0].inputs.len(), 2);
    }

    /// Timeouts/5xx/network blips are operational — never queued as
    /// software gaps.
    #[test]
    fn brain_gap_triggers_transients_never_queue() {
        let mut e = TriggerEngine::load(
            std::env::temp_dir().join(format!("tr-t-{}", std::process::id())),
            TriggerPolicy::default(),
        );
        for kind in ["timeout", "http_5xx", "http_429", "network"] {
            let d = e.on_event(
                DiscoveryEvent::ToolFailure {
                    tool: "m".into(),
                    kind: kind.into(),
                },
                vec![input("r", "a", "b")],
                T0,
            );
            assert_eq!(d, TriggerDecision::Transient, "{kind}");
        }
        assert!(e.pending().is_empty());
        // A non-transient failure does queue.
        let d = e.on_event(
            DiscoveryEvent::ToolFailure {
                tool: "m".into(),
                kind: "parse".into(),
            },
            vec![input("r", "a", "b")],
            T0,
        );
        assert!(matches!(d, TriggerDecision::Queued(0)));
    }

    /// Queue survives restart — pending units and debounce state persist.
    #[test]
    fn brain_gap_triggers_restart_resumes_pending() {
        let dir = std::env::temp_dir().join(format!("tr-r-{}", std::process::id()));
        {
            let mut e = TriggerEngine::load(dir.clone(), TriggerPolicy::default());
            e.on_event(
                DiscoveryEvent::CiRegression {
                    check: "test_x".into(),
                },
                vec![input("r1", "green", "red")],
                T0,
            );
        }
        let mut e2 = TriggerEngine::load(dir.clone(), TriggerPolicy::default());
        assert_eq!(e2.pending().len(), 1);
        // Debounce state persisted too.
        assert!(matches!(
            e2.on_event(
                DiscoveryEvent::CiRegression {
                    check: "test_x".into()
                },
                vec![],
                T0 + 60
            ),
            TriggerDecision::Debounced { .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A closed task whose mastery target still fails re-checks through
    /// the brain and mints a real task — the residual gap is not lost.
    #[test]
    fn brain_gap_triggers_closed_task_residual_publishes() {
        let mut f = fx("resid");
        let brain = cand("sk-b", "m-brain");
        f.elig.record_inference(
            Subject {
                provider: "acme",
                api_key: "sk-b",
                account: None,
                region: None,
                model: "m-brain",
            },
            &InferenceResult::Success,
            T0,
        );
        let mut e = TriggerEngine::load(f.dir.join("eng"), TriggerPolicy::default());
        e.on_event(
            DiscoveryEvent::TaskClosed {
                task_id: "T-CODEX-9".into(),
                residual: Vec::new(),
            },
            vec![input("target_auth", "SSO works", "SSO 500s")],
            T0,
        );
        let candidates = vec![brain];
        let mut runner = ScriptRunner {
            plan: Some("[]".into()),
        };
        let mut seq = 100u32;
        let known: std::collections::BTreeSet<String> = ["T-CODEX-9".into()].into_iter().collect();
        let items = do_flush(
            &mut e,
            &mut f,
            &candidates,
            RunArgs {
                known: &known,
                budget: budget(T0),
                runner: &mut runner,
                seq: &mut seq,
                now: T0,
            },
        );
        // Evidence-direct gap → dedup creates → validated → published.
        assert!(matches!(items[0], FlushItem::Published(1)), "{items:?}");
        assert!(e.pending().is_empty());
        assert!(f.tasks_dir.join("T-DEVIN-100.json").exists());
        let task: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(f.tasks_dir.join("T-DEVIN-100.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(task["roadmap"], "VC-201-016");
        assert!(task["goal"].as_str().unwrap().contains("target_auth"));
    }

    /// With no working brain the flush keeps units queued; a recovered
    /// brain drains them once — dedup prevents duplicate tasks.
    #[test]
    fn brain_gap_triggers_unavailable_brain_resumes_after_recovery() {
        let mut f = fx("resume");
        let brain = cand("sk-b", "m-brain");
        let candidates = vec![brain];
        let mut e = TriggerEngine::load(f.dir.join("eng"), TriggerPolicy::default());
        e.on_event(
            DiscoveryEvent::CiRegression { check: "c".into() },
            vec![input("r1", "a", "b")],
            T0,
        );
        let mut seq = 200u32;
        // Brain dead (no usable evidence + runner fails).
        let mut dead = ScriptRunner { plan: None };
        let items = do_flush(
            &mut e,
            &mut f,
            &candidates,
            RunArgs {
                known: &std::collections::BTreeSet::new(),
                budget: budget(T0),
                runner: &mut dead,
                seq: &mut seq,
                now: T0,
            },
        );
        assert!(matches!(items[0], FlushItem::BrainUnavailable));
        assert_eq!(e.pending().len(), 1);
        // Recovery: usable evidence lands; flush again → publishes once.
        f.elig.record_inference(
            Subject {
                provider: "acme",
                api_key: "sk-b",
                account: None,
                region: None,
                model: "m-brain",
            },
            &InferenceResult::Success,
            T0 + 10,
        );
        let mut live = ScriptRunner {
            plan: Some("[]".into()),
        };
        let items2 = do_flush(
            &mut e,
            &mut f,
            &candidates,
            RunArgs {
                known: &std::collections::BTreeSet::new(),
                budget: budget(T0 + 20),
                runner: &mut live,
                seq: &mut seq,
                now: T0 + 20,
            },
        );
        assert!(matches!(items2[0], FlushItem::Published(1)));
        assert!(e.pending().is_empty());
        // Re-queue the same issue → proposals already hold it → absorbed.
        e.on_event(
            DiscoveryEvent::CiRegression { check: "c".into() },
            vec![input("r1", "a", "b")],
            T0 + 700,
        );
        let mut seq2 = 300u32;
        let items3 = do_flush(
            &mut e,
            &mut f,
            &candidates,
            RunArgs {
                known: &std::collections::BTreeSet::new(),
                budget: budget(T0 + 800),
                runner: &mut live,
                seq: &mut seq2,
                now: T0 + 800,
            },
        );
        assert!(matches!(items3[0], FlushItem::Absorbed));
        assert_eq!(
            std::fs::read_dir(&f.tasks_dir).unwrap().count(),
            1,
            "no duplicate task minted"
        );
    }

    /// A free-only policy that can't afford the brain leaves the queue
    /// pending — honestly bounded, never a fabricated audit.
    #[test]
    fn brain_gap_triggers_budget_bound_holds_queue() {
        let mut f = fx("bud");
        let paid = cand("sk-p", "m-paid");
        f.elig.record_inference(
            Subject {
                provider: "acme",
                api_key: "sk-p",
                account: None,
                region: None,
                model: "m-paid",
            },
            &InferenceResult::Success,
            T0,
        );
        let candidates = vec![paid];
        let mut e = TriggerEngine::load(f.dir.join("eng"), TriggerPolicy::default());
        e.on_event(
            DiscoveryEvent::ExplicitAudit { by: "op".into() },
            vec![input("r1", "a", "b")],
            T0,
        );
        let mut b = budget(T0);
        b.spend = SpendPolicy::FreeOnly;
        let mut runner = ScriptRunner {
            plan: Some("[]".into()),
        };
        let mut seq = 400u32;
        let items = do_flush(
            &mut e,
            &mut f,
            &candidates,
            RunArgs {
                known: &std::collections::BTreeSet::new(),
                budget: b,
                runner: &mut runner,
                seq: &mut seq,
                now: T0,
            },
        );
        assert!(!items.is_empty());
        assert_eq!(e.pending().len(), 1, "unit stays queued");
        assert!(
            std::fs::read_dir(&f.tasks_dir)
                .map(|r| r.count())
                .unwrap_or(0)
                == 0
        );
    }

    /// Pending queue is bounded — over-cap events are refused honestly.
    #[test]
    fn brain_gap_triggers_queue_bounded() {
        let mut e = TriggerEngine::load(
            std::env::temp_dir().join(format!("tr-cap-{}", std::process::id())),
            TriggerPolicy {
                max_pending: 2,
                ..Default::default()
            },
        );
        for i in 0..3 {
            let d = e.on_event(
                DiscoveryEvent::CiRegression {
                    check: format!("check{i}"),
                },
                vec![],
                T0,
            );
            if i < 2 {
                assert!(matches!(d, TriggerDecision::Queued(_)));
            } else {
                assert_eq!(d, TriggerDecision::Bounded);
            }
        }
        assert_eq!(e.pending().len(), 2);
    }
}
