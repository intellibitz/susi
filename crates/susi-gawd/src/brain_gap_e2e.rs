//! End-to-end capability-gap discovery -> executable roadmap work
//! (T-CODEX-34 / VC-201-020).
//!
//! Composes the production pipeline over a real repository directory:
//!
//! ```text
//! TriggerEngine::on_event          (audit/event ingress, debounced)
//!   -> take_pending / run_audit    (strongest working brain, failover)
//!   -> propose                     (semantic dedup vs open+closed work)
//!   -> record_draft + transition   (gap->evidence->task lineage log)
//!   -> publish                     (atomic one-file-per-task record)
//! ```
//!
//! What lands is an actual queue record whose acceptance argv is a real
//! nonzero `cargo test` invocation, linked to a valid roadmap vector with
//! resolved dependencies — a task that fails before the repair and passes
//! after it. Discovery never claims capability improved because a record
//! exists: improvement is only the acceptance command's before/after.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use susi_error::redact::{mask_env_credentials, redact_patterns};
use susi_gawd_swarm::cloud_failover::{FailoverStop, Runner};

use crate::brain_gap_audit::{run_audit, AuditInput, AuditJob};
use crate::brain_gap_dedup::{
    propose, rank_gap, ExistingTask, ProposalStore, ProposeOutcome, TaskState,
};
use crate::brain_gap_publish::{
    record_draft, transition, PubMeta, PublicationLog, PublishState, QueueKnowledge,
};
use crate::brain_gap_tasks::{publish, DraftCtx, TaskDraft};
use crate::brain_gap_triggers::{AuditScope, AuditUnit, BrainRun, TriggerEngine};

// ------------------------------------------------------------ ingress helper

/// Redact raw receipt text into an audit-ready input — secrets and env
/// credentials are stripped before they can reach a brain prompt or a
/// published task record.
#[must_use]
pub fn redacted_input(
    input: &crate::brain_gap_triggers::AuditInputSerde,
    secrets: &[String],
) -> crate::brain_gap_triggers::AuditInputSerde {
    let clean = |s: &str| redact_patterns(secrets, &mask_env_credentials(s));
    let clean_opt = |o: &Option<String>| o.as_deref().map(clean);
    crate::brain_gap_triggers::AuditInputSerde {
        id: input.id.clone(),
        source: input.source.clone(),
        expected: clean(&input.expected),
        observed: clean(&input.observed),
        reproducible: clean_opt(&input.reproducible),
        hidden_behind: clean_opt(&input.hidden_behind),
        at_unix: input.at_unix,
    }
}

// ---------------------------------------------------------------- queue view

/// A repository's task-queue view for dedup/validation — loaded from real
/// `.agents/tasks/*.json` records (open) plus `done/` (closed history).
pub struct DiscoveryQueue {
    /// `.agents/tasks` directory — where drafts are published.
    pub tasks_dir: PathBuf,
    /// Persisted per-issue proposal claims (concurrent-safe).
    pub proposals: ProposalStore,
    /// All known tasks (open + closed) as dedup text.
    pub existing: Vec<ExistingTask>,
    /// All known task ids.
    pub known_ids: BTreeSet<String>,
    /// Task id -> dependency ids.
    pub task_graph: BTreeMap<String, Vec<String>>,
    /// Known roadmap vector ids.
    pub vectors: BTreeSet<String>,
    /// Next task sequence number for this agent namespace.
    pub next_seq: u32,
}

impl DiscoveryQueue {
    /// Load the queue view under `repo` (the temporary repository root).
    /// `tasks_dir` is `repo/.agents/tasks`; `closed_dir` (e.g. `done/`) is
    /// scanned for closed-task history when it exists.
    #[must_use]
    pub fn load(repo: &Path, vectors: BTreeSet<String>, next_seq: u32) -> Self {
        let tasks_dir = repo.join(".agents/tasks");
        let mut q = Self {
            proposals: ProposalStore::load(repo.join(".agents/proposals")),
            tasks_dir: tasks_dir.clone(),
            existing: Vec::new(),
            known_ids: BTreeSet::new(),
            task_graph: BTreeMap::new(),
            vectors,
            next_seq,
        };
        for (dir, state) in [
            (tasks_dir, TaskState::Active),
            (repo.join(".agents/tasks/done"), TaskState::Closed),
        ] {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !name.ends_with(".json") || name.starts_with('.') || name.ends_with(".pub.json")
                {
                    continue;
                }
                let Ok(body) = std::fs::read_to_string(&p) else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
                    continue;
                };
                let Some(id) = v.get("id").and_then(|x| x.as_str()).map(str::to_string) else {
                    continue;
                };
                let text = format!(
                    "{} {}",
                    v.get("title").and_then(|x| x.as_str()).unwrap_or(""),
                    v.get("goal").and_then(|x| x.as_str()).unwrap_or("")
                );
                let deps = v
                    .get("deps")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|d| d.as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                q.known_ids.insert(id.clone());
                q.task_graph.insert(id.clone(), deps);
                q.existing.push(ExistingTask {
                    id,
                    text,
                    state: match v.get("closed").and_then(|x| x.as_bool()) {
                        Some(true) => TaskState::Closed,
                        _ => state.clone(),
                    },
                });
            }
        }
        q
    }

    /// Adopt a freshly published draft — subsequent dedup/validation sees it.
    fn adopt(&mut self, d: &TaskDraft) {
        self.known_ids.insert(d.id.clone());
        self.task_graph.insert(d.id.clone(), d.deps.clone());
        self.existing.push(ExistingTask {
            id: d.id.clone(),
            text: format!("{} {}", d.title, d.goal),
            state: TaskState::Active,
        });
    }
}

// --------------------------------------------------------------- discovery

/// Publication context for one drain: minting rules + the lineage log.
pub struct PubCtx<'a> {
    /// Drafting context (agent namespace, roadmap vector, accept argv,
    /// base deps, clock).
    pub draft: DraftCtx<'a>,
    /// Gap->evidence->task lineage log.
    pub log: &'a PublicationLog,
}

/// Per-unit discovery outcome.
#[derive(Debug)]
pub enum DiscoverItem {
    /// New task record ids published this unit.
    Published(Vec<String>),
    /// Audit ran; everything merged/linked into held proposals or was
    /// rejected as invalid — no new record.
    Absorbed,
    /// No working brain — the unit was re-queued for a later pass.
    BrainUnavailable,
    /// Flush spend bound hit — the unit was re-queued.
    BudgetBound,
}

/// Drain the trigger engine through the full production path, writing real
/// queue records with lineage. Unprocessable units are re-queued — the
/// engine can be reloaded and retried after a restart.
pub fn discover<R: Runner>(
    engine: &mut TriggerEngine,
    scope: &AuditScope<'_>,
    brain: &mut BrainRun<'_, R>,
    queue: &mut DiscoveryQueue,
    pubctx: &PubCtx<'_>,
) -> Vec<DiscoverItem> {
    let mut out = Vec::new();
    for unit in engine.take_pending() {
        match discover_unit(&unit, scope, brain, queue, pubctx) {
            Ok(item) => out.push(item),
            Err(stop) => {
                // Brain couldn't serve it — re-queue, report the real reason.
                engine.requeue(unit);
                out.push(match stop {
                    Some(FailoverStop::BudgetExhausted) => DiscoverItem::BudgetBound,
                    Some(
                        FailoverStop::NoEligibleCandidates
                        | FailoverStop::AttemptBudgetExhausted { .. }
                        | FailoverStop::DeadlineExceeded
                        | FailoverStop::AllLockedOut
                        | FailoverStop::Admission(_),
                    )
                    | None => DiscoverItem::BrainUnavailable,
                });
            }
        }
    }
    out
}

/// Process one audit unit. `Err(stop)` = no working brain served it —
/// the unit is re-queued by the caller.
fn discover_unit<R: Runner>(
    unit: &AuditUnit,
    scope: &AuditScope<'_>,
    brain: &mut BrainRun<'_, R>,
    queue: &mut DiscoveryQueue,
    pubctx: &PubCtx<'_>,
) -> Result<DiscoverItem, Option<FailoverStop>> {
    let inputs: Vec<AuditInput> = unit.inputs.iter().map(|i| i.to_input()).collect();
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
    let Some(brain_id) = rep.brain.clone() else {
        return Err(rep.brain_stop);
    };
    let mut published = Vec::new();
    for g in &rep.gaps {
        let ctx = DraftCtx {
            agent: pubctx.draft.agent,
            roadmap: pubctx.draft.roadmap,
            accept_cmd: pubctx.draft.accept_cmd.clone(),
            deps: pubctx.draft.deps.clone(),
            now: pubctx.draft.now,
        };
        let rank = rank_gap(g, 1, g.receipts.len() as u32, None);
        let draft = match propose(&queue.proposals, g, &queue.existing, &ctx, queue.next_seq) {
            Ok(ProposeOutcome::Created(d))
            | Ok(ProposeOutcome::LinkedFollowUp { draft: d, .. }) => *d,
            _ => continue, // merged / rejected-repeat / held-by-other — no dup
        };
        let meta = PubMeta {
            brain_opaque: Some(brain_id.clone()),
            rationale: rank.drivers.clone(),
            at_unix: scope.now,
        };
        let know = QueueKnowledge {
            known_ids: &queue.known_ids,
            task_graph: &queue.task_graph,
            known_vectors: &queue.vectors,
        };
        // Validation happens inside record_draft — malformed/fabricated/
        // unresolved drafts never reach the log or the queue.
        let Ok(mut rec) = record_draft(pubctx.log, &draft, g, &meta, &know) else {
            continue;
        };
        let Ok(_) = publish(&queue.tasks_dir, &draft) else {
            continue; // lineage stays RecordedLocal — honest retry state
        };
        let _ = transition(&mut rec, PublishState::Published, &[], scope.now);
        let _ = pubctx.log.update(&rec);
        queue.adopt(&draft);
        queue.next_seq += 1;
        published.push(draft.id.clone());
    }
    Ok(if published.is_empty() {
        DiscoverItem::Absorbed
    } else {
        DiscoverItem::Published(published)
    })
}

/// io convenience for tests/callers writing the discovered set.
pub fn published_ids(items: &[DiscoverItem]) -> Vec<String> {
    items
        .iter()
        .flat_map(|i| match i {
            DiscoverItem::Published(ids) => ids.clone(),
            DiscoverItem::Absorbed | DiscoverItem::BrainUnavailable | DiscoverItem::BudgetBound => {
                Vec::new()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain_gap_audit::{Gap, GapStatus, Impact};
    use crate::brain_gap_tasks::{validate_draft, AcceptCmd, DraftReject};
    use crate::brain_gap_triggers::{AuditInputSerde, DiscoveryEvent, TriggerPolicy};
    use std::process::Command;
    use std::sync::Mutex;
    use susi_gawd_agents::cloud_budget::{BudgetLedger, SpendPolicy};
    use susi_gawd_agents::cloud_intent::{Candidate, IntentConstraints, PinFallback};
    use susi_gawd_swarm::cloud_failover::{AttemptOutcome, FailoverBudget, Stores};
    use susi_gawd_swarm::cloud_lockout::{now_unix, LockoutPolicy, LockoutTracker};
    use susi_vendor_models::cloud_eligibility::{EligibilityStore, InferenceResult, Subject};
    use susi_vendor_models::cloud_quota::QuotaInventory;

    const T0: u64 = 4_000_000_000;

    fn cand(key: &str, model: &str, quality: f64, cost: f64) -> Candidate {
        cand_on("acme", key, model, quality, cost)
    }

    fn cand_on(provider: &str, key: &str, model: &str, quality: f64, cost: f64) -> Candidate {
        let mut q = BTreeMap::new();
        q.insert("coding".to_string(), ((quality * 20.0) as u32, 20u32));
        Candidate {
            provider: provider.into(),
            api_key: key.into(),
            account: None,
            region: None,
            model: model.into(),
            context_tokens: 128_000,
            modalities: vec![],
            supports_tools: true,
            supports_structured_output: true,
            residency: None,
            est_latency_ms: 10,
            cost_per_mtok: Some(cost),
            quality: q,
        }
    }

    struct Fx {
        dir: PathBuf,
        elig: EligibilityStore,
        quota: QuotaInventory,
        lockouts: LockoutTracker,
        ledger: BudgetLedger,
    }
    impl Fx {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("gape2e-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self {
                elig: EligibilityStore::new(),
                quota: QuotaInventory::new(),
                lockouts: LockoutTracker::new(LockoutPolicy::default(), now_unix),
                ledger: BudgetLedger::new(),
                dir,
            }
        }
        fn stores(&mut self) -> Stores<'_> {
            Stores {
                eligibility: &mut self.elig,
                quota: &self.quota,
                lockouts: &mut self.lockouts,
                ledger: &self.ledger,
            }
        }
        fn mark_usable(&mut self, c: &Candidate, at: u64) {
            self.elig.record_inference(
                Subject {
                    provider: &c.provider,
                    api_key: &c.api_key,
                    account: c.account.as_deref(),
                    region: c.region.as_deref(),
                    model: &c.model,
                },
                &InferenceResult::Success,
                at,
            );
        }
        fn kill(&mut self, c: &Candidate, status: u16, at: u64) {
            self.elig.record_inference(
                Subject {
                    provider: &c.provider,
                    api_key: &c.api_key,
                    account: c.account.as_deref(),
                    region: c.region.as_deref(),
                    model: &c.model,
                },
                &InferenceResult::Failed {
                    status: Some(status),
                    body_snippet: "outage".into(),
                    retry_after_secs: None,
                },
                at,
            );
        }
    }

    /// Brain runner returning a scripted findings payload; records which
    /// candidate index each attempt hit (selection audit).
    struct ScriptBrain {
        body: String,
        attempted: Mutex<Vec<usize>>,
    }
    impl Runner for ScriptBrain {
        fn attempt(&mut self, index: usize, _d: u64) -> AttemptOutcome {
            self.attempted
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(index);
            AttemptOutcome::Success(self.body.clone())
        }
    }

    fn scope_intent() -> IntentConstraints {
        IntentConstraints {
            task_class: "coding".into(),
            pin_fallback: PinFallback::Deny,
            ..Default::default()
        }
    }

    fn budget(cap: u64) -> FailoverBudget {
        FailoverBudget {
            max_attempts: 3,
            deadline_ms: Some(T0 * 1000 + 60_000),
            spend: SpendPolicy::PaidAuthorized {
                max_spend_micro: cap,
            },
            attempt_estimate_micros: 100,
            now_ms: T0 * 1000,
        }
    }

    /// A temporary repository with a real mini cargo workspace whose
    /// `gap_fix` test FAILS until `gapfix/gap_fixed.marker` exists — the
    /// published task's acceptance command literally detects the repair.
    fn mk_repo(tag: &str) -> PathBuf {
        let repo = std::env::temp_dir().join(format!("gaprepo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        let krate = repo.join("gapfix");
        std::fs::create_dir_all(krate.join("src")).unwrap();
        std::fs::create_dir_all(repo.join(".agents/tasks/done")).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[workspace]\nmembers = [\"gapfix\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        std::fs::write(
            krate.join("Cargo.toml"),
            "[package]\nname = \"gapfix\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            krate.join("src/lib.rs"),
            "#[cfg(test)]\nmod t {\n    #[test]\n    fn gap_fix_works() {\n        assert!(std::path::Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"gap_fixed.marker\").exists(), \"gap not repaired\");\n    }\n}\n",
        )
        .unwrap();
        let _ = Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(&repo)
            .output();
        // A closed upstream task the gap work depends on.
        std::fs::write(
            repo.join(".agents/tasks/done/T-DEVIN-28.json"),
            serde_json::json!({
                "id": "T-DEVIN-28", "title": "Verify default cloud brain e2e",
                "goal": "brain diagnostics coverage", "size": "l",
                "deps": [], "accept": {"cmd": ["cargo","test","-p","susi-gawd","x","--locked"]},
                "roadmap": "VC-201-020", "created_by": "DEVIN",
                "created_unix": T0, "closed": true
            })
            .to_string(),
        )
        .unwrap();
        repo
    }

    fn queue(repo: &Path, next_seq: u32) -> DiscoveryQueue {
        DiscoveryQueue::load(
            repo,
            ["VC-201-020".to_string()].into_iter().collect(),
            next_seq,
        )
    }

    fn pubctx<'a>(log: &'a PublicationLog) -> PubCtx<'a> {
        PubCtx {
            draft: DraftCtx {
                agent: "DEVIN",
                roadmap: "VC-201-020",
                accept_cmd: vec![
                    "cargo".into(),
                    "test".into(),
                    "-p".into(),
                    "gapfix".into(),
                    "gap_fix".into(),
                    "--locked".into(),
                ],
                deps: vec!["T-DEVIN-28".into()],
                now: T0,
            },
            log,
        }
    }

    fn gap_receipt(id: &str) -> AuditInputSerde {
        AuditInputSerde {
            id: id.into(),
            source: "intent".into(),
            expected: "sso login completes and returns a session".into(),
            observed: "sso login 500s with provider key sk-live-SECRET-99".into(),
            reproducible: Some("susi login --sso".into()),
            hidden_behind: None,
            at_unix: T0,
        }
    }

    #[test]
    fn brain_gap_e2e_observable_gap_mints_executable_roadmap_task() {
        let repo = mk_repo("main");
        let mut f = Fx::new("main");
        let strong = cand("sk-strong", "m-strong", 0.95, 0.4);
        let weak = cand("sk-weak", "m-weak", 0.50, 0.4);
        f.mark_usable(&strong, T0);
        f.mark_usable(&weak, T0);
        let candidates = vec![strong.clone(), weak.clone()];

        // Ingress: a failed-intent event carrying a redacted receipt.
        let secret = "sk-live-SECRET-99".to_string();
        let mut engine = TriggerEngine::load(
            f.dir.join("eng"),
            TriggerPolicy {
                debounce_secs: 60,
                max_pending: 16,
                max_receipts_per_unit: 8,
            },
        );
        let input = redacted_input(&gap_receipt("receipt-sso"), std::slice::from_ref(&secret));
        assert!(
            !input.observed.contains(&secret),
            "credential stripped before it reaches a brain prompt"
        );
        engine.on_event(
            DiscoveryEvent::IntentFailed {
                id: "sess-9".into(),
                detail: "sso".into(),
            },
            vec![input],
            T0,
        );

        // Discovery: strongest working brain first, no findings needed —
        // the evidence-direct pass sees expected != observed.
        let mut runner = ScriptBrain {
            body: "[]".into(),
            attempted: Mutex::new(Vec::new()),
        };
        let log = PublicationLog::load(f.dir.join("pub"));
        let mut q = queue(&repo, 500);
        let items = discover(
            &mut engine,
            &AuditScope {
                intent: &scope_intent(),
                candidates: &candidates,
                now: T0,
            },
            &mut BrainRun {
                stores: &mut f.stores(),
                budget: budget(10_000_000),
                runner: &mut runner,
            },
            &mut q,
            &pubctx(&log),
        );
        let ids = published_ids(&items);
        assert_eq!(ids.len(), 1, "one actionable task minted: {items:?}");
        assert_eq!(
            runner.attempted.lock().unwrap().as_slice(),
            &[0],
            "the strongest working brain was dispatched first"
        );

        // The record is a real queue file with the right linkage.
        let path = repo.join(format!(".agents/tasks/{}.json", ids[0]));
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["roadmap"], "VC-201-020");
        assert_eq!(v["deps"], serde_json::json!(["T-DEVIN-28"]));
        assert_eq!(
            v["accept"]["cmd"],
            serde_json::json!(["cargo", "test", "-p", "gapfix", "gap_fix", "--locked"])
        );
        assert!(v["goal"].as_str().unwrap().contains("sso login completes"));
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains(&secret),
            "published record carries no credential material"
        );

        // Lineage: gap -> evidence -> task, brain provenance, ordered states.
        let rec = log.get(&ids[0]).expect("lineage record");
        assert!(rec.evidence.contains(&"receipt-sso".to_string()));
        let strong_id = strong.opaque_id();
        assert_eq!(rec.discovery_brain.as_deref(), Some(strong_id.as_str()));
        assert_eq!(rec.state, PublishState::Published);
        assert!(!rec.rationale.is_empty(), "priority rationale recorded");

        // Acceptance FAILS before the repair — the task is real work, not
        // a victory lap.
        let accept: Vec<String> = serde_json::from_value(v["accept"]["cmd"].clone()).unwrap();
        let before = Command::new(&accept[0])
            .args(&accept[1..])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(!before.success(), "acceptance must fail before repair");
        // Capability did NOT improve because a task record exists.
        // Repair lands…
        std::fs::write(repo.join("gapfix/gap_fixed.marker"), "fixed\n").unwrap();
        let after = Command::new(&accept[0])
            .args(&accept[1..])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(after.success(), "acceptance passes only after repair");
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn brain_gap_e2e_repeat_discovery_never_duplicates() {
        let repo = mk_repo("dup");
        let mut f = Fx::new("dup");
        let brain = cand("sk-b", "m-brain", 0.9, 0.0);
        f.mark_usable(&brain, T0);
        let candidates = vec![brain];
        let mut engine = TriggerEngine::load(f.dir.join("eng"), TriggerPolicy::default());
        engine.on_event(
            DiscoveryEvent::CiRegression {
                check: "sso".into(),
            },
            vec![gap_receipt("receipt-sso")],
            T0,
        );
        let log = PublicationLog::load(f.dir.join("pub"));
        let mut q = queue(&repo, 600);
        let mut runner = ScriptBrain {
            body: "[]".into(),
            attempted: Mutex::new(Vec::new()),
        };
        let mut run = |engine: &mut TriggerEngine, q: &mut DiscoveryQueue| {
            let i = scope_intent();
            let scope = AuditScope {
                intent: &i,
                candidates: &candidates,
                now: T0,
            };
            let mut br = BrainRun {
                stores: &mut f.stores(),
                budget: budget(10_000_000),
                runner: &mut runner,
            };
            discover(engine, &scope, &mut br, q, &pubctx(&log))
        };
        let items = run(&mut engine, &mut q);
        assert_eq!(published_ids(&items).len(), 1);
        // Same issue arrives again later (past debounce) → dedup absorbs.
        engine.on_event(
            DiscoveryEvent::CiRegression {
                check: "sso".into(),
            },
            vec![gap_receipt("receipt-sso")],
            T0 + 90_000,
        );
        let items2 = run(&mut engine, &mut q);
        assert!(
            matches!(items2.as_slice(), [DiscoverItem::Absorbed]),
            "repeat discovery absorbed: {items2:?}"
        );
        assert_eq!(
            std::fs::read_dir(repo.join(".agents/tasks"))
                .unwrap()
                .filter(|e| e
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .and_then(|x| x.to_str())
                    == Some("json"))
                .count(),
            1,
            "exactly one queue record exists"
        );
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn brain_gap_e2e_closed_task_residual_mints_linked_followup() {
        let repo = mk_repo("resid");
        let mut f = Fx::new("resid");
        let brain = cand("sk-b", "m-brain", 0.9, 0.0);
        f.mark_usable(&brain, T0);
        let candidates = vec![brain];
        // A closed task whose text overlaps the residual gap — dedup must
        // mint a FOLLOW-UP linked to it, not reopen or duplicate it.
        std::fs::write(
            repo.join(".agents/tasks/done/T-DEVIN-55.json"),
            serde_json::json!({
                "id": "T-DEVIN-55",
                "title": "sso login completes returns session diverges",
                "goal": "sso login completes and returns a session but sso login 500s",
                "size": "m", "deps": [],
                "accept": {"cmd": ["cargo","test","-p","gapfix","gap_fix","--locked"]},
                "roadmap": "VC-201-020", "created_by": "DEVIN",
                "created_unix": T0, "closed": true
            })
            .to_string(),
        )
        .unwrap();
        let mut engine = TriggerEngine::load(f.dir.join("eng"), TriggerPolicy::default());
        // TaskClosed event carrying the residual mastery evidence.
        engine.on_event(
            DiscoveryEvent::TaskClosed {
                task_id: "T-DEVIN-55".into(),
                residual: Vec::new(),
            },
            vec![gap_receipt("receipt-sso")],
            T0,
        );
        let log = PublicationLog::load(f.dir.join("pub"));
        let mut q = queue(&repo, 700);
        let mut runner = ScriptBrain {
            body: "[]".into(),
            attempted: Mutex::new(Vec::new()),
        };
        let i = scope_intent();
        let items = discover(
            &mut engine,
            &AuditScope {
                intent: &i,
                candidates: &candidates,
                now: T0,
            },
            &mut BrainRun {
                stores: &mut f.stores(),
                budget: budget(10_000_000),
                runner: &mut runner,
            },
            &mut q,
            &pubctx(&log),
        );
        let ids = published_ids(&items);
        assert_eq!(ids.len(), 1, "follow-up published: {items:?}");
        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(repo.join(format!(".agents/tasks/{}.json", ids[0]))).unwrap(),
        )
        .unwrap();
        let deps: Vec<String> = serde_json::from_value(v["deps"].clone()).unwrap();
        assert!(
            deps.contains(&"T-DEVIN-55".to_string()),
            "follow-up links the closed task: {deps:?}"
        );
        assert!(
            v["title"].as_str().unwrap().contains("Follow-up"),
            "marked as recurrence, closed history preserved"
        );
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn brain_gap_e2e_fabricated_and_invalid_never_publish() {
        let repo = mk_repo("fab");
        let mut f = Fx::new("fab");
        let brain = cand("sk-b", "m-brain", 0.9, 0.0);
        f.mark_usable(&brain, T0);
        let candidates = vec![brain];
        let mut engine = TriggerEngine::load(f.dir.join("eng"), TriggerPolicy::default());
        // Real receipt (diverging) + brain invents findings on receipts
        // that don't exist — those become hypotheses, never tasks.
        engine.on_event(
            DiscoveryEvent::ExplicitAudit { by: "dev".into() },
            vec![AuditInputSerde {
                id: "r-ok".into(),
                source: "intent".into(),
                expected: "works".into(),
                observed: "works".into(), // matching — no verified gap
                reproducible: None,
                hidden_behind: None,
                at_unix: T0,
            }],
            T0,
        );
        let log = PublicationLog::load(f.dir.join("pub"));
        let mut q = queue(&repo, 800);
        let mut runner = ScriptBrain {
            body: serde_json::json!([
                {"title": "hallucinated crash", "receipts": ["fake-99"], "confidence": 0.9}
            ])
            .to_string(),
            attempted: Mutex::new(Vec::new()),
        };
        let i = scope_intent();
        let items = discover(
            &mut engine,
            &AuditScope {
                intent: &i,
                candidates: &candidates,
                now: T0,
            },
            &mut BrainRun {
                stores: &mut f.stores(),
                budget: budget(10_000_000),
                runner: &mut runner,
            },
            &mut q,
            &pubctx(&log),
        );
        assert!(
            matches!(items.as_slice(), [DiscoverItem::Absorbed]),
            "fabricated finding is a hypothesis — hypotheses never mint tasks"
        );
        assert_eq!(
            std::fs::read_dir(repo.join(".agents/tasks"))
                .unwrap()
                .filter(|e| e
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .and_then(|x| x.to_str())
                    == Some("json"))
                .count(),
            0
        );

        // Invalid drafts are refused at validation — arbitrary shell,
        // zero-test filters, unresolved deps, unknown vectors.
        let good = Gap {
            id: "GAP-x".into(),
            title: "x diverges".into(),
            receipts: vec!["r-ok".into()],
            trigger: None,
            expected: "a".into(),
            observed: "b".into(),
            hidden_behind: None,
            status: GapStatus::Verified,
            impact: Impact::Low,
            confidence: 0.9,
            observed_at: T0,
        };
        let ctx = DraftCtx {
            agent: "DEVIN",
            roadmap: "VC-201-020",
            accept_cmd: vec!["rm".into(), "-rf".into(), "/".into()],
            deps: vec![],
            now: T0,
        };
        let d = crate::brain_gap_tasks::draft_from_gap(&good, 900, &ctx).unwrap();
        let known: BTreeSet<String> = ["T-DEVIN-28".into()].into_iter().collect();
        let vectors: BTreeSet<String> = ["VC-201-020".into()].into_iter().collect();
        let graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
        assert_eq!(
            validate_draft(&d, &known, &graph, &vectors, &BTreeSet::new()),
            Err(DraftReject::UnsupportedCommand("rm -rf /".into()))
        );
        let mut d2 = d.clone();
        d2.accept = AcceptCmd {
            cmd: vec![
                "cargo".into(),
                "test".into(),
                "-p".into(),
                "gapfix".into(),
                "--locked".into(),
            ],
        };
        assert_eq!(
            validate_draft(&d2, &known, &graph, &vectors, &BTreeSet::new()),
            Err(DraftReject::ZeroTestFilter)
        );
        let mut d3 = d2.clone();
        d3.accept = AcceptCmd {
            cmd: vec![
                "cargo".into(),
                "test".into(),
                "-p".into(),
                "gapfix".into(),
                "gap_fix".into(),
                "--locked".into(),
            ],
        };
        d3.deps = vec!["T-NOPE-1".into()];
        assert_eq!(
            validate_draft(&d3, &known, &graph, &vectors, &BTreeSet::new()),
            Err(DraftReject::UnresolvedDep("T-NOPE-1".into()))
        );
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn brain_gap_e2e_outage_fallback_and_budget_bound() {
        let repo = mk_repo("fo");
        let mut f = Fx::new("fo");
        let dead = cand("sk-dead", "m-dead", 0.99, 0.5);
        let backup = cand_on("beta", "sk-back", "m-back", 0.5, 0.5);
        f.kill(&dead, 503, T0);
        f.mark_usable(&backup, T0);
        let candidates = vec![dead, backup];
        let mut engine = TriggerEngine::load(f.dir.join("eng"), TriggerPolicy::default());
        engine.on_event(
            DiscoveryEvent::CiRegression { check: "x".into() },
            vec![gap_receipt("receipt-sso")],
            T0,
        );
        let log = PublicationLog::load(f.dir.join("pub"));
        let mut q = queue(&repo, 900);
        let mut runner = ScriptBrain {
            body: "[]".into(),
            attempted: Mutex::new(Vec::new()),
        };
        let i = scope_intent();
        let items = discover(
            &mut engine,
            &AuditScope {
                intent: &i,
                candidates: &candidates,
                now: T0,
            },
            &mut BrainRun {
                stores: &mut f.stores(),
                budget: budget(10_000_000),
                runner: &mut runner,
            },
            &mut q,
            &pubctx(&log),
        );
        assert_eq!(published_ids(&items).len(), 1);
        assert_eq!(
            runner.attempted.lock().unwrap().as_slice(),
            &[1],
            "dead strongest brain skipped; backup served"
        );
        let rec = log.get(&published_ids(&items)[0]).unwrap();
        assert!(
            rec.discovery_brain.as_deref().unwrap().contains("m-back"),
            "lineage records the backup brain"
        );

        // Budget bound: a zero-cap spend policy holds the queue.
        engine.on_event(
            DiscoveryEvent::CiRegression { check: "y".into() },
            vec![AuditInputSerde {
                id: "r2".into(),
                source: "intent".into(),
                expected: "a".into(),
                observed: "b".into(),
                reproducible: None,
                hidden_behind: None,
                at_unix: T0,
            }],
            T0 + 200,
        );
        let mut zero = budget(10_000_000);
        zero.spend = SpendPolicy::PaidAuthorized { max_spend_micro: 0 };
        let items2 = discover(
            &mut engine,
            &AuditScope {
                intent: &i,
                candidates: &candidates,
                now: T0 + 200,
            },
            &mut BrainRun {
                stores: &mut f.stores(),
                budget: zero,
                runner: &mut runner,
            },
            &mut q,
            &pubctx(&log),
        );
        assert!(
            matches!(items2.as_slice(), [DiscoverItem::BudgetBound])
                || matches!(items2.as_slice(), [DiscoverItem::BrainUnavailable]),
            "bounded spend holds the unit: {items2:?}"
        );
        assert_eq!(engine.pending().len(), 1, "unit re-queued, not lost");
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn brain_gap_e2e_concurrent_discovery_single_record() {
        // Two agents discover the same issue against one proposals dir:
        // `create_new` claim means exactly one minted record.
        let repo = mk_repo("conc");
        let f2dir = std::env::temp_dir().join(format!("gape2e-conc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&f2dir);
        let pdir = f2dir.join("prop");
        let gap = Gap {
            id: "GAP-conc".into(),
            title: "sso login diverges".into(),
            receipts: vec!["receipt-sso".into()],
            trigger: Some("repro".into()),
            expected: "sso login completes".into(),
            observed: "sso login 500s".into(),
            hidden_behind: None,
            status: GapStatus::Verified,
            impact: Impact::Medium,
            confidence: 0.9,
            observed_at: T0,
        };
        let ctx_a = DraftCtx {
            agent: "DEVIN",
            roadmap: "VC-201-020",
            accept_cmd: vec![
                "cargo".into(),
                "test".into(),
                "-p".into(),
                "gapfix".into(),
                "gap_fix".into(),
                "--locked".into(),
            ],
            deps: vec![],
            now: T0,
        };
        let ctx_b = DraftCtx {
            agent: "CODEX",
            roadmap: "VC-201-020",
            accept_cmd: ctx_a.accept_cmd.clone(),
            deps: vec![],
            now: T0,
        };
        let pa = ProposalStore::load(pdir.clone());
        let pb = ProposalStore::load(pdir);
        // Race order: A claims first, B sees the held proposal.
        let ra = propose(&pa, &gap, &[], &ctx_a, 901);
        let rb = propose(&pb, &gap, &[], &ctx_b, 901);
        assert!(matches!(ra, Ok(ProposeOutcome::Created(_))));
        assert!(
            matches!(
                rb,
                Ok(ProposeOutcome::HeldByOther { .. }) | Ok(ProposeOutcome::Merged { .. })
            ),
            "concurrent second proposer never mints a duplicate: {rb:?}"
        );
        let _ = repo; // workspace fixture unused here; dedup is the point
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&f2dir);
    }
}
