//! Persist executable mission DAG state (VC-201-021).

use crate::mission_persist::{NodeTerminal, PersistedMission, PersistedNode};
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch_dir() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("susi-vc201021-{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

#[test]
fn vc_201_021_persist_and_resume_runnable_after_restart() {
    let dir = scratch_dir();
    let mut m = PersistedMission::new("mission-a");
    m.upsert_node(PersistedNode {
        id: "n0".into(),
        input: "root".into(),
        dependencies: vec![],
        output: None,
        state: NodeTerminal::Pending,
    });
    m.upsert_node(PersistedNode {
        id: "n1".into(),
        input: "child".into(),
        dependencies: vec!["n0".into()],
        output: None,
        state: NodeTerminal::Pending,
    });
    assert_eq!(m.runnable().len(), 1);
    assert_eq!(m.runnable()[0].id, "n0");
    m.dispatch("n0").expect("dispatch n0");
    m.complete("n0", "out0".into()).expect("complete n0");
    let path = m.save(&dir).expect("save");
    drop(m);

    let loaded = PersistedMission::load(&path).expect("load");
    assert_eq!(loaded.nodes["n0"].state, NodeTerminal::Completed);
    assert_eq!(loaded.nodes["n1"].state, NodeTerminal::Pending);
    let ready = loaded.runnable();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, "n1");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vc_201_021_refuse_dispatch_before_deps() {
    let mut m = PersistedMission::new("mission-b");
    m.upsert_node(PersistedNode {
        id: "a".into(),
        input: "a".into(),
        dependencies: vec![],
        output: None,
        state: NodeTerminal::Pending,
    });
    m.upsert_node(PersistedNode {
        id: "b".into(),
        input: "b".into(),
        dependencies: vec!["a".into()],
        output: None,
        state: NodeTerminal::Pending,
    });
    assert!(m.dispatch("b").is_err());
    assert_eq!(m.nodes["b"].state, NodeTerminal::Pending);
}

#[test]
fn vc_201_021_running_not_treated_as_complete_on_reload() {
    let dir = scratch_dir();
    let mut m = PersistedMission::new("mission-c");
    m.upsert_node(PersistedNode {
        id: "x".into(),
        input: "x".into(),
        dependencies: vec![],
        output: None,
        state: NodeTerminal::Running,
    });
    let path = m.save(&dir).expect("save");
    let loaded = PersistedMission::load(&path).expect("load");
    assert_eq!(loaded.nodes["x"].state, NodeTerminal::Running);
    assert!(loaded.runnable().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
