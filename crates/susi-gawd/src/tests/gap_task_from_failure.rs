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
    // The daemon records gaps after both scheduled run kinds — into the
    // host outbox, never as task files in whatever directory it runs in.
    let admin = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../susi-daemon/src/runtime_admin.rs"
    ))
    .unwrap();
    assert!(
        admin.matches("intent_ladder::record_gaps").count() >= 2,
        "runtime_admin.rs must record gaps after the slice and the full run"
    );
    for forbidden in ["intent_ladder::gap_tasks", "repo_root()"] {
        assert!(
            !admin.contains(forbidden),
            "runtime_admin.rs must not use `{forbidden}`: the daemon has no repository"
        );
    }
    // Every daemon spawn runs in the substrate home, not the caller's cwd.
    let server = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../susi-daemon/src/server.rs"
    ))
    .unwrap();
    assert!(server.contains("cmd.current_dir(substrate_home)"));
    assert!(server.contains("std::env::set_current_dir(&workspace)"));
    // Not through systemd-run's `--working-directory=` (systemd >= 240
    // only): an older systemd rejects the call and every start falls back
    // to the uncapped double-fork. The daemon's own set_current_dir covers
    // the unit.
    assert!(!server.contains(".arg(format!(\"--working-directory="));
    // An agent adopts a gap through a real CLI arm.
    let admin_cli = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/cli/admin_cli.rs"
    ))
    .unwrap();
    assert!(admin_cli.contains("intent_ladder::adopt_gap("));
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

/// A susi home whose ladder override is the test ladder — the daemon
/// resolves its ladder from the home it owns.
fn home(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("susi-gap-home-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".agents")).unwrap();
    std::fs::write(home.join(".agents/intent-ladder.json"), LADDER).unwrap();
    home
}

fn snapshot(dir: &std::path::Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(p.clone(), std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

/// The daemon's share of VC-201-012 writes the host outbox and nothing
/// else: a repository next to it — an agent's worktree the daemon was
/// spawned from — stays byte-identical, and no task file appears anywhere.
#[test]
fn gap_outbox_records_failures_without_touching_any_repo() {
    let repo = ws("outbox-repo");
    let before = snapshot(&repo);
    let home = home("outbox");
    let card = card(vec![
        entry("trivial.a", "trivial", "fail", "a.txt was never produced"),
        entry("extreme.z", "extreme", "pass", "ok"),
    ]);
    let outbox = intent_ladder::record_gaps(&home, &card).unwrap();
    assert_eq!(snapshot(&repo), before, "the repo is untouched");
    assert!(
        !home.join(".agents/tasks").exists(),
        "no task file is authored"
    );
    assert_eq!(outbox.schema, intent_ladder::GAPS_SCHEMA);
    assert_eq!(outbox.gaps.len(), 1);
    let gap = &outbox.gaps[0];
    assert_eq!(gap.intent, "trivial.a");
    assert_eq!(gap.grade, "trivial");
    assert_eq!(gap.expected, "a file `a.txt` containing `done`");
    assert_eq!(gap.detail, "a.txt was never produced");
    assert_eq!(gap.failures, 1);
    assert_eq!(gap.adopted, None);
    assert_eq!(
        gap.adopt_command(),
        "susi admin intent-ladder --adopt-gap trivial.a"
    );
    // What the daemon wrote is what every worktree reads back.
    assert_eq!(intent_ladder::read_gaps(&home), outbox);
    assert!(intent_ladder::gaps_path(&home).is_file());
}

/// A rung a later ladder revision renamed or dropped is never run again, so
/// it can never pass and heal: its gap stayed pending in every worktree's
/// task list for good. The next record drops it.
#[test]
fn a_gap_for_a_rung_the_ladder_no_longer_has_is_dropped() {
    let home = home("off-ladder");
    intent_ladder::record_gaps(
        &home,
        &card(vec![
            entry("trivial.a", "trivial", "fail", "a.txt was never produced"),
            entry("hard.h", "hard", "fail", "h.txt was never produced"),
        ]),
    )
    .unwrap();
    assert_eq!(intent_ladder::read_gaps(&home).gaps.len(), 2);
    // The next revision drops `trivial.a`.
    let mut ladder: serde_json::Value = serde_json::from_str(LADDER).unwrap();
    ladder["revision"] = 4.into();
    ladder["intents"]
        .as_array_mut()
        .unwrap()
        .retain(|i| i["id"] != "trivial.a");
    std::fs::write(
        home.join(".agents/intent-ladder.json"),
        serde_json::to_string(&ladder).unwrap(),
    )
    .unwrap();
    // A partial card that names neither rung still prunes the dropped one.
    let outbox = intent_ladder::record_gaps(
        &home,
        &card(vec![entry("extreme.z", "extreme", "pass", "ok")]),
    )
    .unwrap();
    let kept: Vec<&str> = outbox.gaps.iter().map(|g| g.intent.as_str()).collect();
    assert_eq!(kept, ["hard.h"], "only the rung still on the ladder stays");
}

#[test]
fn gap_outbox_refreshes_triages_bounds_and_heals() {
    let home = home("heal");
    let first = intent_ladder::record_gaps(
        &home,
        &card(vec![entry("trivial.a", "trivial", "fail", "first")]),
    )
    .unwrap();
    let seen = first.gaps[0].first_seen_unix;
    // The same rung failing again refreshes its evidence, never duplicates.
    let again = intent_ladder::record_gaps(
        &home,
        &card(vec![entry("trivial.a", "trivial", "partial", "second")]),
    )
    .unwrap();
    assert_eq!(again.gaps.len(), 1);
    assert_eq!(again.gaps[0].failures, 2);
    assert_eq!(again.gaps[0].detail, "second");
    assert_eq!(again.gaps[0].verdict, "partial");
    assert_eq!(again.gaps[0].first_seen_unix, seen);
    // New failures enter by grade, at most GAP_TASKS_PER_RUN per scorecard.
    let wide = intent_ladder::record_gaps(
        &home,
        &card(vec![
            entry("trivial.a", "trivial", "fail", "third"),
            entry("moderate.m", "moderate", "fail", "never produced"),
            entry("hard.h", "hard", "partial", "hit its budget"),
            entry("extreme.z", "extreme", "fail", "marker absent"),
        ]),
    )
    .unwrap();
    let ids: Vec<&str> = wide.gaps.iter().map(|g| g.intent.as_str()).collect();
    assert_eq!(intent_ladder::GAP_TASKS_PER_RUN, 2);
    assert_eq!(ids, vec!["extreme.z", "hard.h", "trivial.a"]);
    // A rung that passes leaves the outbox; skipped rungs change nothing.
    let healed = intent_ladder::record_gaps(
        &home,
        &card(vec![
            entry("trivial.a", "trivial", "pass", "ok"),
            entry("hard.h", "hard", "skipped", ""),
        ]),
    )
    .unwrap();
    let ids: Vec<&str> = healed.gaps.iter().map(|g| g.intent.as_str()).collect();
    assert_eq!(ids, vec!["extreme.z", "hard.h"]);
}

fn failing_m() -> Scorecard {
    card(vec![entry(
        "moderate.m",
        "moderate",
        "fail",
        "m.txt was never produced",
    )])
}

/// Adoption, run by an agent in its own checkout, authors exactly the task
/// the scorecard would have produced — same title, evidence and `--verify`
/// acceptance — marks the gap adopted, and refuses a second adoption.
#[test]
fn adopt_gap_authors_the_same_task_and_marks_it_adopted() {
    let home = home("adopt");
    let failing = failing_m();
    intent_ladder::record_gaps(&home, &failing).unwrap();

    let direct = ws("adopt-direct");
    let report = intent_ladder::gap_tasks(&direct, &failing, "SUSI").unwrap();
    let expected = tasks::list_open(&direct)
        .into_iter()
        .find(|t| t.id == report.tasks[0])
        .unwrap();

    let repo = ws("adopt-repo");
    let adoption = intent_ladder::adopt_gap(&repo, &home, "moderate.m", "SUSI").unwrap();
    assert_eq!(adoption.authored.tasks.len(), 1);
    assert_eq!(adoption.task.as_ref(), adoption.authored.tasks.first());
    let task = tasks::list_open(&repo)
        .into_iter()
        .find(|t| Some(&t.id) == adoption.task.as_ref())
        .unwrap();
    assert_eq!(task.title, expected.title);
    assert_eq!(task.goal, expected.goal);
    assert_eq!(task.accept.cmd, expected.accept.cmd);
    assert_eq!(task.created_by, "SUSI");
    assert!(task.authored);
    assert_eq!(adoption.gap.adopted.as_deref(), Some(task.id.as_str()));
    assert!(adoption.gap.adopted_unix > 0);
    assert_eq!(
        adoption.gap.adopted_in.as_deref(),
        Some(std::fs::canonicalize(&repo).unwrap().as_path())
    );

    let outbox = intent_ladder::read_gaps(&home);
    assert_eq!(outbox.pending().count(), 0, "an adopted gap is not pending");
    assert!(intent_ladder::adopt_gap(&repo, &home, "moderate.m", "SUSI").is_err());
    assert!(intent_ladder::adopt_gap(&repo, &home, "nope.x", "SUSI").is_err());
    // Still failing: the daemon refreshes the adopted gap, it stays adopted.
    let refreshed = intent_ladder::record_gaps(&home, &failing).unwrap();
    assert_eq!(refreshed.gaps[0].failures, 2);
    assert_eq!(refreshed.gaps[0].adopted, adoption.gap.adopted);
    assert_eq!(refreshed.pending().count(), 0);
}

/// An adoption whose worktree was discarded — its task file never merged —
/// must not hide a rung that keeps failing: the gap is pending again, and
/// another checkout adopts it afresh.
#[test]
fn an_abandoned_adoption_is_offered_again() {
    let home = home("abandoned");
    intent_ladder::record_gaps(&home, &failing_m()).unwrap();
    let discarded = ws("abandoned-a");
    let first = intent_ladder::adopt_gap(&discarded, &home, "moderate.m", "SUSI").unwrap();
    let other = ws("abandoned-b");
    assert!(
        intent_ladder::adopt_gap(&other, &home, "moderate.m", "SUSI").is_err(),
        "a live adoption holds"
    );
    // The worktree goes away with its unmerged task file.
    std::fs::remove_dir_all(&discarded).unwrap();
    let outbox = intent_ladder::read_gaps(&home);
    let pending: Vec<&str> = outbox.pending().map(|g| g.intent.as_str()).collect();
    assert_eq!(pending, vec!["moderate.m"], "the rung is offered again");
    let second = intent_ladder::adopt_gap(&other, &home, "moderate.m", "SUSI").unwrap();
    assert_eq!(second.authored.tasks.len(), 1, "a fresh task is authored");
    assert!(tasks::list_open(&other)
        .iter()
        .any(|t| Some(&t.id) == second.task.as_ref()));
    assert_eq!(second.gap.adopted, second.task);
    assert_ne!(second.gap.adopted_in, first.gap.adopted_in);
    assert_eq!(intent_ladder::read_gaps(&home).pending().count(), 0);
}

/// The adopting checkout dropped the task (deleted, never committed): it
/// may adopt the gap again at once; any other checkout still may not.
#[test]
fn a_task_dropped_in_the_adopting_checkout_frees_the_gap_there() {
    let home = home("dropped");
    intent_ladder::record_gaps(&home, &failing_m()).unwrap();
    let repo = ws("dropped");
    let first = intent_ladder::adopt_gap(&repo, &home, "moderate.m", "SUSI").unwrap();
    let id = first.task.clone().unwrap();
    std::fs::remove_file(repo.join(format!(".agents/tasks/{id}.json"))).unwrap();
    let other = ws("dropped-other");
    assert!(
        intent_ladder::adopt_gap(&other, &home, "moderate.m", "SUSI").is_err(),
        "another checkout cannot see the adopter's checkout, so the adoption holds there"
    );
    let again = intent_ladder::adopt_gap(&repo, &home, "moderate.m", "SUSI").unwrap();
    assert_eq!(again.authored.tasks.len(), 1);
    assert_eq!(again.gap.adopted, again.task);
}

/// Rewrite the outbox's adoption time for `intent` — a test stand-in for
/// the clock moving past the adoption bound.
fn age_adoption(home: &std::path::Path, intent: &str, by_secs: u64) {
    let mut outbox = intent_ladder::read_gaps(home);
    for g in outbox.gaps.iter_mut().filter(|g| g.intent == intent) {
        g.adopted_unix = g.adopted_unix.saturating_sub(by_secs);
    }
    std::fs::write(
        intent_ladder::gaps_path(home),
        serde_json::to_vec_pretty(&outbox).unwrap(),
    )
    .unwrap();
}

/// A worktree that is kept but whose task never merges: past the adoption
/// bound the gap is offered again; a checkout that already holds the
/// adopted task (it merged) re-links to it instead of authoring a
/// duplicate, and one that does not authors a fresh task.
#[test]
fn a_lapsed_adoption_is_relinked_or_adopted_afresh() {
    let home = home("lapsed");
    intent_ladder::record_gaps(&home, &failing_m()).unwrap();
    let a = ws("lapsed-a");
    let first = intent_ladder::adopt_gap(&a, &home, "moderate.m", "SUSI").unwrap();
    let task_id = first.task.clone().unwrap();
    assert!(!intent_ladder::read_gaps(&home).gaps[0]
        .is_pending_at(first.gap.adopted_unix + intent_ladder::GAP_ADOPTION_TTL_SECS - 1));
    age_adoption(&home, "moderate.m", intent_ladder::GAP_ADOPTION_TTL_SECS);
    assert_eq!(intent_ladder::read_gaps(&home).pending().count(), 1);

    // A checkout where the adopted task merged: linked, nothing authored.
    let merged = ws("lapsed-merged");
    std::fs::copy(
        a.join(format!(".agents/tasks/{task_id}.json")),
        merged.join(format!(".agents/tasks/{task_id}.json")),
    )
    .unwrap();
    let linked = intent_ladder::adopt_gap(&merged, &home, "moderate.m", "SUSI").unwrap();
    assert!(linked.authored.tasks.is_empty(), "no duplicate is authored");
    assert_eq!(linked.task.as_deref(), Some(task_id.as_str()));
    assert_eq!(tasks::list_open(&merged).len(), 1);
    assert_eq!(intent_ladder::read_gaps(&home).pending().count(), 0);

    // Lapsed again, adopted from a checkout without the task: authored.
    age_adoption(&home, "moderate.m", intent_ladder::GAP_ADOPTION_TTL_SECS);
    let fresh = ws("lapsed-fresh");
    let afresh = intent_ladder::adopt_gap(&fresh, &home, "moderate.m", "SUSI").unwrap();
    assert_eq!(afresh.authored.tasks.len(), 1);
    assert_eq!(afresh.gap.adopted, afresh.task);
}

/// Close the open task `id` in `ws` the way the done record reads.
fn close_task(ws: &std::path::Path, id: &str, at_unix: u64) {
    let open = ws.join(format!(".agents/tasks/{id}.json"));
    let mut task: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&open).unwrap()).unwrap();
    task["closed"] = serde_json::json!({ "at_unix": at_unix, "commit": "abc1234", "by": "SUSI" });
    let done = ws.join(".agents/tasks/done");
    std::fs::create_dir_all(&done).unwrap();
    std::fs::write(
        done.join(format!("{id}.json")),
        serde_json::to_vec_pretty(&task).unwrap(),
    )
    .unwrap();
    std::fs::remove_file(open).unwrap();
}

/// The adopted task closed, yet the daemon saw the rung fail after the
/// close: a regression after repair is adopted again at once. A gap whose
/// last failure predates the close is the stale record, not a regression.
#[test]
fn a_regression_after_the_adopted_task_closed_is_adopted_again() {
    let home = home("regressed");
    intent_ladder::record_gaps(&home, &failing_m()).unwrap();
    let repo = ws("regressed");
    let first = intent_ladder::adopt_gap(&repo, &home, "moderate.m", "SUSI").unwrap();
    let id = first.task.clone().unwrap();
    let last_failure = first.gap.last_seen_unix;
    close_task(&repo, &id, last_failure);
    assert!(
        intent_ladder::adopt_gap(&repo, &home, "moderate.m", "SUSI").is_err(),
        "no failure seen since the close"
    );
    close_task_again(&repo, &id, last_failure - 1);
    let again = intent_ladder::adopt_gap(&repo, &home, "moderate.m", "SUSI").unwrap();
    assert_eq!(again.authored.tasks.len(), 1);
    assert_ne!(again.task.as_deref(), Some(id.as_str()));
}

/// Re-date an already closed task's close.
fn close_task_again(ws: &std::path::Path, id: &str, at_unix: u64) {
    let done = ws.join(format!(".agents/tasks/done/{id}.json"));
    let mut task: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&done).unwrap()).unwrap();
    task["closed"]["at_unix"] = serde_json::json!(at_unix);
    std::fs::write(done, serde_json::to_vec_pretty(&task).unwrap()).unwrap();
}

/// The daemon's record and a worktree's adoption run concurrently against
/// one outbox: neither loses the other's write — the adoption mark (which
/// keeps a second worktree from adopting) and every refresh both land.
#[test]
fn concurrent_record_and_adopt_lose_nothing() {
    let home = home("concurrent");
    intent_ladder::record_gaps(&home, &failing_m()).unwrap();
    let repo = ws("concurrent");
    const RECORDS: u32 = 40;
    let recorder = {
        let home = home.clone();
        std::thread::spawn(move || {
            for _ in 0..RECORDS {
                intent_ladder::record_gaps(&home, &failing_m()).unwrap();
            }
        })
    };
    let adopter = {
        let (home, repo) = (home.clone(), repo.clone());
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(5));
            intent_ladder::adopt_gap(&repo, &home, "moderate.m", "SUSI").unwrap()
        })
    };
    recorder.join().unwrap();
    let adoption = adopter.join().unwrap();
    let outbox = intent_ladder::read_gaps(&home);
    assert_eq!(outbox.gaps.len(), 1);
    assert_eq!(outbox.gaps[0].failures, 1 + RECORDS, "no refresh was lost");
    assert_eq!(
        outbox.gaps[0].adopted, adoption.task,
        "the adoption mark was not lost"
    );
    assert!(
        !home.join("intent-gaps.lock").exists(),
        "the lock is released"
    );
}
