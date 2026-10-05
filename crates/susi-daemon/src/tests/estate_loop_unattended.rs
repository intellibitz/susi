//! T-DEEPSEEK-160 (VC-202-024): the estate reconciliation loop runs on
//! susi's own schedule — observes the estate, diffs it against the
//! declared desired state, acts within policy, audits every pass, and
//! resumes mid-pass after a crash rather than restarting the work.

use std::sync::Mutex;

use crate::estate_loop::{
    self, Action, ActionKind, Actuate, LoopState, Observed, ServiceObs, Verdict,
};
use crate::susi_config::desired_state::{DesiredState, parse_desired_state};

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("susi-estate-loop-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn desired(json: &str) -> DesiredState {
    parse_desired_state(json).expect("desired document parses")
}

/// Records every act it is asked to take; verdicts can be scripted per
/// action id.
struct Recorder {
    calls: Mutex<Vec<String>>,
    fail: Vec<String>,
}

impl Recorder {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail: Vec::new(),
        }
    }
    fn failing(ids: &[&str]) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail: ids.iter().map(|s| s.to_string()).collect(),
        }
    }
    fn taken(&self) -> Vec<String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Actuate for Recorder {
    fn act(&self, action: &Action) -> Verdict {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(action.id());
        if self.fail.iter().any(|f| f == &action.id()) {
            Verdict::Failed("scripted failure".to_string())
        } else {
            Verdict::Applied("ok".to_string())
        }
    }
}

fn observed() -> Observed {
    Observed {
        loaded_models: vec!["stray-weights".to_string(), "kept-model".to_string()],
        services: vec![
            ServiceObs {
                name: "susi-native".to_string(),
                up: false,
                stopped: true,
                external: false,
            },
            ServiceObs {
                name: "susi-config".to_string(),
                up: true,
                stopped: false,
                external: false,
            },
        ],
        cooled_providers: vec!["openai:gpt-x".to_string()],
    }
}

fn full_desired() -> DesiredState {
    desired(
        r#"{
        "schema_version": "susi.desired_state/v1",
        "models": [{"id": "kept-model", "kind": "warm"}],
        "runtimes": [
            {"id": "susi-native", "kind": "running"},
            {"id": "susi-config", "kind": "stopped"}
        ],
        "providers": [{"id": "openai:gpt-x", "kind": "live"}]
    }"#,
    )
}

#[test]
fn estate_loop_unattended_plans_every_kind_and_acts_through_the_seam() {
    let home = scratch("plan-act");
    let report = estate_loop::run_pass_with(
        &home,
        Some(&full_desired()),
        &observed(),
        1_000,
        &Recorder::new(),
    )
    .expect("pass completes");

    // The diff produced every kind of act the goal names: warm the
    // declared-but-cold model, drain the declared-stopped runtime, start
    // the declared-running one, rotate away from the dead credential,
    // collect the resident weight nothing declares.
    let kinds: Vec<ActionKind> = report.receipts.iter().map(|r| r.action.kind).collect();
    for want in [
        ActionKind::DrainRuntime,
        ActionKind::StartRuntime,
        ActionKind::RotateKey,
        ActionKind::CollectOrphan,
    ] {
        assert!(kinds.contains(&want), "missing {want:?} in {kinds:?}");
    }
    // The declared warm model is already resident — nothing to do for it.
    assert!(
        !report
            .receipts
            .iter()
            .any(|r| r.action.id() == "warm_model:kept-model"),
        "resident declared model produced no act: {:?}",
        report.receipts
    );
    // Every act applied — the recorded ids are what the seam was asked.
    assert!(
        report
            .receipts
            .iter()
            .all(|r| matches!(r.verdict, Verdict::Applied(_))),
        "{:?}",
        report.receipts
    );
}

#[test]
fn estate_loop_unattended_policy_holds_never_act_but_are_audited() {
    let home = scratch("policy");
    let d = desired(
        r#"{
        "schema_version": "susi.desired_state/v1",
        "models": [{"id": "cold-one", "kind": "warm"}],
        "providers": [{"id": "openai:gpt-x", "kind": "live"}],
        "policy": {"estate": {"act": ["warm_model"], "hold": ["rotate_key"]}}
    }"#,
    );
    let obs = Observed {
        cooled_providers: vec!["openai:gpt-x".to_string()],
        ..Observed::default()
    };
    let recorder = Recorder::new();
    let report =
        estate_loop::run_pass_with(&home, Some(&d), &obs, 100, &recorder).expect("pass completes");

    // The allowlist kept rotate_key out; the denylist would too — the
    // act was planned, held, and never executed.
    let rotate = report
        .receipts
        .iter()
        .find(|r| r.action.kind == ActionKind::RotateKey)
        .expect("rotation was planned");
    assert!(matches!(rotate.verdict, Verdict::Held(_)), "{rotate:?}");
    let warm = report
        .receipts
        .iter()
        .find(|r| r.action.kind == ActionKind::WarmModel)
        .expect("warm was planned");
    assert!(matches!(warm.verdict, Verdict::Applied(_)), "{warm:?}");
    assert_eq!(recorder.taken(), vec!["warm_model:cold-one".to_string()]);

    // A report-only estate plans and audits everything, applies nothing.
    let mut report_only = d.clone();
    report_only.policy.insert(
        "estate".to_string(),
        serde_json::json!({"autonomy": "report"}),
    );
    let home2 = scratch("report-only");
    let recorder2 = Recorder::new();
    let report = estate_loop::run_pass_with(&home2, Some(&report_only), &obs, 200, &recorder2)
        .expect("pass completes");
    assert!(recorder2.taken().is_empty());
    assert!(
        report
            .receipts
            .iter()
            .all(|r| matches!(r.verdict, Verdict::Held(_)))
    );
}

#[test]
fn estate_loop_unattended_crash_mid_pass_resumes_not_restarts() {
    let home = scratch("resume");
    // A pass that died after applying its first act: the journal carries
    // the plan and the applied id.
    let state = LoopState {
        in_flight: Some(estate_loop::InFlight {
            pass: 3,
            started_unix: 500,
            plan: vec![
                Action {
                    kind: ActionKind::WarmModel,
                    target: "a".to_string(),
                    reason: "r".to_string(),
                },
                Action {
                    kind: ActionKind::CollectOrphan,
                    target: "b".to_string(),
                    reason: "r".to_string(),
                },
            ],
            applied: vec!["warm_model:a".to_string()],
        }),
        ..LoopState::default()
    };
    state
        .save(&estate_loop::state_path(&home))
        .expect("journal writes");

    let recorder = Recorder::new();
    let report =
        estate_loop::run_pass_with(&home, Some(&full_desired()), &observed(), 600, &recorder)
            .expect("pass resumes and completes");
    assert!(report.resumed, "the pass reports its resume");
    assert_eq!(report.pass, 3, "the interrupted pass continues");
    // The applied act was not re-run — only the unapplied one remained.
    assert_eq!(recorder.taken().len(), 1);
    assert_eq!(report.receipts.len(), 1);
    assert_eq!(report.receipts[0].action.id(), "collect_orphan:b");

    // The journal shows the pass completed, history recorded it, and no
    // pass is left in flight.
    let state = LoopState::load(&estate_loop::state_path(&home));
    assert!(state.in_flight.is_none());
    assert_eq!(state.last_completed, 3);
    assert_eq!(state.history.len(), 1);
    assert_eq!(state.history[0].pass, 3);
}

#[test]
fn estate_loop_unattended_every_pass_and_action_is_signed_into_audit() {
    let home = scratch("audit");
    let report = estate_loop::run_pass_with(
        &home,
        Some(&full_desired()),
        &observed(),
        42,
        &Recorder::new(),
    )
    .expect("pass completes");

    // The signed chain verifies: one ESTATE_ACTION per act plus one
    // ESTATE_PASS — a tampered file would fail verify_chain outright.
    let entries = crate::susi_sandbox::audit_chain::verify_chain(&estate_loop::audit_path(&home))
        .expect("audit chain verifies");
    assert_eq!(entries, report.receipts.len() + 1);

    // The journal round-trips through JSON.
    let state = LoopState::load(&estate_loop::state_path(&home));
    let summary = &state.history[0];
    assert_eq!(summary.planned, report.receipts.len());
    assert_eq!(summary.applied, report.receipts.len());
    assert_eq!(summary.failed, 0);
}

#[test]
fn estate_loop_unattended_no_desired_state_plans_nothing_but_audits() {
    let home = scratch("no-desired");
    // The production entry: no desired-state.json — nothing is managed
    // (an absent declaration is not consent), but the pass is still
    // journaled and audited.
    let report = estate_loop::run_pass(&home, 7).expect("empty pass completes");
    assert!(report.receipts.is_empty());
    let state = LoopState::load(&estate_loop::state_path(&home));
    assert_eq!(state.last_completed, 1);
    let entries = crate::susi_sandbox::audit_chain::verify_chain(&estate_loop::audit_path(&home))
        .expect("audit chain verifies");
    assert_eq!(entries, 1, "one ESTATE_PASS entry, no actions");
}

#[test]
fn estate_loop_unattended_failed_acts_are_recorded_not_retried_same_pass() {
    let home = scratch("failures");
    let d = desired(
        r#"{
        "schema_version": "susi.desired_state/v1",
        "models": [{"id": "ghost", "kind": "warm"}]
    }"#,
    );
    let recorder = Recorder::failing(&["warm_model:ghost"]);
    let report = estate_loop::run_pass_with(&home, Some(&d), &Observed::default(), 10, &recorder)
        .expect("pass completes despite a refused act");
    let receipt = &report.receipts[0];
    assert!(matches!(receipt.verdict, Verdict::Failed(_)), "{receipt:?}");
    // One attempt only — a failed act is evidence, not a retry loop.
    assert_eq!(recorder.taken().len(), 1);
    // The pass still completed and journaled its outcome.
    assert_eq!(report.summary().failed, 1);
}

#[test]
fn estate_loop_unattended_production_actuator_and_tick_wiring() {
    // SystemActuate's guards are real: unknown runtimes are held, never
    // signalled; a dead credential rotates away through the arbiter's
    // quarantine, which the test build persists to a per-pid temp file.
    let actuator = estate_loop::SystemActuate;
    let held = actuator.act(&Action {
        kind: ActionKind::StartRuntime,
        target: "not-a-leaf-service".to_string(),
        reason: "t".to_string(),
    });
    assert!(matches!(held, Verdict::Held(_)), "{held:?}");

    let rotated = actuator.act(&Action {
        kind: ActionKind::RotateKey,
        target: "estate-test-vendor".to_string(),
        reason: "t".to_string(),
    });
    assert!(matches!(rotated, Verdict::Applied(_)), "{rotated:?}");

    // The unattended wiring: the maintenance tick schedules the estate
    // pass through the probe scheduler, and run_pass drives observe() →
    // plan → act through the seams named here.
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/runtime_admin.rs"))
        .expect("runtime_admin source");
    assert!(src.contains("estate_reconcile"), "tick schedules the pass");
    assert!(
        src.contains("estate_loop::run_pass"),
        "tick drives the loop"
    );
    let loop_src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/estate_loop.rs"))
            .expect("estate_loop source");
    for seam in [
        "InferenceHost::preload",
        "InferenceHost::unload",
        "record_vendor_failure",
        "service_table::update_with",
        "loaded_models",
        "cooled_providers",
        "append_signed_entry",
    ] {
        assert!(loop_src.contains(seam), "missing production seam {seam}");
    }
}
