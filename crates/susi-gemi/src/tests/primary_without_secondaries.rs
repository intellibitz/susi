//! The primary working alone is a first-class path (VC-202-022,
//! T-DEEPSEEK-123). With no other model available, healthy or worth its
//! cost, the primary does the work itself and the mission records *why* it
//! worked alone — every considered candidate and its refusal reason —
//! rather than a silent gap or an error. Nothing in the routing, budget
//! or trace paths treats solo as a failure.

use crate::engines::brain::TaskClass;
use crate::engines::routing::{LadderRung, RoutingLadder, StepOutcome};

#[test]
fn primary_without_secondaries_solo_verdict_names_why_alone() {
    // Every alternative refused, the primary serves alone: the report
    // names the server and carries each loser's reason verbatim —
    // unavailable (cooled), not worth its cost (budget refusal), or
    // failing.
    let mut ladder = RoutingLadder::new(TaskClass::Chat);
    ladder.record(
        LadderRung::Provider,
        "acme-cooled",
        StepOutcome::Skipped,
        "provider cooled until unix 9999",
    );
    ladder.record(
        LadderRung::Provider,
        "acme-broke",
        StepOutcome::Skipped,
        "budget ceiling daily exhausted",
    );
    ladder.record(
        LadderRung::Provider,
        "acme-dead",
        StepOutcome::Failed,
        "connection timed out",
    );
    ladder.record(
        LadderRung::Provider,
        "acme-primary",
        StepOutcome::Selected,
        "top-ranked candidate",
    );
    let solo = ladder.served_alone().expect("served alone");
    assert_eq!(solo.served_by, "acme-primary");
    assert_eq!(solo.rung, LadderRung::Provider);
    assert_eq!(solo.alone_because.len(), 3);
    assert_eq!(solo.alone_because[0].candidate, "acme-cooled");
    assert_eq!(
        solo.alone_because[1].reason,
        "budget ceiling daily exhausted"
    );
    assert_eq!(solo.alone_because[2].outcome, StepOutcome::Failed);
}

#[test]
fn primary_without_secondaries_sole_candidate_alone_by_default() {
    // A single working candidate serving is solo by default — the normal
    // case, not a fallback or an error.
    let mut ladder = RoutingLadder::new(TaskClass::Reflex);
    ladder.record(
        LadderRung::Model,
        "acme-only",
        StepOutcome::Selected,
        "caller-named model honored",
    );
    let solo = ladder.served_alone().expect("alone by default");
    assert_eq!(solo.served_by, "acme-only");
    assert_eq!(solo.rung, LadderRung::Model);
    assert!(
        solo.alone_because.is_empty(),
        "no other model was even considered — alone by default"
    );
    assert_eq!(ladder.step_downs(), 0, "no fallback happened");
}

#[test]
fn primary_without_secondaries_is_never_an_error() {
    // A solo resolution carries no failure marker anywhere: the selected
    // step is a success, the summary says "served alone" and no wording
    // treats the solo path as degraded.
    let mut ladder = RoutingLadder::new(TaskClass::Chat);
    ladder.record(
        LadderRung::Provider,
        "acme-p",
        StepOutcome::Selected,
        "top-ranked candidate",
    );
    assert_eq!(ladder.served_alone().expect("solo").alone_because.len(), 0);
    let summary = ladder.summary();
    assert!(summary.contains("served alone"), "{summary}");
    assert!(!summary.contains("no candidate selected"), "{summary}");
}

#[test]
fn primary_without_secondaries_persisted_mission_carries_verdict() {
    // The mission's persisted trace records the solo verdict: who served,
    // on which rung, and the verbatim reasons everyone else lost.
    let dir = std::env::temp_dir().join(format!("susi-solo-{}-persist", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace = dir.join("routing_trace.jsonl");
    // SAFETY: test-only env mutation; path unique to this process/test.
    unsafe {
        std::env::set_var("SUSI_ROUTING_TRACE_FILE", &trace);
    }
    let mut ladder = RoutingLadder::new(TaskClass::Code);
    ladder.record(
        LadderRung::Provider,
        "acme-slow",
        StepOutcome::Skipped,
        "key quota window exhausted",
    );
    ladder.record(
        LadderRung::Provider,
        "acme-hero",
        StepOutcome::Selected,
        "top-ranked candidate",
    );
    ladder.persist();
    let text = std::fs::read_to_string(&trace).expect("trace written");
    let v: serde_json::Value = serde_json::from_str(text.trim()).expect("json");
    assert_eq!(v["solo"]["served_by"], "acme-hero");
    assert_eq!(v["solo"]["rung"], "Provider");
    assert_eq!(v["solo"]["alone_because"][0]["candidate"], "acme-slow");
    assert_eq!(
        v["solo"]["alone_because"][0]["reason"],
        "key quota window exhausted"
    );
    unsafe {
        std::env::remove_var("SUSI_ROUTING_TRACE_FILE");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn primary_without_secondaries_no_selection_is_not_solo() {
    // Nobody served at all: that is a failure (a different thing), not the
    // solo path — served_alone must say None.
    let mut ladder = RoutingLadder::new(TaskClass::Chat);
    ladder.record(
        LadderRung::Provider,
        "acme-a",
        StepOutcome::Failed,
        "empty response",
    );
    ladder.record(
        LadderRung::Provider,
        "acme-b",
        StepOutcome::Failed,
        "transport error",
    );
    assert_eq!(ladder.served_alone(), None);
}

#[test]
fn primary_without_secondaries_production_wired() {
    // The solo verdict is computed on the real dispatch path: persist()
    // fills it in and the runtime persists at every resolution exit.
    let routing_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/engines/routing.rs"
    ))
    .expect("routing.rs readable");
    assert!(
        routing_src.contains("self.solo = self.served_alone()"),
        "persist() must record the solo verdict on every mission"
    );
    let runtime_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/engines/runtime.rs"
    ))
    .expect("runtime.rs readable");
    assert!(
        runtime_src.contains("ladder.persist()"),
        "the production dispatch persists the ladder — solo verdict included"
    );
}
