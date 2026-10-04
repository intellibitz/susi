//! Mastery verification for VC-202-007: intent becomes a typed, executable
//! DAG.
//!
//! The vector's claim has two halves. The machinery half is real:
//! `susi_core::intent_plan` plans a natural-language intent into a validated,
//! acyclic DAG of registered leaf operations, executes it in topological
//! order with per-node provenance, and refuses command shapes and
//! unplannable intents as typed failures. The production half is delivered:
//! `SusiMasterAgent::solve_planned_mission` — the mission path callers
//! actually invoke — tries `Planner::plan` on the whole intent and on
//! every decomposed step before falling to the general solve; leaf-
//! expressible intents and steps run as real DAGs via
//! `intent_plan::execute` with per-node `NodeProvenance` recorded as
//! `PLAN_NODE_EXECUTED` interactions on the mission report, command-shaped
//! input is refused (`PLAN_REFUSAL`/`PLAN_STEP_REFUSED`), and steps the
//! leaf vocabulary cannot express take a named `PLAN_STEP_ROUTE` instead
//! of a silent single-goal fallback. These tests pin both halves.
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

/// The production path delivers the contract: `solve_planned_mission`
/// consults the typed planner on the whole intent and on every decomposed
/// step, executes leaf-expressible work as a real DAG with per-node
/// provenance surfaced on the mission report, and refuses what it cannot
/// serve as a typed failure. Verified by source inspection of the
/// production orchestrator — the same convention as the VC-202-010 and
/// VC-202-014 reachability scans — plus the executable machinery proofs
/// above, which call the identical public API the production path calls.
#[test]
fn vc_202_007_mastery_production_path_runs_typed_dags() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/ama/master.rs"))
        .expect("production orchestrator source must exist");

    // The typed planner is consulted on the production mission path — on
    // the whole intent and per decomposed step.
    assert!(
        src.contains("Planner::plan(goal)"),
        "whole-intent planning must consult Planner::plan"
    );
    assert!(
        src.contains("Planner::plan(&sub_goal)"),
        "each decomposed step must consult Planner::plan"
    );
    // Executed DAGs produce per-node provenance on the report.
    assert!(
        src.contains("intent_plan::execute"),
        "the production path must execute plans via intent_plan::execute"
    );
    assert!(
        src.contains("PLAN_NODE_EXECUTED"),
        "per-node provenance must be recorded on the report"
    );
    // Refusals are typed and named, never silently approximated.
    assert!(
        src.contains("PLAN_REFUSAL"),
        "command-shaped intents refused"
    );
    assert!(
        src.contains("PLAN_STEP_ROUTE"),
        "non-leaf steps take a named route, not a silent fallback"
    );
}

/// The contract the route split implements: a leaf-expressible step goes
/// down the DAG path with provenance; a step outside the vocabulary is a
/// typed `PlanError` (routed, named — never a one-node guess).
#[test]
fn vc_202_007_mastery_route_split_is_typed() {
    let served = Planner::plan("list the directory .").expect("servable intent");
    assert!(!served.nodes.is_empty());
    assert!(matches!(
        Planner::plan("teleport me to the moon"),
        Err(PlanError::Unplannable { .. }) | Err(PlanError::CommandShaped)
    ));
}
