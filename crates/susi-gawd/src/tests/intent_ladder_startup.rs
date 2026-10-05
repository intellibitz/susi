//! T-DEEPSEEK-200 / VC-201-001: a graded user-intent ladder, run at
//! start-up within a budget.
//!
//! The contract: a versioned `.agents/intent-ladder.json` of real intents
//! from trivial to extreme; a bounded slice after susi starts and the
//! full ladder on demand and on a schedule; every intent hermetic —
//! throwaway workspace, scratch SUSI_HOME, no live estate, no egress;
//! and a reproducible scorecard: pass/fail/partial, cost, worker, and
//! where it stopped.

use crate::intent_ladder::{
    self, Budget, Check, Grade, Intent, IntentLadder, RunDirs, RunEnv, RunOutcome, Runner,
    SlicePolicy,
};
use std::path::PathBuf;

fn tmp(tag: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("susi-intent-ladder-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn intent(id: &str, grade: &str, seconds: u64) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "grade": grade,
        "intent": format!("do the {id} thing"),
        "check": { "kind": "file_contains", "path": format!("{id}.txt"), "needle": "done" },
        "budget": { "seconds": seconds, "micros": 0 }
    })
}

fn ladder_json(intents: Vec<serde_json::Value>) -> String {
    serde_json::json!({
        "schema": "susi.intent_ladder/v1",
        "revision": 1,
        "authored_by": "test",
        "rationale": "test ladder",
        "intents": intents,
    })
    .to_string()
}

fn ladder(intents: Vec<serde_json::Value>) -> IntentLadder {
    IntentLadder::from_json(&ladder_json(intents)).unwrap()
}

type OutcomeFn = Box<dyn Fn(&Intent, &RunEnv) -> RunOutcome>;

struct StubRunner {
    f: OutcomeFn,
}

impl Runner for StubRunner {
    fn run(&self, intent: &Intent, env: &RunEnv, _budget: Budget) -> RunOutcome {
        (self.f)(intent, env)
    }
}

fn outcome(exit_ok: bool, output: &str, seconds: u64, micros: u64) -> RunOutcome {
    RunOutcome {
        exit_ok,
        output: output.into(),
        seconds,
        micros,
        worker: "stub-worker".into(),
        timed_out: false,
    }
}

#[test]
fn intent_ladder_startup_slice_is_bounded_and_hermetic() {
    let root = tmp("slice");
    let ladder = ladder(vec![
        intent("a", "trivial", 10),
        intent("b", "easy", 10),
        intent("c", "moderate", 10), // above the slice's grade ceiling
        intent("d", "easy", 10),
        intent("e", "trivial", 10), // past the slice's count ceiling
    ]);
    let seen = std::sync::Arc::new(std::sync::Mutex::new(
        Vec::<(String, PathBuf, PathBuf)>::new(),
    ));
    let seen_in = seen.clone();
    let runner = StubRunner {
        f: Box::new(move |i: &Intent, env: &RunEnv| {
            // The hermetic contract, asserted from inside the run: scratch
            // home and workspace are fresh, empty, and not the state dir.
            assert!(env.home.exists() && env.workspace.exists());
            assert!(std::fs::read_dir(&env.home).unwrap().next().is_none());
            std::fs::write(env.workspace.join(format!("{}.txt", i.id)), "done").unwrap();
            seen_in.lock().unwrap_or_else(|e| e.into_inner()).push((
                i.id.clone(),
                env.home.clone(),
                env.workspace.clone(),
            ));
            outcome(true, "", 1, 5)
        }),
    };
    let card = intent_ladder::run_slice(
        &RunDirs {
            state_dir: &root.join("state"),
            runs_root: &root.join("runs"),
        },
        &ladder,
        "test",
        &SlicePolicy::startup(),
        &runner,
    );
    let seen = seen.lock().unwrap_or_else(|e| e.into_inner());
    // Bounded: SLICE_MAX_INTENTS rungs, and nothing above the slice grade.
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0].0, "a");
    assert_eq!(seen[1].0, "b");
    assert_eq!(seen[2].0, "d");
    // Hermetic: distinct scratch dirs per rung, none the state dir, all
    // thrown away after the rung.
    for (_id, home, ws) in seen.iter() {
        assert!(home.starts_with(root.join("runs")) && ws.starts_with(root.join("runs")));
        assert!(!home.exists() && !ws.exists(), "throwaway state was kept");
    }
    assert_eq!(card.entries.len(), 3);
    assert!(card.entries.iter().all(|e| e.verdict == "pass"));
}

#[test]
fn intent_ladder_startup_scorecard_records_verdict_worker_cost_and_stop() {
    let root = tmp("card");
    let ladder = ladder(vec![
        intent("passes", "trivial", 10),
        intent("times-out", "trivial", 10),
        intent("fails", "trivial", 10),
    ]);
    let runner = StubRunner {
        f: Box::new(|i: &Intent, env: &RunEnv| {
            match i.id.as_str() {
                "passes" => {
                    std::fs::write(env.workspace.join("passes.txt"), "done").unwrap();
                    outcome(true, "", 3, 42)
                }
                "times-out" => RunOutcome {
                    timed_out: true,
                    ..outcome(false, "", 10, 7)
                },
                _ => outcome(true, "", 2, 9), // never produced the file
            }
        }),
    };
    let card = intent_ladder::run_slice(
        &RunDirs {
            state_dir: &root.join("state"),
            runs_root: &root.join("runs"),
        },
        &ladder,
        "test",
        &SlicePolicy::full(),
        &runner,
    );
    let by = |id: &str| card.entries.iter().find(|e| e.id == id).unwrap();
    let (p, t, f) = (by("passes"), by("times-out"), by("fails"));
    assert_eq!(p.verdict, "pass");
    assert_eq!(
        (p.seconds, p.micros, p.worker.as_str()),
        (3, 42, "stub-worker")
    );
    assert_eq!(p.stopped, "completed");
    assert_eq!(t.verdict, "partial");
    assert_eq!(t.stopped, "timeout");
    assert_eq!(f.verdict, "fail");
    assert!(f.detail.contains("never produced"), "{}", f.detail);

    // The durable scorecard is the same document — reproducible on disk.
    let on_disk = intent_ladder::read_scorecard(&root.join("state")).unwrap();
    assert_eq!(on_disk, card);
    assert_eq!(on_disk.ladder_revision, 1);
    assert_eq!(on_disk.schema, intent_ladder::SCORECARD_SCHEMA);
}

#[test]
fn intent_ladder_startup_slice_ceiling_skips_later_rungs() {
    let root = tmp("ceiling");
    let ladder = ladder(vec![
        intent("first", "trivial", 10),
        intent("second", "trivial", 10),
    ]);
    let runner = StubRunner {
        f: Box::new(|_i: &Intent, _env: &RunEnv| outcome(true, "", 1, 100)),
    };
    let policy = SlicePolicy {
        total_micros: 1, // the first rung alone spends past it
        ..SlicePolicy::full()
    };
    let card = intent_ladder::run_slice(
        &RunDirs {
            state_dir: &root.join("state"),
            runs_root: &root.join("runs"),
        },
        &ladder,
        "test",
        &policy,
        &runner,
    );
    assert_eq!(card.entries[0].verdict, "fail"); // no file written
    assert_eq!(card.entries[1].verdict, "skipped");
    assert_eq!(card.entries[1].stopped, "slice-budget");
    assert_eq!(card.entries[1].worker, "");
}

#[test]
fn intent_ladder_startup_verification_rejects_a_bad_ladder() {
    // Bad schema.
    assert!(IntentLadder::from_json(&ladder_json(vec![]).replacen(
        "susi.intent_ladder/v1",
        "other",
        1
    ))
    .is_err());
    for (tag, intents) in [
        (
            "duplicate ids",
            vec![intent("x", "trivial", 10), intent("x", "easy", 10)],
        ),
        ("unsafe check path `..`", {
            let mut i = intent("x", "trivial", 10);
            i["check"]["path"] = serde_json::json!("../escape.txt");
            vec![i]
        }),
        ("absolute check path", {
            let mut i = intent("x", "trivial", 10);
            i["check"]["path"] = serde_json::json!("/etc/passwd");
            vec![i]
        }),
        ("hidden check path", {
            let mut i = intent("x", "trivial", 10);
            i["check"]["path"] = serde_json::json!(".git/config");
            vec![i]
        }),
        ("zero wall-clock budget", vec![intent("x", "trivial", 0)]),
        ("empty intent text", {
            let mut i = intent("x", "trivial", 10);
            i["intent"] = serde_json::json!("   ");
            vec![i]
        }),
    ] {
        let err = IntentLadder::from_json(&ladder_json(intents))
            .expect_err(&format!("{tag} must be refused"));
        assert!(err.to_string().len() > 8, "{tag}: {err}");
    }
    // And a rationale-less ladder — an authored artifact must name its analysis.
    let mut doc: serde_json::Value = serde_json::from_str(&ladder_json(vec![])).unwrap();
    doc["rationale"] = serde_json::json!(" ");
    assert!(IntentLadder::from_json(&doc.to_string()).is_err());
}

#[test]
fn intent_ladder_startup_check_is_independent_of_the_runner() {
    let root = tmp("independent");
    let ladder = ladder(vec![
        intent("claims-only", "trivial", 10),
        intent("claims-and-proves", "trivial", 10),
    ]);
    let runner = StubRunner {
        f: Box::new(|i: &Intent, env: &RunEnv| {
            if i.id == "claims-and-proves" {
                std::fs::write(env.workspace.join("claims-and-proves.txt"), "done").unwrap();
            }
            // Both claim exit success — only the one that left evidence passes.
            outcome(true, "", 1, 0)
        }),
    };
    let card = intent_ladder::run_slice(
        &RunDirs {
            state_dir: &root.join("state"),
            runs_root: &root.join("runs"),
        },
        &ladder,
        "test",
        &SlicePolicy::startup(),
        &runner,
    );
    assert_eq!(card.entries[0].verdict, "fail");
    assert_eq!(card.entries[1].verdict, "pass");
}

#[test]
fn intent_ladder_startup_process_runner_env_is_hermetic() {
    // A stub binary that dumps its environment to stdout — ProcessRunner
    // captures it, so the scrub is asserted end to end.
    let root = tmp("proc");
    let script = root.join("env-dump.sh");
    std::fs::write(&script, "#!/bin/sh\nenv\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // A credential-shaped variable in OUR env must not survive into the run.
    std::env::set_var("SUSI_LADDER_SECRET_LEAK", "topsecret");

    let runner = intent_ladder::ProcessRunner { susi_bin: script };
    let runs = root.join("runs");
    let env = RunEnv {
        home: runs.join("h"),
        workspace: runs.join("w"),
    };
    std::fs::create_dir_all(&env.home).unwrap();
    std::fs::create_dir_all(&env.workspace).unwrap();
    let out = runner.run(
        &Intent {
            id: "t".into(),
            grade: Grade::Trivial,
            intent: "anything".into(),
            check: Check::StdoutMarker {
                needle: "SUSI_OFFLINE".into(),
            },
            budget: Budget {
                seconds: 30,
                micros: 0,
            },
        },
        &env,
        Budget {
            seconds: 30,
            micros: 0,
        },
    );
    std::env::remove_var("SUSI_LADDER_SECRET_LEAK");
    assert!(out.exit_ok, "spawn failed: {}", out.output);
    assert!(
        out.output.contains("SUSI_OFFLINE=1"),
        "egress gate missing: {}",
        out.output
    );
    assert!(
        out.output
            .contains(&format!("SUSI_HOME={}", env.home.display())),
        "scratch home not set: {}",
        out.output
    );
    assert!(
        !out.output.contains("SUSI_LADDER_SECRET_LEAK"),
        "a credential leaked into the hermetic env: {}",
        out.output
    );
    assert!(out.worker.contains("env-dump.sh@"));
}

#[test]
fn intent_ladder_startup_wiring_is_on_the_production_path() {
    // The bundled ladder is real: loads, is graded, covers the span.
    let bundled = IntentLadder::bundled();
    assert_eq!(bundled.schema, intent_ladder::LADDER_SCHEMA);
    assert!(bundled.revision >= 1);
    let grades: Vec<Grade> = bundled.intents.iter().map(|i| i.grade).collect();
    assert_eq!(grades.first(), Some(&Grade::Trivial));
    assert_eq!(grades.last(), Some(&Grade::Extreme));

    // Boot slice + scheduled full run + on-demand CLI, all through the
    // same seams.
    let admin = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../susi-daemon/src/runtime_admin.rs"
    ))
    .unwrap();
    for needle in [
        "intent_ladder::startup_slice",
        "intent_ladder::full_run",
        "intent_ladder::schedule_secs()",
        "\"intent_ladder\"",
    ] {
        assert!(
            admin.contains(needle),
            "runtime_admin.rs is missing `{needle}`"
        );
    }
    let defs = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/cli/defs.rs"
    ))
    .unwrap();
    assert!(defs.contains("IntentLadder"));
    let cli = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/cli/admin_cli.rs"
    ))
    .unwrap();
    for needle in [
        "AdminCommands::IntentLadder",
        "intent_ladder::full_run",
        "intent_ladder::read_scorecard",
    ] {
        assert!(cli.contains(needle), "admin_cli.rs is missing `{needle}`");
    }
}
