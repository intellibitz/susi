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
    assert!(rx.is_empty());
    assert!(board.is_empty());

    // Round-trip through dispatch's load_or_new + from_persisted path.
    let again = PersistedMission::load_or_new(&persist_dir, &mission_id).expect("load_or_new");
    let dag2 = MissionDag::from_persisted(&again);
    assert!(dag2.nodes[0].completed);
    assert!(!dag2.nodes[1].completed);

    let _ = std::fs::remove_dir_all(&workspace);
}
