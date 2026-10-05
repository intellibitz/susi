//! VC-202-013: each mission yields a trace linking intent to plan to node
//! to model to tokens to cost to outcome, queryable per agent and per
//! task, append-only and durable across restarts.

use crate::mission_trace::{
    read_all, read_all_for_agent, read_all_for_task, MissionTrace, ModelCallCost,
};

fn workspace() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn call(node: &str, model: &str, tokens: u64, cost_micros: u64) -> ModelCallCost {
    ModelCallCost {
        node: Some(node.into()),
        model: model.into(),
        tokens,
        cost_micros,
    }
}

/// A mission's cost is attributable per model call, not just an
/// aggregate: the trace carries the full breakdown, and the total is
/// the sum of it — never a figure nobody can trace back.
#[test]
fn cost_ledger_trace_breaks_down_cost_per_model_call() {
    let mut trace = MissionTrace::new("m-cost-1", "plan a deploy", "COMPLETE", "swarm");
    trace.task_id = Some("T-CLAUDE-9".into());
    trace.node = Some("node-a".into());
    trace.plan_steps = vec!["plan".into(), "deploy".into()];
    trace.model_calls = vec![
        call("node-a", "anthropic/claude", 1_000, 5_000),
        call("node-b", "openai/gpt", 2_000, 9_000),
    ];
    assert_eq!(trace.total_tokens(), 3_000);
    assert_eq!(trace.total_cost_micros(), 14_000);
}

/// Append-only and durable across restarts: traces persisted by one
/// process are read back whole by a fresh read — "restart" is modeled
/// by dropping all in-memory state and reloading straight from the
/// workspace's `.susi/mission_traces.jsonl`.
#[test]
fn cost_ledger_trace_is_append_only_and_survives_restart() {
    let ws = workspace();
    for (id, task, agent) in [
        ("m-1", "T-CLAUDE-1", "Coder"),
        ("m-2", "T-CLAUDE-2", "Coder"),
        ("m-3", "T-CLAUDE-1", "Reviewer"),
    ] {
        let mut trace = MissionTrace::new(id, "goal", "COMPLETE", "swarm");
        trace.task_id = Some(task.into());
        trace.agents = vec![agent.into()];
        trace.model_calls = vec![call("node-a", "m", 100, 500)];
        trace.emit(ws.path()).unwrap();
    }
    // Simulate a restart: nothing but the file on disk is consulted.
    let reloaded = read_all(ws.path());
    assert_eq!(
        reloaded.len(),
        3,
        "every emitted trace must survive a reload"
    );
    assert_eq!(
        reloaded
            .iter()
            .map(MissionTrace::total_cost_micros)
            .sum::<u64>(),
        1_500
    );
}

/// Queryable per agent: every trace a given agent participated in is
/// retrievable, and traces belonging to other agents are excluded.
#[test]
fn cost_ledger_trace_queryable_per_agent() {
    let mut coder_1 = MissionTrace::new("m-1", "goal", "COMPLETE", "swarm");
    coder_1.agents = vec!["Coder".into()];
    let mut coder_2 = MissionTrace::new("m-2", "goal", "FAILED", "swarm");
    coder_2.agents = vec!["Coder".into(), "Reviewer".into()];
    let mut reviewer_only = MissionTrace::new("m-3", "goal", "COMPLETE", "swarm");
    reviewer_only.agents = vec!["Reviewer".into()];
    let traces = vec![coder_1, coder_2, reviewer_only];

    let coder_traces = read_all_for_agent(&traces, "Coder");
    assert_eq!(coder_traces.len(), 2);
    assert!(coder_traces
        .iter()
        .all(|t| t.agents.contains(&"Coder".to_string())));

    let reviewer_traces = read_all_for_agent(&traces, "Reviewer");
    assert_eq!(reviewer_traces.len(), 2);
}

/// Queryable per task: every mission dispatched on behalf of a queue
/// task is retrievable by that task's id, and the per-mission cost
/// breakdown for a task is the sum of every mission that served it.
#[test]
fn cost_ledger_trace_queryable_per_task() {
    let mut m1 = MissionTrace::new("m-1", "goal", "COMPLETE", "swarm");
    m1.task_id = Some("T-CLAUDE-9".into());
    m1.model_calls = vec![call("node-a", "m", 100, 1_000)];
    let mut m2 = MissionTrace::new("m-2", "goal", "COMPLETE", "swarm");
    m2.task_id = Some("T-CLAUDE-9".into());
    m2.model_calls = vec![call("node-a", "m", 50, 500)];
    let mut m3 = MissionTrace::new("m-3", "goal", "COMPLETE", "swarm");
    m3.task_id = Some("T-CLAUDE-10".into());
    let traces = vec![m1, m2, m3];

    let for_task_9 = read_all_for_task(&traces, "T-CLAUDE-9");
    assert_eq!(for_task_9.len(), 2);
    assert_eq!(
        for_task_9
            .iter()
            .map(|t| t.total_cost_micros())
            .sum::<u64>(),
        1_500,
        "a task's cost is attributable as the sum of the missions that served it"
    );

    let for_task_10 = read_all_for_task(&traces, "T-CLAUDE-10");
    assert_eq!(for_task_10.len(), 1);

    let for_unknown_task = read_all_for_task(&traces, "T-GHOST-1");
    assert!(for_unknown_task.is_empty());
}

/// Node, model and token identity survive the JSONL round trip —
/// linking intent to plan to node to model to tokens to cost to
/// outcome is a durable property of the record, not something lost on
/// (de)serialization.
#[test]
fn cost_ledger_trace_links_intent_plan_node_model_tokens_cost_outcome() {
    let ws = workspace();
    let mut trace = MissionTrace::new("m-full", "migrate the database", "COMPLETE", "swarm");
    trace.task_id = Some("T-CLAUDE-42".into());
    trace.node = Some("coordinator".into());
    trace.plan_steps = vec!["backup".into(), "migrate".into(), "verify".into()];
    trace.model_calls = vec![call("worker-1", "anthropic/claude-haiku", 1_200, 600)];
    trace.emit(ws.path()).unwrap();

    let reloaded = read_all(ws.path());
    assert_eq!(reloaded.len(), 1);
    let t = &reloaded[0];
    assert_eq!(t.task_id.as_deref(), Some("T-CLAUDE-42"));
    assert_eq!(t.node.as_deref(), Some("coordinator"));
    assert_eq!(t.plan_steps, vec!["backup", "migrate", "verify"]);
    assert_eq!(t.model_calls.len(), 1);
    assert_eq!(t.model_calls[0].model, "anthropic/claude-haiku");
    assert_eq!(t.total_tokens(), 1_200);
    assert_eq!(t.total_cost_micros(), 600);
    assert_eq!(t.outcome, "COMPLETE");
}
