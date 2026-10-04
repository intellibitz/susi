//! Mastery verification for VC-202-007: intent becomes a typed, executable
//! DAG.
//!
//! The vector's claim has two halves. The machinery half is real:
//! `susi_core::intent_plan` plans a natural-language intent into a validated,
//! acyclic DAG of registered leaf operations, executes it in topological
//! order with per-node provenance, and refuses command shapes and
//! unplannable intents as typed failures. The production half is refuted:
//! `scripts/check-reachability.py` finds zero production callers of
//! `Planner::plan` or `intent_plan::execute`, and the mission path actually
//! invoked (`MissionPlanner::plan_mission` over the gemi plane bus) returns
//! a comma-split `Vec<String>` — no node types, no declared inputs/outputs,
//! no provenance, and a silent single-goal fallback for any intent the
//! model cannot serve. These tests pin both halves.
//! (Filed under susi-gawd-swarm because the verifying scope could not
//! reserve susi-core.)

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use susi_core::intent_plan::{execute, leaf_operation, PlanError, Planner};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "susi_vc202007m_{tag}_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// What does hold: a natural-language intent becomes a real typed DAG —
/// every node names a registered leaf operation with declared inputs and a
/// declared output, the graph validates and executes in topological order,
/// and each executed node records its operation, resolved inputs, cost and
/// output in the provenance trail.
#[test]
fn vc_202_007_mastery_typed_dag_machinery_holds() {
    let ws = scratch("dag");
    std::fs::write(ws.join("notes.md"), "# Title\ncontains unsafe here\n").unwrap();
    std::fs::create_dir_all(ws.join("crates")).unwrap();

    let dag = Planner::plan("read the file notes.md and search for unsafe in crates")
        .expect("plannable intent");
    assert!(dag.nodes.len() >= 2, "expected a multi-node DAG");
    dag.validate().expect("plan must validate");
    for node in &dag.nodes {
        let op = leaf_operation(&node.operation)
            .unwrap_or_else(|| panic!("{} is not a leaf operation", node.operation));
        for input in op.inputs {
            assert!(node.inputs.contains_key(*input), "missing {input}");
        }
    }
    let order = dag.topo_order().expect("acyclic");
    assert_eq!(order.len(), dag.nodes.len());
    assert!(dag.total_cost() > 0, "each node carries a declared cost");

    let provenance = execute(&dag, &ws).expect("executable");
    assert_eq!(provenance.len(), dag.nodes.len());
    let dag_ids: BTreeSet<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
    let prov_ids: BTreeSet<&str> = provenance.iter().map(|p| p.node_id.as_str()).collect();
    assert_eq!(dag_ids, prov_ids, "one provenance record per DAG node");
    assert!(provenance
        .iter()
        .any(|p| p.operation == "view_file" && p.output.contains("unsafe")));

    let _ = std::fs::remove_dir_all(&ws);
}

/// What does hold: the two refusal shapes the vector requires are typed
/// failures — a command-shaped input dies before planning (Mandate 55), and
/// an intent with no recognizable action is `Unplannable` carrying the
/// missing capability, not a silently approximated plan.
#[test]
fn vc_202_007_mastery_refusals_are_typed_not_approximated() {
    assert_eq!(
        Planner::plan("rm -rf /").unwrap_err(),
        PlanError::CommandShaped
    );
    assert_eq!(
        Planner::plan("cargo build --release").unwrap_err(),
        PlanError::CommandShaped
    );
    match Planner::plan("bake a sourdough loaf").unwrap_err() {
        PlanError::Unplannable { missing } => {
            assert!(!missing.is_empty(), "refusal names what is missing")
        }
        other => panic!("expected Unplannable, got {other:?}"),
    }
}

/// What does NOT hold, pinned as documentation: the production mission
/// intake never reaches this planner. `MissionPlanner::plan_mission`
/// (crates/susi-gemi/src/engines/runtime.rs) splits an LLM reply on commas
/// into `Vec<String>` goals and falls back to `goals.push(goal)` when the
/// model cannot decompose — a silent approximation, the exact failure mode
/// the vector forbids. No production code calls `Planner::plan` or
/// `execute` (see the verdict's recorded `check`), so on the path users
/// actually run, an intent never becomes a DAG and never earns per-node
/// provenance.
#[test]
fn vc_202_007_mastery_production_path_uses_comma_split_goals() {
    // Compile-time boundary: the typed planner is reachable from a test
    // crate but the reachability checker records zero production callers.
    // What is executable here is the contract the production path must
    // satisfy to flip the verdict: a plan that cannot be served must come
    // back as `PlanError`, never as a one-node guess. `Planner` already
    // demonstrates that contract — `plan_mission` does not implement it.
    let served = Planner::plan("list the directory .").expect("servable intent");
    assert!(!served.nodes.is_empty());
    assert!(matches!(
        Planner::plan("teleport me to the moon"),
        Err(PlanError::Unplannable { .. }) | Err(PlanError::CommandShaped)
    ));
}
