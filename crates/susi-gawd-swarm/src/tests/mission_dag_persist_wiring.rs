//! Wiring: PersistedMission ↔ MissionDag dispatch/resume (T-INTELLIBITZ-5 / VC-201-021).

use crate::dag::MissionDag;
use crate::mission_persist::{NodeTerminal, PersistedMission};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use susi_gawd_agents::HighDensityContextStore;

fn scratch_dir() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("susi-mission-dag-persist-{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

#[test]
fn mission_dag_persist_wiring() {
    let workspace = scratch_dir();
    let persist_dir = PersistedMission::missions_dir(&workspace);
    let goal = "wire persist into mission dag";
    let mission_id = PersistedMission::mission_id_for_goal(goal);

    // Build a two-node DAG and seed persist as dispatch would.
    let mut dag = MissionDag::new(goal);
    let child = dag.push_node("child", "depend on root", vec![0]);
    assert_eq!(child, 1);
    let mut persist = dag.to_persisted(&mission_id);
    persist.save(&persist_dir).expect("seed save");

    // Simulate first session completing only the root node.
    MissionDag::persist_node_complete(&mut persist, &persist_dir, 0, "root-out")
        .expect("complete root");
    assert_eq!(persist.nodes["n0"].state, NodeTerminal::Completed);
    assert_eq!(persist.nodes["n1"].state, NodeTerminal::Pending);
    assert_eq!(persist.runnable().len(), 1);
    assert_eq!(persist.runnable()[0].id, "n1");

    // Restart: load persist and rebuild DAG — unfinished must not look complete.
    let path = persist_dir.join(format!("{mission_id}.json"));
    let loaded = PersistedMission::load(&path).expect("reload");
    let resumed = MissionDag::from_persisted(&loaded);
    assert!(resumed.nodes[0].completed, "completed root must stay done");
    assert!(
        !resumed.nodes[1].completed,
        "pending child must not be treated as complete"
    );
    assert_eq!(resumed.nodes[1].dependencies, vec![0]);

    // Resume execute path: only unfinished work remains; completed graph
    // slice is not re-dispatched (execute_dag sees node 0 already done).
    let mut resume_dag = resumed;
    let board = Arc::new(HighDensityContextStore::new(1024));
    let (tx, rx) = crate::susi_core::bus::create_swarm_bus();
    // Mark child complete without LLM by simulating a prior partial persist
    // where everything finished — proves re-entry is a no-op on full complete.
    resume_dag.nodes[1].completed = true;
    let evidence = resume_dag
        .execute_dag(std::path::Path::new("."), &board, &tx)
        .expect("fully completed dag is a no-op");
    assert!(evidence.is_empty());
    assert!(
        rx.try_recv().is_err(),
        "no event is pending after a no-op re-entry"
    );
    assert!(board.is_empty());

    // Round-trip through dispatch's load_or_new + from_persisted path.
    let again = PersistedMission::load_or_new(&persist_dir, &mission_id).expect("load_or_new");
    let dag2 = MissionDag::from_persisted(&again);
    assert!(dag2.nodes[0].completed);
    assert!(!dag2.nodes[1].completed);

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn mission_id_for_goal_is_deterministic_and_distinct_per_goal() {
    let id1 = PersistedMission::mission_id_for_goal("alpha goal");
    let id2 = PersistedMission::mission_id_for_goal("alpha goal");
    let id3 = PersistedMission::mission_id_for_goal("beta goal");
    assert_eq!(id1, id2);
    assert_ne!(id1, id3);
    assert!(!id1.is_empty());
}

#[test]
fn persist_node_complete_records_output_and_round_trips_from_disk() {
    let workspace = scratch_dir().join("output-round-trip");
    let persist_dir = PersistedMission::missions_dir(&workspace);
    let dag = MissionDag::new("output round-trip");
    let mission_id = PersistedMission::mission_id_for_goal("output round-trip");
    let mut persist = dag.to_persisted(&mission_id);
    persist.save(&persist_dir).expect("initial save");

    MissionDag::persist_node_complete(&mut persist, &persist_dir, 0, "verified-artifact-hash")
        .expect("complete root with output");
    let path = persist_dir.join(format!("{mission_id}.json"));
    let loaded = PersistedMission::load(&path).expect("reload persisted mission");
    assert_eq!(loaded.nodes["n0"].state, NodeTerminal::Completed);
    assert_eq!(
        loaded.nodes["n0"].output.as_deref(),
        Some("verified-artifact-hash")
    );

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn multi_dependency_join_node_becomes_runnable_only_after_all_parents_complete() {
    let workspace = scratch_dir().join("join-node");
    let persist_dir = PersistedMission::missions_dir(&workspace);
    let mut dag = MissionDag::new("diamond join");
    let n1 = dag.push_node("branch-b", "parallel branch", vec![]);
    let n2 = dag.push_node("join", "depends on n0 and n1", vec![0, n1]);
    assert_eq!(n2, 2);

    let mission_id = PersistedMission::mission_id_for_goal("diamond join");
    let mut persist = dag.to_persisted(&mission_id);
    persist.save(&persist_dir).expect("save diamond");

    // Completing only n0 leaves n2 blocked on n1.
    MissionDag::persist_node_complete(&mut persist, &persist_dir, 0, "n0-done").expect("n0");
    let runnable_ids: Vec<&str> = persist.runnable().iter().map(|n| n.id.as_str()).collect();
    assert_eq!(runnable_ids, vec!["n1"]);

    // Completing n1 unlocks n2.
    MissionDag::persist_node_complete(&mut persist, &persist_dir, 1, "n1-done").expect("n1");
    let runnable_ids2: Vec<&str> = persist.runnable().iter().map(|n| n.id.as_str()).collect();
    assert_eq!(runnable_ids2, vec!["n2"]);

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn load_or_new_creates_fresh_mission_when_file_absent_and_reloads_when_present() {
    let workspace = scratch_dir().join("load-or-new");
    let persist_dir = PersistedMission::missions_dir(&workspace);
    let fresh = PersistedMission::load_or_new(&persist_dir, "m-fresh").expect("fresh");
    assert_eq!(fresh.mission_id, "m-fresh");
    assert!(fresh.nodes.is_empty());

    let dag = MissionDag::new("persisted goal");
    let seeded = dag.to_persisted("m-fresh");
    seeded.save(&persist_dir).expect("save seeded");

    let reloaded = PersistedMission::load_or_new(&persist_dir, "m-fresh").expect("reloaded");
    assert_eq!(reloaded.nodes.len(), 1);
    assert!(reloaded.nodes.contains_key("n0"));

    let _ = std::fs::remove_dir_all(&workspace);
}
