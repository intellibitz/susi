//! T-DEEPSEEK-94 mastery tests (VC-202-011): snapshot a mission's durable
//! state, restore it, and roll a failed mission back to a known-good
//! point — without touching another mission's state. A bad step is
//! undone cheaply and precisely: the restored record resumes through the
//! normal recovery machinery, the restore itself is recorded on the
//! mission, and the failure report names the restore point.

use crate::dag::{self, MissionDag};
use crate::mission_persist::{NodeTerminal, PersistedMission, ResumeVerdict};
use crate::mission_resume::rollback_mission;
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
    let dir = std::env::temp_dir().join(format!("susi-snapshot-rollback-{tag}-{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn mission_path(workspace: &Path, mission_id: &str) -> PathBuf {
    PersistedMission::missions_dir(workspace).join(format!("{mission_id}.json"))
}

fn seed_two_node(workspace: &Path, goal: &str) -> String {
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

fn ok_dispatch() -> (dag::DagModelGenerator, dag::DagToolExecutor) {
    (
        Arc::new(|_: &str, _: &Path| "ok-output".to_string()) as dag::DagModelGenerator,
        Arc::new(|_: &str, _: &serde_json::Value, _: &Path| Ok(String::new()))
            as dag::DagToolExecutor,
    )
}

#[test]
fn snapshot_rollback_restores_known_good_without_touching_sibling() {
    let workspace = scratch_dir("sibling");
    let id_a = seed_two_node(&workspace, "mission alpha rollback");
    let id_b = seed_two_node(&workspace, "mission beta untouched");
    let sibling_before = std::fs::read(mission_path(&workspace, &id_b)).expect("sibling bytes");

    // Snapshot A, then simulate the bad step: n1 goes terminally Failed.
    let a = PersistedMission::load(&mission_path(&workspace, &id_a)).expect("load a");
    let snap = a
        .snapshot(&workspace, "pre-risky", 8, 1_000)
        .expect("snapshot");
    {
        let mut cur = PersistedMission::load(&mission_path(&workspace, &id_a)).expect("load a");
        cur.nodes.get_mut("n1").expect("n1").state = NodeTerminal::Failed;
        cur.nodes.get_mut("n1").expect("n1").output =
            Some("[TOOL_ERROR] wrote the wrong file".to_string());
        cur.save(&PersistedMission::missions_dir(&workspace))
            .expect("save a");
    }
    assert_eq!(
        PersistedMission::load(&mission_path(&workspace, &id_a))
            .expect("load a")
            .resume_verdict(),
        ResumeVerdict::TerminalFailed(vec![(
            "n1".to_string(),
            "[TOOL_ERROR] wrote the wrong file".to_string()
        )])
    );

    // Roll A back: the record returns to the known-good point, the
    // restore is recorded naming the states it replaced, and B's file is
    // byte-identical — one mission's rollback never touches another's.
    let restored = PersistedMission::rollback(&workspace, &id_a, None, 2_000).expect("rollback");
    assert_eq!(restored.nodes["n0"].state, NodeTerminal::Completed);
    assert_eq!(restored.nodes["n0"].output.as_deref(), Some("root-out"));
    assert_eq!(restored.nodes["n1"].state, NodeTerminal::Running);
    assert_eq!(restored.restores.len(), 1);
    assert_eq!(restored.restores[0].snapshot, snap);
    assert!(
        restored.restores[0]
            .replaced
            .iter()
            .any(|r| r == "n1:failed"),
        "replaced must name the failed state: {:?}",
        restored.restores[0].replaced
    );
    let on_disk = PersistedMission::load(&mission_path(&workspace, &id_a)).expect("reload a");
    assert_eq!(on_disk.resume_verdict(), ResumeVerdict::Resumable);
    assert_eq!(on_disk.restores.len(), 1, "the restore itself is durable");
    let sibling_after = std::fs::read(mission_path(&workspace, &id_b)).expect("sibling bytes");
    assert_eq!(sibling_before, sibling_after, "sibling mission untouched");

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn snapshot_rollback_failed_mission_resumes_after_restore() {
    // The full production path: crash-interrupted mission resumes, the
    // resume auto-snapshots, the mission then records a terminal failure
    // whose report names the snapshot, the operator rolls back through
    // the bus-arm entry point, and the next dispatch completes.
    let workspace = scratch_dir("e2e");
    let goal = "mission gamma e2e";
    let id = seed_two_node(&workspace, goal);
    let board = Arc::new(HighDensityContextStore::new(1024));
    let (gen, tool) = ok_dispatch();
    let results = dag::dispatch_mission_dag_with_backends(goal, &workspace, &board, gen, tool);
    let text = results
        .iter()
        .map(|(_, t)| t.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("[MISSION_RESUMED]"), "{text}");

    // The resume snapshotted the pre-resume record — n1 still Running in
    // the snapshot, the known-good point.
    let snaps = PersistedMission::list_snapshots(&workspace, &id);
    assert_eq!(snaps.len(), 1, "one pre-resume snapshot: {snaps:?}");
    let snap_view =
        PersistedMission::load(&PersistedMission::snapshots_dir(&workspace, &id).join(&snaps[0]))
            .expect("snapshot parses");
    assert_eq!(snap_view.nodes["n1"].state, NodeTerminal::Running);
    assert_eq!(snap_view.nodes["n0"].state, NodeTerminal::Completed);

    // Now the bad step: the child records a terminal failure.
    {
        let mut cur = PersistedMission::load(&mission_path(&workspace, &id)).expect("load");
        cur.nodes.get_mut("n1").expect("n1").state = NodeTerminal::Failed;
        cur.nodes.get_mut("n1").expect("n1").output = Some("[STALE_FENCE] displaced".to_string());
        cur.save(&PersistedMission::missions_dir(&workspace))
            .expect("save");
    }
    let (gen, tool) = ok_dispatch();
    let failed = dag::dispatch_mission_dag_with_backends(goal, &workspace, &board, gen, tool);
    let failed_text = failed
        .iter()
        .map(|(_, t)| t.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(failed_text.contains("recorded failure"), "{failed_text}");
    assert!(
        failed_text.contains("snapshot") && failed_text.contains("rollback"),
        "the failure report names the restore point: {failed_text}"
    );

    // The operator entry point — the same fn the gawd.mission.rollback
    // arm calls — restores the known-good record.
    let (view, line) = rollback_mission(&workspace, &id, None, 5_000).expect("rollback");
    assert!(line.contains("resumable"), "{line}");
    assert_eq!(
        view.nodes["n1"].view,
        crate::mission_resume::NodeView::Running
    );

    // Next dispatch resumes the restored state: the child is folded and
    // re-served exactly once, the root's output is still preserved.
    let calls = Arc::new(AtomicUsize::new(0));
    let gen = {
        let calls = Arc::clone(&calls);
        Arc::new(move |_: &str, _: &Path| -> String {
            calls.fetch_add(1, Ordering::SeqCst);
            "rolled-back-output".to_string()
        }) as dag::DagModelGenerator
    };
    let tool = Arc::new(|_: &str, _: &serde_json::Value, _: &Path| Ok(String::new()))
        as dag::DagToolExecutor;
    let results = dag::dispatch_mission_dag_with_backends(goal, &workspace, &board, gen, tool);
    let text = results
        .iter()
        .map(|(_, t)| t.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!text.contains("DAG_EXECUTION_FAILED"), "{text}");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "{text}");
    let final_m = PersistedMission::load(&mission_path(&workspace, &id)).expect("final");
    assert_eq!(final_m.nodes["n1"].state, NodeTerminal::Completed);
    assert_eq!(final_m.nodes["n0"].output.as_deref(), Some("root-out"));
    assert_eq!(final_m.restores.len(), 1, "the rollback stays on record");

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn snapshot_rollback_named_bounded_and_absent() {
    let workspace = scratch_dir("bounded");
    let id = seed_two_node(&workspace, "mission delta bounded");
    let m = PersistedMission::load(&mission_path(&workspace, &id)).expect("load");

    // keep_last bounds the pile; names sort chronologically.
    for i in 0..10u64 {
        m.snapshot(&workspace, &format!("point-{i}"), 3, 1_000 + i)
            .expect("snapshot");
    }
    let names = PersistedMission::list_snapshots(&workspace, &id);
    assert_eq!(names.len(), 3, "pruned to keep_last: {names:?}");
    assert!(names[2].contains("point-9"), "newest kept: {names:?}");

    // No snapshot at all → a clean named refusal, not a panic.
    let other = PersistedMission::rollback(&workspace, "mission-never-snapshotted", None, 9_999);
    assert!(other.is_err());
    assert!(
        other
            .err()
            .map(|e| e.to_string().contains("no snapshot"))
            .unwrap_or(false),
        "refusal names the missing snapshot"
    );

    // A pruned named point is gone — the rollback says so.
    let gone = PersistedMission::rollback(
        &workspace,
        &id,
        Some("00000000000000001000-point-0.json"),
        9_999,
    );
    assert!(gone.is_err(), "pruned snapshot must not restore");

    // Rollback to the newest restores and the record shows it.
    let restored = PersistedMission::rollback(&workspace, &id, None, 9_999).expect("rollback");
    assert!(restored.restores[0].snapshot.contains("point-9"));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn snapshot_rollback_production_wiring() {
    // The snapshot is taken on the production resume path, the rollback
    // is reached through the gawd bus arm, and `susi status` surfaces
    // which failed missions still hold a restore point.
    let dag_src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/dag.rs"))
        .expect("dag.rs");
    assert!(
        dag_src.contains("persist.mission.snapshot("),
        "the persisted resume path must snapshot the pre-resume record"
    );
    assert!(
        dag_src.contains("latest_snapshot"),
        "the failure report must name the available restore point"
    );

    let handler = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/susi-gawd/src/plane_handler.rs"
    ))
    .expect("plane_handler.rs");
    assert!(
        handler.contains("\"gawd.mission.rollback\"") && handler.contains("rollback_mission"),
        "the bus arm must reach the rollback entry point"
    );

    let status_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/cli/status_cli.rs"
    ))
    .expect("status_cli.rs");
    assert!(
        status_src.contains("read_rollback_ready") && status_src.contains("mission-snapshots"),
        "susi status must surface failed missions holding snapshots"
    );
}
