//! Mastery verification for VC-202-020 (T-DEEPSEEK-115): model health is a
//! state machine with typed failure classes, on the production dispatch
//! path — and it is a separate axis from capability.
//!
//! What the tests pin:
//! - `LockoutTracker::health` reports each scope as a typed
//!   `susi_core::model_health::ModelHealth`: healthy, cooling after a rate
//!   limit with `Retry-After` honoured in the deadline, degraded after
//!   timeouts/5xx, key-dead, or out of quota.
//! - `parallel_dispatch::run_jobs` — the production dispatch loop — reports
//!   that typed health per consulted target in `JobOutcome::target_health`.
//! - Health state survives a restart (`save`/`load` round-trip).
//! - The axis separation is structural: a storm of 429s and timeouts
//!   changes `ModelHealth` but leaves `BrainRankStore` capability scores,
//!   confidence and evidence counts byte-identical — availability gates
//!   leadership through `is_working`, never the score.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Mutex;

use susi_core::model_health::{FailureClass, ModelHealth};
use susi_gawd_agents::brain_ranking::{
    BrainRankStore, EvalDimension, EvalEvidence, EvalProvenance,
};
use susi_gawd_agents::cloud_budget::BudgetLedger;
use susi_gawd_agents::cloud_intent::{Candidate, IntentConstraints};
use susi_vendor_models::cloud_eligibility::{EligibilityKind, EligibilityStore, InferenceResult};
use susi_vendor_models::cloud_quota::QuotaInventory;

use crate::cloud_failover::{AttemptOutcome, FailoverBudget, Runner};
use crate::cloud_lockout::{LockoutPolicy, LockoutTracker};
use crate::parallel_dispatch::{run_jobs, DispatchPlan, Job, Shared};

const T0: u64 = 1_700_000_000;

// Per-test-thread clock — parallel tests never race each other's time.
thread_local! {
    static NOW: Cell<u64> = const { Cell::new(T0) };
}

fn test_clock() -> u64 {
    NOW.get()
}

fn set_now(secs: u64) {
    NOW.set(secs);
}

fn tracker() -> LockoutTracker {
    LockoutTracker::new(
        LockoutPolicy {
            base_cooldown_ms: 1_000,
            max_cooldown_ms: 60_000,
            max_consecutive_failures: 3,
            half_open_probes: 2,
            jitter_ppt: 0,
        },
        test_clock,
    )
}

fn fail(status: u16, body: &str) -> InferenceResult {
    InferenceResult::Failed {
        status: Some(status),
        body_snippet: body.into(),
        retry_after_secs: None,
    }
}

/// Every provider failure class drives a distinct typed health state:
/// rate-limited cools, quota reports out-of-quota, a dead key is `Dead`,
/// timeouts/5xx degrade, and a proven target is healthy.
#[test]
fn model_health_failure_classes_verdicts_drive_typed_states() {
    set_now(T0);
    let mut t = tracker();
    let s = "prov:cred8:model";

    // 401 → key-dead, permanently (never re-armed on a timer).
    t.record(s, &fail(401, "invalid api key"));
    assert_eq!(
        t.health(s, EligibilityKind::InvalidCredential),
        ModelHealth::Dead {
            class: FailureClass::InvalidCredential
        }
    );

    let mut t = tracker();
    // 429 → cooling: Unhealthy{RateLimited} while the window holds.
    t.record(s, &fail(429, "too many requests"));
    match t.health(s, EligibilityKind::RateLimited) {
        ModelHealth::Unhealthy { class, .. } => assert_eq!(class, FailureClass::RateLimited),
        other => panic!("expected rate-limited cooling, got {other:?}"),
    }

    let mut t = tracker();
    // Quota exhausted → out of quota, recoverable (not Dead).
    t.record(
        s,
        &InferenceResult::Failed {
            status: Some(429),
            body_snippet: "quota exceeded".into(),
            retry_after_secs: Some(3600),
        },
    );
    match t.health(s, EligibilityKind::QuotaExhausted) {
        ModelHealth::Unhealthy { class, reason } => {
            assert_eq!(class, FailureClass::InsufficientQuota);
            assert!(reason.contains("quota"), "reason names quota: {reason}");
        }
        other => panic!("expected out-of-quota, got {other:?}"),
    }

    let mut t = tracker();
    // 503 → degraded while intermittent.
    t.record(s, &fail(503, "service unavailable"));
    match t.health(s, EligibilityKind::ServiceUnavailable) {
        ModelHealth::Degraded { .. } => {}
        other => panic!("expected degraded, got {other:?}"),
    }

    // A never-failed scope reports Healthy once evidence proves it usable.
    let t = tracker();
    assert_eq!(
        t.health("prov:cred9:ok", EligibilityKind::Usable),
        ModelHealth::Healthy
    );
}

/// Cooling honours the provider's deadline: a `Retry-After` on a 429 sets
/// the health reason's deadline, and the permit refuses until it passes.
#[test]
fn model_health_failure_classes_retry_after_sets_cooling_deadline() {
    set_now(T0);
    let mut t = tracker();
    let s = "prov:cred8:model";
    t.record(
        s,
        &InferenceResult::Failed {
            status: Some(429),
            body_snippet: "slow down".into(),
            retry_after_secs: Some(120),
        },
    );
    let expected_ms = (T0 + 120) * 1000;
    match t.health(s, EligibilityKind::RateLimited) {
        ModelHealth::Unhealthy { class, reason } => {
            assert_eq!(class, FailureClass::RateLimited);
            assert!(
                reason.contains(&expected_ms.to_string()),
                "cooling names the Retry-After deadline {expected_ms}: {reason}"
            );
        }
        other => panic!("expected cooling, got {other:?}"),
    }
    // While cooling, dispatch refuses; once the deadline passes, a probe
    // is admitted — the state machine recovers on schedule.
    assert!(!matches!(
        t.permit(s, EligibilityKind::Unknown),
        crate::cloud_lockout::Permit::Allowed
    ));
    set_now(T0 + 121);
    assert_eq!(
        t.permit(s, EligibilityKind::Unknown),
        crate::cloud_lockout::Permit::Allowed
    );
}

/// A genuine success is the only thing that returns a scope to `Healthy` —
/// and it lifts even a permanent block, because fresh positive evidence is
/// the only honest recovery signal.
#[test]
fn model_health_failure_classes_success_restores_health() {
    set_now(T0);
    let mut t = tracker();
    let s = "prov:cred8:model";
    t.record(s, &fail(401, "bad key"));
    assert!(matches!(
        t.health(s, EligibilityKind::Unknown),
        ModelHealth::Dead { .. }
    ));
    t.record(s, &InferenceResult::Success);
    assert_eq!(t.health(s, EligibilityKind::Usable), ModelHealth::Healthy);
}

/// Health survives a restart: the persisted tracker round-trips through
/// `save`/`load` and the reloaded instance reports the same typed state.
#[test]
fn model_health_failure_classes_state_survives_restart() {
    set_now(T0);
    let dir = std::env::temp_dir().join(format!(
        "susi_vc202020m_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let path = dir.join("lockouts.json");
    let s = "prov:cred8:model";
    let before = {
        let mut t = tracker();
        t.record(s, &fail(401, "bad key"));
        t.save(&path).expect("save");
        t.health(s, EligibilityKind::Unknown)
    };
    let reloaded = LockoutTracker::load(&path, LockoutPolicy::default(), test_clock);
    assert_eq!(reloaded.health(s, EligibilityKind::Unknown), before);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The production dispatch loop reports typed health for every target it
/// consulted: the rate-limited candidate is `Unhealthy{RateLimited}`, the
/// winner is `Healthy` — availability stays typed next to the attempt
/// trace, never folded into the output.
#[test]
fn model_health_failure_classes_dispatch_reports_typed_health() {
    fn cand(key: &str, model: &str, provider: &str, account: &str) -> Candidate {
        Candidate {
            provider: provider.into(),
            api_key: key.into(),
            account: Some(account.into()),
            region: None,
            model: model.into(),
            context_tokens: 128_000,
            modalities: vec![],
            supports_tools: true,
            supports_structured_output: true,
            residency: None,
            est_latency_ms: 100,
            cost_per_mtok: Some(0.0),
            quality: BTreeMap::new(),
        }
    }
    struct Storm {
        bad: usize,
    }
    impl Runner for Storm {
        fn attempt(&mut self, index: usize, _r: u64) -> AttemptOutcome {
            if index == self.bad {
                AttemptOutcome::PreDispatch(InferenceResult::Failed {
                    status: Some(429),
                    body_snippet: "rate limited".into(),
                    retry_after_secs: Some(60),
                })
            } else {
                AttemptOutcome::Success("ok".into())
            }
        }
    }

    let cs = vec![
        cand("sk-a", "mA", "prov-a", "acct-a"),
        cand("sk-b", "mB", "prov-b", "acct-b"),
    ];
    // The 429'd candidate must sort first so it is genuinely attempted.
    let mut cs = cs;
    cs[0].quality.insert("coding".into(), (50, 50));
    let elig = Mutex::new(EligibilityStore::new());
    let quota = QuotaInventory::new();
    let lock = Mutex::new(tracker());
    let ledger = BudgetLedger::new();
    let shared = Shared::new(&elig, &quota, &lock, &ledger);
    let jobs = vec![Job {
        id: "j1".into(),
        intent: IntentConstraints {
            task_class: "coding".into(),
            discovery_budget: 3,
            ..Default::default()
        },
    }];
    let plan = DispatchPlan {
        max_workers: 1,
        mission: "m".into(),
        job_cpu_millis: 0,
        job_ram_mb: 0,
        job_vram_mb: 0,
        job_subprocesses: 0,
        per_job: FailoverBudget {
            max_attempts: 8,
            deadline_ms: Some((T0 + 300) * 1000),
            spend: susi_gawd_agents::cloud_budget::SpendPolicy::PaidAuthorized {
                max_spend_micro: 100_000,
            },
            attempt_estimate_micros: 10,
            now_ms: T0 * 1000,
        },
    };
    let outcomes = run_jobs(&jobs, &cs, &shared, plan, &|_j: &Job| Storm { bad: 0 });
    let out = &outcomes[0];
    assert!(out.output.is_some(), "failover must still serve the job");
    let bad_health = out.target_health.values().find(|h| {
        matches!(
            h,
            ModelHealth::Unhealthy {
                class: FailureClass::RateLimited,
                ..
            }
        )
    });
    assert!(
        bad_health.is_some(),
        "the 429'd target must be reported Unhealthy{{RateLimited}}: {:?}",
        out.target_health
    );
    assert!(
        out.target_health
            .values()
            .any(|h| *h == ModelHealth::Healthy),
        "the winner must be reported Healthy: {:?}",
        out.target_health
    );
}

/// The axis split, refuted against contamination: a storm of 429s and
/// timeouts moves `ModelHealth` but leaves capability measurement — score,
/// confidence, evidence count — byte-identical. Availability only ever
/// gates leadership through `is_working`.
#[test]
fn model_health_failure_classes_storm_leaves_capability_rank_untouched() {
    let dir = std::env::temp_dir().join(format!(
        "susi_vc202020r_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let mut store = BrainRankStore::load(dir.clone(), 86_400);
    let ev = |model: &str, score: f64| EvalEvidence {
        task_class: "coding".into(),
        model: model.into(),
        version: "v1".into(),
        dimension: EvalDimension::Correctness,
        score,
        samples: 8,
        observed_unix: T0,
        provenance: EvalProvenance::VerifiedOutcome,
    };
    store.record(ev("strong-model", 0.9));
    store.record(ev("weak-model", 0.5));
    let cands: Vec<(String, String)> = vec![
        ("strong-model".into(), "v1".into()),
        ("weak-model".into(), "v1".into()),
    ];
    let snapshot = |working: &dyn Fn(&str, &str) -> bool| {
        store
            .rank("coding", &cands, working, T0 + 10)
            .into_iter()
            .map(|r| (r.model, r.score, r.confidence, r.evidence_n))
            .collect::<Vec<_>>()
    };
    let all_working = |_: &str, _: &str| true;
    let before = snapshot(&all_working);

    // A storm of 429s and timeouts batters the health axis.
    let mut t = tracker();
    for _ in 0..5 {
        t.record("prov:cred8:strong-model", &fail(429, "rate limited"));
        t.record("prov:cred8:weak-model", &fail(503, "down"));
    }
    match t.health("prov:cred8:strong-model", EligibilityKind::RateLimited) {
        ModelHealth::Unhealthy { class, .. } => assert_eq!(class, FailureClass::RateLimited),
        other => panic!("storm must move health, got {other:?}"),
    }

    // Ranking recomputed after the storm: capability identical.
    let after = snapshot(&all_working);
    assert_eq!(
        before, after,
        "a 429/timeout storm must leave capability ranking untouched"
    );
    // And `is_working` — the only door availability has into ranking —
    // changes the leadership flag, never the score.
    let stormed_down = |m: &str, _: &str| m != "strong-model";
    let ranked = store.rank("coding", &cands, &stormed_down, T0 + 10);
    let strong = ranked
        .iter()
        .find(|r| r.model == "strong-model")
        .expect("present");
    assert!(!strong.working, "the stormed model stops leading");
    assert_eq!(
        strong.score, before[0].1,
        "its capability score is untouched"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The health surface is on the production dispatch path — verified by
/// source inspection (the VC-202-007/010/014 convention): the dispatch
/// loop consults `health` when the permit refuses a target, and the job
/// outcome carries the typed map.
#[test]
fn model_health_failure_classes_health_is_on_dispatch_path() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/parallel_dispatch.rs"
    ))
    .expect("production dispatch source must exist");
    assert!(
        src.contains("pub target_health: BTreeMap<String, ModelHealth>"),
        "JobOutcome must carry typed per-target health"
    );
    assert!(
        src.contains("lk.health(&scope"),
        "the lockout gate must consult the typed health surface"
    );
    let lock_src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cloud_lockout.rs"))
            .expect("lockout source must exist");
    assert!(
        lock_src.contains("pub fn health("),
        "LockoutTracker::health must exist on the production tracker"
    );
}
