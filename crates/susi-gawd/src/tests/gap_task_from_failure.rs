//! T-DEEPSEEK-201 / VC-201-012: a ladder failure becomes a task whose
//! acceptance is the failing intent.
//!
//! The contract: every fail/partial scorecard entry authors an
//! evidence-backed task through `tasks::author` — the intent named, its
//! trace, expected against observed — deduplicated against open tasks and
//! vectors, triaged by grade, rate-limited; and the task's acceptance is
//! `susi admin intent-ladder --verify <id>`: closing it requires the
//! intent itself to pass, so the corpus is a growing regression suite.

use crate::admin::tasks;
use crate::intent_ladder::{
    self, Budget, Check, Intent, RunEnv, RunOutcome, Runner, Scorecard, ScorecardEntry,
};
use std::path::PathBuf;

const ROADMAP: &str = r#"{
  "vectors": [
    {"depends_on": [], "id": "VC-900-001", "mastery_target": "m", "priority": "P1",
     "progress": "PARTIAL: seeded", "type": "OPERATIONS", "vector": "existing capability"}
  ]
}"#;

const LADDER: &str = r#"{
  "schema": "susi.intent_ladder/v1",
  "revision": 3,
  "authored_by": "test",
  "rationale": "test ladder",
  "intents": [
    {"id": "trivial.a", "grade": "trivial", "intent": "do a",
     "check": {"kind": "file_contains", "path": "a.txt", "needle": "done"},
     "budget": {"seconds": 10, "micros": 0}},
    {"id": "extreme.z", "grade": "extreme", "intent": "do z",
     "check": {"kind": "stdout_marker", "needle": "Z-OK"},
     "budget": {"seconds": 20, "micros": 0}},
    {"id": "moderate.m", "grade": "moderate", "intent": "do m",
     "check": {"kind": "file_contains", "path": "m.txt", "needle": "done"},
     "budget": {"seconds": 30, "micros": 0}},
    {"id": "hard.h", "grade": "hard", "intent": "do h",
     "check": {"kind": "file_contains", "path": "h.txt", "needle": "done"},
     "budget": {"seconds": 40, "micros": 0}}
  ]
}"#;

fn ws(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("susi-gap-task-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".agents/tasks")).unwrap();
    std::fs::write(root.join(".agents/roadmap.json"), ROADMAP).unwrap();
    std::fs::write(root.join(".agents/intent-ladder.json"), LADDER).unwrap();
    root
}

fn entry(id: &str, grade: &str, verdict: &str, detail: &str) -> ScorecardEntry {
    ScorecardEntry {
        id: id.into(),
        grade: grade.into(),
        verdict: verdict.into(),
        seconds: 7,
        micros: 42,
        worker: "susi@1234".into(),
        stopped: if verdict == "partial" {
            "timeout".into()
        } else {
            "completed".into()
        },
        detail: detail.into(),
    }
}

fn card(entries: Vec<ScorecardEntry>) -> Scorecard {
    Scorecard {
        schema: intent_ladder::SCORECARD_SCHEMA.into(),
        ladder_revision: 3,
        ladder_source: "test".into(),
        full_ladder: true,
        started_unix: 1_700_000_000,
        entries,
    }
}

#[test]
fn gap_task_from_failure_authors_evidence_backed_repair_tasks() {
    let root = ws("author");
    let card = card(vec![
        entry("trivial.a", "trivial", "fail", "a.txt was never produced"),
        entry("extreme.z", "extreme", "pass", "ok"),
    ]);
    let report = intent_ladder::gap_tasks(&root, &card, "SUSI").unwrap();
    assert_eq!(report.tasks.len(), 1);
    let open = tasks::list_open(&root);
    let t = open.iter().find(|t| t.id == report.tasks[0]).unwrap();
    // The task names the intent and carries the evidence.
    assert!(t.title.contains("trivial.a"), "{}", t.title);
    assert!(t.goal.contains("a.txt was never produced"));
    assert!(t
        .goal
        .contains("Expected: a file `a.txt` containing `done`"));
    assert!(t.goal.contains("worker susi@1234"));
    assert!(t.goal.contains("ladder revision 3"));
    // Its acceptance IS the failing intent — closing it re-runs the rung.
    assert_eq!(
        t.accept.cmd,
        vec!["susi", "admin", "intent-ladder", "--verify", "trivial.a"]
    );
    // It lands through the authored path — stamped, bounded, reviewable.
    assert!(t.authored);
    assert_eq!(t.created_by, "SUSI");
}

#[test]
fn gap_task_from_failure_dedups_against_open_work_only() {
    let root = ws("dedup");
    let card = card(vec![entry(
        "trivial.a",
        "trivial",
        "fail",
        "never produced",
    )]);
    intent_ladder::gap_tasks(&root, &card, "SUSI").unwrap();
    // A second pass over the same scorecard authors nothing — the open
    // task already names the intent.
    let report = intent_ladder::gap_tasks(&root, &card, "SUSI").unwrap();
    assert!(report.tasks.is_empty());
    assert_eq!(tasks::list_open(&root).len(), 1);
}

#[test]
fn gap_task_from_failure_triages_by_grade_and_is_rate_limited() {
    let root = ws("triage");
    let card = card(vec![
        entry("trivial.a", "trivial", "fail", "never produced"),
        entry("moderate.m", "moderate", "fail", "never produced"),
        entry("hard.h", "hard", "partial", "hit its budget"),
        entry("extreme.z", "extreme", "fail", "marker absent"),
    ]);
    let report = intent_ladder::gap_tasks(&root, &card, "SUSI").unwrap();
    // Rate-limited per pass, and the highest-impact failures author first.
    assert_eq!(report.tasks.len(), intent_ladder::GAP_TASKS_PER_RUN);
    let open = tasks::list_open(&root);
    let titles: Vec<String> = open.iter().map(|t| t.title.clone()).collect();
    assert!(titles.iter().any(|t| t.contains("extreme.z")));
    assert!(titles.iter().any(|t| t.contains("hard.h")));
    assert!(!titles.iter().any(|t| t.contains("trivial.a")));
}

#[test]
fn gap_task_from_failure_outside_a_repo_is_a_noop() {
    let root = std::env::temp_dir().join(format!("susi-gap-task-norepo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // No .agents/ at all — a daemon cwd that is not a checkout must not
    // invent queue state.
    let report = intent_ladder::gap_tasks(&root, &card(vec![]), "SUSI").unwrap();
    assert!(report.tasks.is_empty());
    assert!(!root.join(".agents").exists());
}

#[test]
fn gap_task_from_failure_verify_rechecks_the_one_intent() {
    struct Maker;
    impl Runner for Maker {
        fn run(&self, i: &Intent, env: &RunEnv, _b: Budget) -> RunOutcome {
            match &i.check {
                Check::FileContains { path, needle } => {
                    std::fs::write(env.workspace.join(path), needle).unwrap();
                }
                Check::StdoutMarker { needle } => {
                    return RunOutcome {
                        exit_ok: true,
                        output: needle.clone(),
                        seconds: 1,
                        micros: 0,
                        worker: "maker".into(),
                        timed_out: false,
                    };
                }
            }
            RunOutcome {
                exit_ok: true,
                output: String::new(),
                seconds: 1,
                micros: 0,
                worker: "maker".into(),
                timed_out: false,
            }
        }
    }
    let root = ws("verify");
    let e = intent_ladder::verify_intent(&root, "moderate.m", &Maker).unwrap();
    assert_eq!(e.verdict, "pass");
    assert_eq!(e.id, "moderate.m");
    assert_eq!(e.grade, "moderate");
    // Unknown rung: a named error, not a silent pass.
    assert!(intent_ladder::verify_intent(&root, "nope.x", &Maker).is_err());
    // The scratch dirs are thrown away by verify too.
    assert!(std::fs::read_dir(std::env::temp_dir().join(format!(
        "susi-intent-verify-moderate.m-{}",
        std::process::id()
    )))
    .is_err());
}

#[test]
fn gap_task_from_failure_wiring_is_on_the_production_path() {
    // The daemon authors gap tasks after both scheduled run kinds.
    let admin = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../susi-daemon/src/runtime_admin.rs"
    ))
    .unwrap();
    assert!(
        admin.matches("intent_ladder::gap_tasks").count() >= 2,
        "runtime_admin.rs must gap-task after the slice and the full run"
    );
    // `--verify` is a real CLI arm, and gap tasks go through the same
    // authored path a human's work meets.
    let defs = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/cli/defs.rs"
    ))
    .unwrap();
    assert!(defs.contains("verify: Option<String>"));
    let cli = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/cli/admin_cli.rs"
    ))
    .unwrap();
    for needle in ["verify_intent(", "entry.verdict != \"pass\""] {
        assert!(cli.contains(needle), "admin_cli.rs is missing `{needle}`");
    }
    let lib = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/intent_ladder.rs"))
        .unwrap();
    for needle in ["tasks::author(", "GAP_TASKS_PER_RUN", "verify_intent"] {
        assert!(
            lib.contains(needle),
            "intent_ladder.rs is missing `{needle}`"
        );
    }
}
