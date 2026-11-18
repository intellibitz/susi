//! Routing-ladder trace on the production cascade (VC-202-004, T-DEEPSEEK-101).
//!
//! A fallback that happens silently is a defect: every step down the
//! model → provider → local ladder must record which candidate it named,
//! how the rung resolved, and why — including refusals (cooled providers,
//! key-arbitration denials, quota exhaustion). These tests exercise the
//! `RoutingLadder` machinery the production cascade writes into, plus a
//! source scan proving the wiring lives on the dispatch path.

use crate::engines::brain::TaskClass;
use crate::engines::routing::{LadderRung, RoutingLadder, StepOutcome};

#[test]
fn routing_ladder_records_every_step_down_with_reason() {
    let mut ladder = RoutingLadder::new(TaskClass::Code);
    ladder.record(
        LadderRung::Provider,
        "openai-gpt-5",
        StepOutcome::Skipped,
        "key quota window exhausted — resets at unix 4100",
    );
    ladder.record(
        LadderRung::Provider,
        "anthropic-claude",
        StepOutcome::Failed,
        "connection timed out",
    );
    ladder.record(
        LadderRung::Local,
        "susi-7b.gguf",
        StepOutcome::Selected,
        "every provider rung stepped down (2 skipped/failed)",
    );

    assert_eq!(ladder.steps.len(), 3);
    assert!(
        ladder.steps.iter().all(|step| !step.reason.is_empty()),
        "every recorded step must carry its reason — a silent step down is the defect"
    );
    let selected = ladder.selected().expect("a rung was selected");
    assert_eq!(selected.candidate, "susi-7b.gguf");
    assert_eq!(selected.rung, LadderRung::Local);
}

#[test]
fn routing_ladder_step_downs_counts_only_non_selected() {
    let mut ladder = RoutingLadder::new(TaskClass::Chat);
    assert_eq!(ladder.step_downs(), 0);
    ladder.record(
        LadderRung::Provider,
        "gemini-flash",
        StepOutcome::Selected,
        "top-ranked candidate above the chat capability floor",
    );
    assert_eq!(
        ladder.step_downs(),
        0,
        "first-choice success is not a fallback"
    );
    ladder.record(
        LadderRung::Provider,
        "ollama-llama",
        StepOutcome::Skipped,
        "provider cooled until unix 9999 after recent failures",
    );
    assert_eq!(ladder.step_downs(), 1);
}

#[test]
fn routing_ladder_summary_names_model_and_step_down_reasons() {
    let mut ladder = RoutingLadder::new(TaskClass::Reasoning);
    ladder.record(
        LadderRung::Provider,
        "openai-gpt-5",
        StepOutcome::Skipped,
        "key rate window exhausted",
    );
    ladder.record(
        LadderRung::Provider,
        "deepseek-v3",
        StepOutcome::Selected,
        "top-ranked candidate above the reasoning capability floor",
    );
    let summary = ladder.summary();
    assert!(
        summary.contains("deepseek-v3"),
        "summary names the model that ran: {summary}"
    );
    assert!(
        summary.contains("openai-gpt-5"),
        "summary names what was stepped down: {summary}"
    );
    assert!(
        summary.contains("key rate window exhausted"),
        "summary carries the refusal reason: {summary}"
    );
}

#[test]
fn routing_ladder_summary_without_selection_says_so() {
    let mut ladder = RoutingLadder::new(TaskClass::Chat);
    ladder.record(
        LadderRung::Provider,
        "ollama-llama",
        StepOutcome::Failed,
        "empty response",
    );
    assert!(ladder.summary().starts_with("no candidate selected"));
}

#[test]
fn routing_ladder_persist_appends_one_json_line_per_decision() {
    let dir = std::env::temp_dir().join(format!(
        "susi-routing-ladder-{}-{}",
        std::process::id(),
        "persist"
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace = dir.join("routing_trace.jsonl");
    // SAFETY: test-only env mutation; this test is the only writer and the
    // path is unique to this process and test.
    unsafe {
        std::env::set_var("SUSI_ROUTING_TRACE_FILE", &trace);
    }

    let mut ladder = RoutingLadder::new(TaskClass::Reflex);
    ladder.record(
        LadderRung::Model,
        "local-7b",
        StepOutcome::Selected,
        "caller-named local model honored",
    );
    ladder.persist();
    ladder.persist();

    let text = std::fs::read_to_string(&trace).expect("trace file written");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "one JSON line per recorded decision");
    for line in lines {
        let value: serde_json::Value = serde_json::from_str(line).expect("each line is JSON");
        assert_eq!(value["schema"], "susi/routing-ladder/v1");
        assert!(value["recorded_at_unix"].as_u64().is_some());
        assert_eq!(value["task_class"], TaskClass::Reflex.label());
        let steps = value["steps"].as_array().expect("steps is an array");
        assert_eq!(steps[0]["candidate"], "local-7b");
        assert_eq!(steps[0]["outcome"], "Selected");
        assert_eq!(steps[0]["rung"], "Model");
        assert!(!steps[0]["reason"].as_str().unwrap_or("").is_empty());
    }
}

#[test]
fn routing_ladder_production_cascade_records_and_persists() {
    // The ladder must be written on the real dispatch path — a trace type
    // nothing calls is scaffold, not delivery.
    let runtime_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/engines/runtime.rs"
    ))
    .expect("runtime.rs readable");

    assert!(
        runtime_src.contains("RoutingLadder::new(class)"),
        "the production provider cascade builds the ladder from the classified task"
    );
    for marker in [
        "StepOutcome::Skipped",
        "StepOutcome::Failed",
        "StepOutcome::Selected",
        "ladder.persist()",
        "ladder.summary()",
    ] {
        assert!(
            runtime_src.contains(marker),
            "production cascade must {marker}"
        );
    }
    // Refusals must reach the trace, not silently `continue`.
    assert!(
        runtime_src.contains("key quota window exhausted"),
        "quota refusals must name their reset in the trace"
    );
    assert!(
        runtime_src.contains("provider cooled until"),
        "cooldown skips must name the cooldown in the trace"
    );
    // The local rung is part of the same ladder.
    assert!(
        runtime_src.contains("LadderRung::Local"),
        "the local fallback rung must be recorded in the same trace"
    );
}

#[test]
fn routing_ladder_cascade_sorts_below_floor_last() {
    // "Cheapest model above its floor": the live cascade sort key must keep
    // `meets_floor` in the ordering so a below-floor candidate trails every
    // floor-meeting one (T-100 floor, honored on the dispatch path itself).
    let runtime_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/engines/runtime.rs"
    ))
    .expect("runtime.rs readable");
    assert!(
        runtime_src.contains("!meets_floor"),
        "the provider cascade must demote below-floor candidates in its sort key"
    );
}
