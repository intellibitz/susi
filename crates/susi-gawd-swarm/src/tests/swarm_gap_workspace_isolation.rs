//! Production-entry regressions for DAG workspace isolation (VC-201-029).
//!
//! These tests enter through the persisted MissionDag dispatch hook with only
//! the model and tool providers replaced. The admission, lease, cancellation,
//! intent, scoped mutation, fold, conflict and truth-gate code is unchanged.

use crate::dag::{
    dispatch_mission_dag_with_backends, DagModelGenerator, DagToolExecutor, MissionDag,
};
use crate::mission_persist::{NodeTerminal, PersistedMission};
use crate::susi_error::{EaiError, EaiResult};
use crate::writer_isolation::{FoldOutcome, WriterIsolation};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(tag: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "susi-swarm-workspace-isolation-{tag}-{}-{suffix}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn model_for(prompt: &str, _workspace: &Path) -> String {
    let (path, content) = if prompt.contains("left.txt") {
        ("left.txt", "left\n")
    } else if prompt.contains("right.txt") {
        ("right.txt", "right\n")
    } else if prompt.contains("node 'n0'") {
        ("shared.txt", "node-zero\n")
    } else {
        ("shared.txt", "node-one\n")
    };
    format!("```bash\nprintf '{content}' > {path}\n```")
}

fn scoped_tool(name: &str, args: &Value, workspace: &Path) -> EaiResult<String> {
    if name != "exec_command" {
        return Err(EaiError::protocol(format!("unexpected test tool {name}")));
    }
    let Some(command) = args.get("command").and_then(Value::as_str) else {
        return Err(EaiError::protocol("test exec_command requires command"));
    };
    // Mirror the production command guard: a worker may not escape its
    // private workspace through a parent or absolute path.
    if command.contains("..") || command.contains("/") && command.contains(" > /") {
        return Err(EaiError::filesystem(
            "test exec_command refused a path outside the worker scope",
        ));
    }
    let output = Command::new("sh")
        .args(["-c", command])
        .current_dir(workspace)
        .output()
        .map_err(|error| EaiError::process(format!("test exec_command: {error}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(EaiError::process(format!(
            "test exec_command exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

fn backends() -> (DagModelGenerator, DagToolExecutor) {
    (Arc::new(model_for), Arc::new(scoped_tool))
}

fn save_parallel_mission(workspace: &Path, goal: &str, second_goal: &str) {
    let mut dag = MissionDag::new(goal);
    dag.push_node("second", second_goal, Vec::new());
    let persist_dir = PersistedMission::missions_dir(workspace);
    dag.to_persisted(&PersistedMission::mission_id_for_goal(goal))
        .save(&persist_dir)
        .unwrap();
}

fn load_mission(workspace: &Path, goal: &str) -> PersistedMission {
    let id = PersistedMission::mission_id_for_goal(goal);
    PersistedMission::load(&PersistedMission::missions_dir(workspace).join(format!("{id}.json")))
        .unwrap()
}

#[test]
fn swarm_gap_workspace_isolation_parallel_dag_writers_use_private_scopes() {
    let workspace = temp_dir("parallel-dag");
    let goal = "write left.txt";
    save_parallel_mission(&workspace, goal, "write right.txt");
    let board = Arc::new(crate::HighDensityContextStore::new(4096));
    let (model, tool) = backends();

    let result = dispatch_mission_dag_with_backends(goal, &workspace, &board, model, tool);

    assert!(
        result.is_empty(),
        "successful DAG should have no unbound evidence: {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("left.txt")).unwrap(),
        "left\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("right.txt")).unwrap(),
        "right\n"
    );
    let mission = load_mission(&workspace, goal);
    assert!(mission
        .nodes
        .values()
        .all(|node| node.state == NodeTerminal::Completed));
    assert!(!workspace
        .join(".susi/dag-scopes")
        .read_dir()
        .unwrap()
        .any(|entry| {
            entry
                .ok()
                .is_some_and(|entry| entry.file_type().map(|ty| ty.is_dir()).unwrap_or(false))
        }));
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn swarm_gap_workspace_isolation_conflicting_dag_writers_never_overwrite() {
    let workspace = temp_dir("conflict-dag");
    let goal = "write shared.txt from node zero";
    std::fs::write(workspace.join("shared.txt"), "primary baseline\n").unwrap();
    save_parallel_mission(&workspace, goal, "write shared.txt from node one");
    let board = Arc::new(crate::HighDensityContextStore::new(4096));
    let (model, tool) = backends();

    let result = dispatch_mission_dag_with_backends(goal, &workspace, &board, model, tool);

    let report = result
        .first()
        .map(|(_, output)| output.as_str())
        .unwrap_or("");
    assert!(
        report.contains("write conflict"),
        "conflict must be reviewable: {report}"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("shared.txt")).unwrap(),
        "node-zero\n",
        "the second writer must not silently overwrite the first fold"
    );
    let mission = load_mission(&workspace, goal);
    assert!(mission.nodes.values().any(|node| {
        node.state == NodeTerminal::Failed
            && node
                .output
                .as_deref()
                .is_some_and(|output| output.contains("WRITE_CONFLICT"))
    }));
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn swarm_gap_workspace_isolation_stale_scope_is_quarantined_and_dirty_primary_survives() {
    let workspace = temp_dir("stale-primary");
    std::fs::write(workspace.join("primary.txt"), "human edit\n").unwrap();
    let isolation = WriterIsolation::new(&workspace).unwrap();
    let stale = isolation.scope("worker-old-f1").unwrap();
    std::fs::write(stale.dir.join("ignored.txt"), "old worker\n").unwrap();
    let retired = isolation.retire_stale_scopes("coordinator-2").unwrap();
    assert_eq!(retired.len(), 1);
    assert!(matches!(
        isolation.fold(&stale),
        Err(error) if error.to_string().contains("stale worker scope")
    ));

    let fresh = isolation.scope("worker-new-f2").unwrap();
    std::fs::write(fresh.dir.join("new.txt"), "fresh worker\n").unwrap();
    assert!(matches!(
        isolation.fold(&fresh),
        Ok(FoldOutcome::Clean { .. })
    ));
    assert_eq!(
        std::fs::read_to_string(workspace.join("primary.txt")).unwrap(),
        "human edit\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("new.txt")).unwrap(),
        "fresh worker\n"
    );
    isolation.cleanup(&fresh);
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn swarm_gap_workspace_isolation_unrelated_scopes_merge_without_index_loss() {
    let workspace = temp_dir("index");
    std::fs::write(workspace.join("index.txt"), "base\n").unwrap();
    let isolation = WriterIsolation::new(&workspace).unwrap();
    let first = isolation.scope("worker-a-f1").unwrap();
    let second = isolation.scope("worker-b-f1").unwrap();
    std::fs::write(first.dir.join("index-a.txt"), "a\n").unwrap();
    std::fs::write(second.dir.join("index-b.txt"), "b\n").unwrap();
    let first_out = isolation.fold(&first).unwrap();
    let second_out = isolation.fold(&second).unwrap();
    assert!(matches!(first_out, FoldOutcome::Clean { .. }));
    assert!(matches!(second_out, FoldOutcome::Clean { .. }));
    let names = std::fs::read_dir(&workspace)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<BTreeSet<_>>();
    assert!(names.contains("index.txt"));
    assert!(names.contains("index-a.txt"));
    assert!(names.contains("index-b.txt"));
    isolation.cleanup(&first);
    isolation.cleanup(&second);
    let _ = std::fs::remove_dir_all(workspace);
}
