//! T-DEEPSEEK-161 (VC-202-024): autonomy tiers — reversible low-blast
//! changes are automatic, costly or irreversible ones are rehearsed
//! (dry-run) or gated on a durable one-shot operator grant (ask), and
//! every tier's decision lands on the receipt and the audit.

use std::sync::Mutex;

use crate::estate_loop::{self, Action, Actuate, Observed, ServiceObs, Verdict};
use crate::susi_config::desired_state::{DesiredState, parse_desired_state};

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("susi-estate-tiers-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn desired(json: &str) -> DesiredState {
    parse_desired_state(json).expect("desired document parses")
}

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

/// One warm-but-unloaded model, one up-but-declared-stopped runtime and
/// one cooled-but-declared-live provider: the plan holds a low-blast
/// warm plus two high-blast asks.
fn observed() -> Observed {
    Observed {
        loaded_models: vec![],
        services: vec![ServiceObs {
            name: "susi-config".to_string(),
            up: true,
            stopped: false,
            external: false,
        }],
        cooled_providers: vec!["openai:gpt-x".to_string()],
    }
}

fn policy_desired(estate: &str) -> DesiredState {
    desired(&format!(
        r#"{{
        "schema_version": "susi.desired_state/v1",
        "models": [{{"id": "kept-model", "kind": "warm"}}],
        "runtimes": [{{"id": "susi-config", "kind": "stopped"}}],
        "providers": [{{"id": "openai:gpt-x", "kind": "live"}}],
        "policy": {{"estate": {estate}}}
    }}"#
    ))
}

fn receipt<'a>(
    report: &'a crate::estate_loop::PassReport,
    id: &str,
) -> &'a crate::estate_loop::ActionReceipt {
    report
        .receipts
        .iter()
        .find(|r| r.action.id() == id)
        .unwrap_or_else(|| panic!("receipt for {id}: {:?}", report.receipts))
}

#[test]
fn estate_autonomy_tiers_low_blast_acts_high_blast_asks_by_default() {
    let home = scratch("default-tiers");
    let recorder = Recorder::new();
    let report = estate_loop::run_pass_with(
        &home,
        Some(&policy_desired("{}")),
        &observed(),
        1_000,
        &recorder,
    )
    .expect("pass completes");

    // Low blast: warm a declared model — automatic.
    assert!(
        matches!(
            receipt(&report, "warm_model:kept-model").verdict,
            Verdict::Applied(_)
        ),
        "low-blast warm acts by default"
    );
    // High blast: drain a running service, move a credential — ask.
    for id in ["drain_runtime:susi-config", "rotate_key:openai:gpt-x"] {
        assert!(
            matches!(receipt(&report, id).verdict, Verdict::AwaitingApproval(_)),
            "{id} waits on a grant by default"
        );
    }
    assert_eq!(
        recorder.taken(),
        vec!["warm_model:kept-model".to_string()],
        "only the low-blast act reached the seam"
    );
    let s = report.summary();
    assert_eq!(s.awaiting, 2);
    assert_eq!(s.applied, 1);
}

#[test]
fn estate_autonomy_tiers_grant_actuates_once_and_is_consumed() {
    let home = scratch("one-shot");
    let grant =
        estate_loop::approve(&home, "drain_runtime:susi-config", 900).expect("grant writes");
    assert!(grant.exists());

    let recorder = Recorder::new();
    let report = estate_loop::run_pass_with(
        &home,
        Some(&policy_desired("{}")),
        &observed(),
        1_000,
        &recorder,
    )
    .expect("first pass completes");
    assert!(
        matches!(
            receipt(&report, "drain_runtime:susi-config").verdict,
            Verdict::Approved(_)
        ),
        "granted act runs as approved"
    );
    assert!(
        !grant.exists(),
        "the grant is consumed — a crash can never re-run on a stale approval"
    );
    let audit = std::fs::read_to_string(estate_loop::audit_path(&home)).expect("audit log");
    assert!(
        audit.contains("ESTATE_APPROVAL"),
        "the consumed grant is signed evidence"
    );

    // The next pass re-plans the same drift — with no second grant it
    // asks again rather than acting twice on one approval.
    let report = estate_loop::run_pass_with(
        &home,
        Some(&policy_desired("{}")),
        &observed(),
        2_000,
        &recorder,
    )
    .expect("second pass completes");
    assert!(
        matches!(
            receipt(&report, "drain_runtime:susi-config").verdict,
            Verdict::AwaitingApproval(_)
        ),
        "a consumed grant does not carry over"
    );
    assert_eq!(
        recorder
            .taken()
            .iter()
            .filter(|c| *c == "drain_runtime:susi-config")
            .count(),
        1,
        "the costly act ran exactly once across passes"
    );
}

#[test]
fn estate_autonomy_tiers_dry_run_rehearses_without_actuating() {
    let home = scratch("dry-run");
    let recorder = Recorder::new();
    let report = estate_loop::run_pass_with(
        &home,
        Some(&policy_desired(
            r#"{"dry_run": ["warm_model", "drain_runtime", "rotate_key"]}"#,
        )),
        &observed(),
        1_000,
        &recorder,
    )
    .expect("pass completes");

    assert!(
        report
            .receipts
            .iter()
            .all(|r| matches!(r.verdict, Verdict::DryRun(_))),
        "every act rehearsed: {:?}",
        report.receipts
    );
    assert!(
        recorder.taken().is_empty(),
        "a rehearsal never reaches the seam"
    );
    let audit = std::fs::read_to_string(estate_loop::audit_path(&home)).expect("audit log");
    assert!(
        audit.contains("dry-run"),
        "the rehearsal is signed evidence: {audit}"
    );
}

#[test]
fn estate_autonomy_tiers_ask_names_the_grant_an_operator_writes() {
    let home = scratch("awaiting");
    let report = estate_loop::run_pass_with(
        &home,
        Some(&policy_desired(r#"{"ask": ["warm_model"]}"#)),
        &observed(),
        1_000,
        &Recorder::new(),
    )
    .expect("pass completes");

    let Verdict::AwaitingApproval(detail) = &receipt(&report, "warm_model:kept-model").verdict
    else {
        panic!("expected awaiting: {:?}", report.receipts)
    };
    assert!(
        detail.contains("estate-approvals") && detail.contains("warm_model_kept-model"),
        "the receipt names the grant file to write: {detail}"
    );
}

#[test]
fn estate_autonomy_tiers_hold_beats_a_grant_and_report_beats_all() {
    let home = scratch("deny-wins");
    estate_loop::approve(&home, "drain_runtime:susi-config", 900).expect("grant writes");
    let grant = estate_loop::approvals_dir(&home).join("drain_runtime_susi-config.json");
    assert!(grant.exists());

    let recorder = Recorder::new();
    let report = estate_loop::run_pass_with(
        &home,
        Some(&policy_desired(r#"{"hold": ["drain_runtime"]}"#)),
        &observed(),
        1_000,
        &recorder,
    )
    .expect("pass completes");
    assert!(
        matches!(
            receipt(&report, "drain_runtime:susi-config").verdict,
            Verdict::Held(_)
        ),
        "an explicit hold beats a grant"
    );
    assert!(
        grant.exists(),
        "a denied act must not consume the operator's grant"
    );
    assert_eq!(
        recorder.taken(),
        vec!["warm_model:kept-model".to_string()],
        "the held drain and the ungranted rotate never reached the seam"
    );

    let report = estate_loop::run_pass_with(
        &home,
        Some(&policy_desired(r#"{"autonomy": "report"}"#)),
        &observed(),
        2_000,
        &recorder,
    )
    .expect("report-mode pass completes");
    assert!(
        report
            .receipts
            .iter()
            .all(|r| matches!(r.verdict, Verdict::Held(_))),
        "report tier holds every act"
    );
    assert_eq!(
        recorder.taken().len(),
        1,
        "the report pass actuated nothing new"
    );
}

#[test]
fn estate_autonomy_tiers_failed_approved_act_re_arms_the_grant() {
    let home = scratch("re-arm");
    estate_loop::approve(&home, "drain_runtime:susi-config", 900).expect("grant writes");
    let grant = estate_loop::approvals_dir(&home).join("drain_runtime_susi-config.json");

    let recorder = Recorder::failing(&["drain_runtime:susi-config"]);
    let report = estate_loop::run_pass_with(
        &home,
        Some(&policy_desired("{}")),
        &observed(),
        1_000,
        &recorder,
    )
    .expect("pass completes");
    assert!(
        matches!(
            receipt(&report, "drain_runtime:susi-config").verdict,
            Verdict::Failed(_)
        ),
        "the seam's refusal is recorded"
    );
    assert!(
        grant.exists(),
        "a refused act never landed — the grant is re-armed, not burned"
    );
}

#[test]
fn estate_autonomy_tiers_production_wiring() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/estate_loop.rs"))
        .expect("estate_loop source");
    for seam in [
        "fn tier_of",
        "Tier::DryRun",
        "Tier::Ask",
        "consume_grant",
        "pub fn approve",
        "ESTATE_APPROVAL",
        "Blast::High",
        "kind.blast()",
    ] {
        assert!(src.contains(seam), "missing tier wiring {seam}");
    }
}
