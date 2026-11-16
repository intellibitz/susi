//! A core-only installation can report repository evidence in an empty user home.
#![allow(clippy::unwrap_used)]

use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn scorecard(repo: &Path, home: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_susi"));
    susi_paths::test_env::isolate_home(&mut command, home);
    command
        .env("SUSI_HOME", home.join(".susi"))
        .current_dir(repo)
        .args(["harness", "scorecard"])
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn standalone_scorecard_preserves_empty_and_recorded_results() {
    let fixture = susi_paths::tempfile::tempdir().unwrap();
    let home = fixture.path().join("isolated-home");
    let repo = fixture.path().join("scorecard-λ");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git")
        .current_dir(&repo)
        .args(["init", "-q"])
        .status()
        .unwrap()
        .success());

    let empty = scorecard(&repo, &home, &["--json"]);
    assert!(
        empty.status.success(),
        "{}",
        String::from_utf8_lossy(&empty.stderr)
    );
    let report: Value = serde_json::from_slice(&empty.stdout).unwrap();
    assert_eq!(
        report,
        json!({
            "phases": ["proposed", "implemented", "gated", "merged", "released", "promoted"],
            "tasks": [], "problems": []
        })
    );
    for flags in [
        vec!["--require-complete"],
        vec!["--task", "T-MISSING", "--require-complete"],
    ] {
        let missing = scorecard(&repo, &home, &flags);
        assert!(!missing.status.success());
        assert!(String::from_utf8_lossy(&missing.stderr)
            .contains("scorecard requires all tasks complete"));
    }

    let records = repo.join(".agents/tasks");
    fs::create_dir_all(&records).unwrap();
    fs::write(
        records.join("T-A.json"),
        r#"{"id":"T-A","title":"First","created_unix":10}"#,
    )
    .unwrap();
    let events = repo.join("custom.jsonl");
    fs::write(
        &events,
        r#"{"task":"T-A","phase":"implemented","at_unix":16,"cost":2,"cost_unit":"usd"}"#,
    )
    .unwrap();
    let flags = ["--task", "T-A", "--events", "custom.jsonl"];
    let populated = scorecard(&repo, &home, &[&flags[..], &["--json"]].concat());
    assert!(
        populated.status.success(),
        "{}",
        String::from_utf8_lossy(&populated.stderr)
    );
    let report: Value = serde_json::from_slice(&populated.stdout).unwrap();
    let task = &report["tasks"][0];
    assert_eq!(task["task"], "T-A");
    assert_eq!(task["status"], "in_progress");
    assert_eq!(task["elapsed_seconds"], 6);
    assert_eq!(task["stop_at"], "gated");
    assert_eq!(task["cost"], json!({"value": 2.0, "unit":"usd"}));
    let table = scorecard(&repo, &home, &flags);
    assert!(table.status.success());
    assert!(String::from_utf8_lossy(&table.stdout).contains("in_progress"));
    let incomplete = scorecard(
        &repo,
        &home,
        &[&flags[..], &["--require-complete"]].concat(),
    );
    assert!(!incomplete.status.success());
    assert!(String::from_utf8_lossy(&incomplete.stderr)
        .contains("scorecard requires all tasks complete"));
    fs::write(events, "broken event").unwrap();
    let invalid = scorecard(&repo, &home, &flags);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("scorecard reported 1 problem(s)"));
}
