//! Durable mission state across restart (T-DEEPSEEK-93 / VC-202-011).
//!
//! A mission left mid-DAG by a process death must continue or fail
//! cleanly on restart — never silently restart from the beginning,
//! never lose the record of what it already did, and never wedge on a
//! persisted `Running` node that `dispatch` refuses to re-drive. The
//! resume point must be explicit: recorded on the mission and surfaced
//! to the operator.

use crate::dag::{self, MissionDag};
use crate::mission_persist::{NodeTerminal, PersistedMission, ResumeVerdict};
use crate::mission_resume::{resumable_missions, MissionSummary, NodeView};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use susi_gawd_agents::HighDensityContextStore;

fn scratch_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("susi-durable-mission-state-{tag}-{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// Persisted mission as it looked when the process died: root completed
/// with its output, child left `Running` mid-flight.
fn seed_crash_interrupted(workspace: &Path, goal: &str) -> String {
    let mission_id = PersistedMission::mission_id_for_goal(goal);
    let dir = PersistedMission::missions_dir(workspace);
    let mut dag = MissionDag::new(goal);
    dag.push_node("child", "child work", vec![0]);
    let mut persist = dag.to_persisted(&mission_id);
    persist.nodes.get_mut("n0").expect("n0").state = NodeTerminal::Completed;
    persist.nodes.get_mut("n0").expect("n0").output = Some("root-out".to_string());
    persist.nodes.get_mut("n1").expect("n1").state = NodeTerminal::Running;
    persist.save(&dir).expect("seed save");
    mission_id
}

fn text_of(results: &[(String, String)]) -> String {
    results
        .iter()
        .map(|(_, t)| t.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn durable_mission_state_resumes_interrupted_nodes() {
    let workspace = scratch_dir("resume");
    let goal = "durable mission resume";
    let mission_id = seed_crash_interrupted(&workspace, goal);
    let dir = PersistedMission::missions_dir(&workspace);

    let root_calls = Arc::new(AtomicUsize::new(0));
    let child_calls = Arc::new(AtomicUsize::new(0));
    let gen = {
        let root_calls = Arc::clone(&root_calls);
        let child_calls = Arc::clone(&child_calls);
        Arc::new(move |prompt: &str, _ws: &Path| -> String {
            if prompt.contains("Primary Mission Analysis") {
                root_calls.fetch_add(1, Ordering::SeqCst);
            }
            if prompt.contains("child work") {
                child_calls.fetch_add(1, Ordering::SeqCst);
            }
            "resumed-output".to_string()
        }) as dag::DagModelGenerator
    };
    let tool = Arc::new(|_: &str, _: &serde_json::Value, _: &Path| Ok(String::new()))
        as dag::DagToolExecutor;
    let board = Arc::new(HighDensityContextStore::new(1024));

    let results = dag::dispatch_mission_dag_with_backends(goal, &workspace, &board, gen, tool);
    let text = text_of(&results);

    // The crash-interrupted child re-ran exactly once; the completed root
    // was preserved and never re-dispatched.
    assert_eq!(child_calls.load(Ordering::SeqCst), 1, "{text}");
    assert_eq!(root_calls.load(Ordering::SeqCst), 0, "{text}");

    // The resume point is explicit in the report.
    assert!(text.contains("[MISSION_RESUMED]"), "{text}");
    assert!(text.contains("n1"), "{text}");
    assert!(text.contains("n0"), "{text}");
    assert!(!text.contains("DAG_EXECUTION_FAILED"), "{text}");

    // The durable record keeps the fold, the preserved output, and the
    // now-completed child — the record of what the mission already did.
    let loaded =
        PersistedMission::load(&dir.join(format!("{mission_id}.json"))).expect("reload mission");
    assert_eq!(loaded.interruptions.len(), 1);
    assert_eq!(loaded.interruptions[0].nodes, vec!["n1".to_string()]);
    assert_eq!(loaded.nodes["n0"].state, NodeTerminal::Completed);
    assert_eq!(loaded.nodes["n0"].output.as_deref(), Some("root-out"));
    assert_eq!(loaded.nodes["n1"].state, NodeTerminal::Completed);
    assert!(
        loaded.nodes["n1"]
            .output
            .as_deref()
            .is_some_and(|o| o.contains("resumed-output")),
        "child output must be persisted: {:?}",
        loaded.nodes["n1"].output
    );

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn durable_mission_state_terminal_failure_reports_cleanly() {
    let workspace = scratch_dir("failed");
    let goal = "durable mission failed node";
    let mission_id = PersistedMission::mission_id_for_goal(goal);
    let dir = PersistedMission::missions_dir(&workspace);

    let mut dag = MissionDag::new(goal);
    dag.push_node("child", "child work", vec![0]);
    let mut persist = dag.to_persisted(&mission_id);
    persist.nodes.get_mut("n0").expect("n0").state = NodeTerminal::Completed;
    persist.nodes.get_mut("n1").expect("n1").state = NodeTerminal::Failed;
    persist.nodes.get_mut("n1").expect("n1").output =
        Some("[STALE_FENCE] worker's fence was displaced".to_string());
    persist.save(&dir).expect("seed save");

    let calls = Arc::new(AtomicUsize::new(0));
    let gen = {
        let calls = Arc::clone(&calls);
        Arc::new(move |_: &str, _: &Path| -> String {
            calls.fetch_add(1, Ordering::SeqCst);
            "should-not-run".to_string()
        }) as dag::DagModelGenerator
    };
    let tool = Arc::new(|_: &str, _: &serde_json::Value, _: &Path| Ok(String::new()))
        as dag::DagToolExecutor;
    let board = Arc::new(HighDensityContextStore::new(1024));

    let results = dag::dispatch_mission_dag_with_backends(goal, &workspace, &board, gen, tool);
    let text = text_of(&results);

    // Fails cleanly naming the recorded verdict — no silent retry, no
    // wedge on a dispatch refusal, and nothing re-ran.
    assert!(text.contains("DAG_EXECUTION_FAILED"), "{text}");
    assert!(text.contains("recorded failure"), "{text}");
    assert!(text.contains("n1"), "{text}");
    assert!(text.contains("STALE_FENCE"), "{text}");
    assert!(!text.contains("refuse dispatch"), "{text}");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "{text}");

    // The recorded verdict is preserved, never rewritten.
    let loaded =
        PersistedMission::load(&dir.join(format!("{mission_id}.json"))).expect("reload mission");
    assert_eq!(loaded.nodes["n1"].state, NodeTerminal::Failed);
    assert!(loaded.interruptions.is_empty());

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn durable_mission_state_resume_point_is_explicit() {
    let workspace = scratch_dir("surface");
    let mission_id = seed_crash_interrupted(&workspace, "surface resume goal");

    // A second mission in the same workspace is terminal-failed; a third
    // is fully complete. Only the interrupted one is resumable.
    let dir = PersistedMission::missions_dir(&workspace);
    let mut failed = PersistedMission::new("m-failed".to_string());
    failed.upsert_node(crate::mission_persist::PersistedNode {
        id: "n0".to_string(),
        input: "x".to_string(),
        dependencies: vec![],
        output: Some("[CANCELLED] propagated".to_string()),
        state: NodeTerminal::Failed,
    });
    failed.save(&dir).expect("failed save");
    let mut done = PersistedMission::new("m-done".to_string());
    done.upsert_node(crate::mission_persist::PersistedNode {
        id: "n0".to_string(),
        input: "x".to_string(),
        dependencies: vec![],
        output: Some("ok".to_string()),
        state: NodeTerminal::Completed,
    });
    done.save(&dir).expect("done save");

    // The production enumeration sees exactly the resumable mission and
    // names the unfinished nodes as its resume point.
    let views = resumable_missions(&workspace);
    assert_eq!(views.len(), 1, "{views:?}");
    let view = &views[0];
    assert_eq!(view.mission_id, mission_id);
    assert_eq!(view.summary(), MissionSummary::Resumable);
    assert!(!view.reports_full_success());
    assert_eq!(view.nodes["n0"].view, NodeView::Completed);
    assert!(!view.nodes["n0"].resumable);
    assert_eq!(view.nodes["n1"].view, NodeView::Running);
    assert!(view.nodes["n1"].resumable);
    let line = crate::mission_resume::cli_status_line(view);
    assert!(line.contains("resumable"), "{line}");
    assert!(line.contains("n1"), "{line}");
    assert!(!line.contains("fully complete"), "{line}");

    // The record-level verdict agrees with the view.
    let loaded = PersistedMission::load(&dir.join(format!("{mission_id}.json"))).expect("reload");
    assert_eq!(loaded.resume_verdict(), ResumeVerdict::Resumable);
    assert!(resumable_missions(&workspace.join("nonexistent-subdir")).is_empty());

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn durable_mission_state_production_wiring() {
    // The recovery transaction must fold interrupted Running nodes and the
    // dispatch path must gate on the persisted verdict — otherwise a
    // crash-interrupted mission wedges on "refuse dispatch of non-pending".
    let dag_src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/dag.rs"))
        .expect("dag.rs");
    assert!(
        dag_src.contains("mission.reconcile_interrupted(now)"),
        "recovery transaction must fold interrupted Running nodes"
    );
    assert!(
        dag_src.contains("resume_verdict()"),
        "persisted dispatch must gate on the recorded verdict"
    );
    assert!(
        dag_src.contains("[MISSION_RESUMED]"),
        "the resume point must be explicit in the mission report"
    );

    // `susi status` surfaces resumable workspace DAG missions — the
    // enumeration is production-reachable, not a test-only helper.
    let status_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/cli/status_cli.rs"
    ))
    .expect("status_cli.rs");
    assert!(
        status_src.contains("read_resumable_missions"),
        "susi status must enumerate resumable durable missions"
    );
    assert!(
        status_src.contains("resumable DAG missions"),
        "susi status must render the resume surface"
    );
}
