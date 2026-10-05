//! T-DEEPSEEK-124 mastery tests (VC-202-022): a primary that times out or
//! dies mid-mission fails over — the next fully-working candidate is
//! elected, the mission resumes from the work already done instead of
//! restarting, and the switch, the work done and the cost on each model
//! are recorded on the journaled topology.
//!
//! `orchestrate` is the decision the production bus arm makes; tests
//! inject the ranking view, the headroom view and dispatch so no model is
//! needed; the wiring test asserts the production path really carries
//! the failover.

use crate::engines::brain::{Ranked, TaskClass};
use crate::orchestration::{self, Failover};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

fn ranked(provider: &str, effective_cost_usd: Option<f64>) -> Ranked {
    Ranked {
        provider: provider.to_string(),
        meets_floor: true,
        capability: "reasoning",
        unfit: false,
        last_failure: None,
        score: 1.0,
        cost_tier: "low",
        expected_cost_usd: effective_cost_usd,
        cost_per_outcome_usd: effective_cost_usd,
        billing: "metered",
        quota_remaining: None,
        effective_cost_usd,
        samples: 10,
        success_rate: Some(0.9),
        avg_latency_ms: Some(100),
    }
}

fn unfit(provider: &str, effective_cost_usd: Option<f64>) -> Ranked {
    Ranked {
        unfit: true,
        ..ranked(provider, effective_cost_usd)
    }
}

fn no_headroom(_: &str) -> Option<(u64, u64)> {
    None
}

/// A dispatch fake: providers in `dead_names` fail every call with the
/// given reason; everyone else answers `ans-from-<provider>`. Every call
/// is journaled so tests can assert who was asked what, and that
/// finished work was never re-asked.
type CallLog = Arc<Mutex<Vec<(String, String)>>>;

fn fake_dispatch(
    dead_names: &'static [&'static str],
    log: CallLog,
) -> impl Fn(&str, &str) -> Result<String, String> {
    move |provider: &str, prompt: &str| {
        log.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((provider.to_string(), prompt.to_string()));
        if dead_names.contains(&provider) {
            Err(format!("{provider}: timeout — deadline exceeded"))
        } else {
            Ok(format!("ans-from-{provider}"))
        }
    }
}

fn calls_to(log: &CallLog, provider: &str) -> usize {
    log.lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(p, _)| p == provider)
        .count()
}

/// Count direct goal dispatches — the synthesis brief quotes every goal
/// inside its prompt, so matching must be exact, not substring.
fn calls_for(log: &CallLog, provider: &str, goal: &str) -> usize {
    log.lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(p, prompt)| p == provider && prompt == goal)
        .count()
}

fn scratch_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("susi-primary-failover-{tag}-{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

#[test]
fn primary_failover_resume_elects_successor_and_keeps_completed_work() {
    // Three candidates. The reflex goal delegates to the second; the
    // code goal stays with the primary (the second is unfit for code and
    // the third is priced above the bound). The primary dies on the code
    // goal; the second is elected successor and finishes the mission.
    let goals = vec![
        "summarize the release notes for the team update".to_string(),
        "refactor the parser to handle empty input".to_string(),
    ];
    let ranked_for = |class: TaskClass| -> Vec<Ranked> {
        match class {
            TaskClass::Code => vec![
                ranked("acme-prime", Some(0.001)),
                unfit("acme-second", Some(0.002)),
                ranked("acme-third", Some(0.005)),
            ],
            _ => vec![
                ranked("acme-prime", Some(0.001)),
                ranked("acme-second", Some(0.001)),
                ranked("acme-third", Some(0.005)),
            ],
        }
    };
    let log: CallLog = Arc::new(Mutex::new(Vec::new()));
    let dispatch = fake_dispatch(&["acme-prime"], Arc::clone(&log));

    let (topology, answer) = orchestration::orchestrate(
        "mission alpha brief",
        &goals,
        &ranked_for,
        &no_headroom,
        &dispatch,
        100,
    )
    .expect("orchestrate");

    // One switch, recorded with both models, the failing goal, the work
    // already done and the verbatim reason.
    assert_eq!(topology.failovers.len(), 1, "{topology:?}");
    let f: &Failover = &topology.failovers[0];
    assert_eq!(f.from, "acme-prime");
    assert_eq!(f.to.as_deref(), Some("acme-second"));
    assert_eq!(f.goal.as_deref(), Some(goals[1].as_str()));
    assert_eq!(f.completed_goals, 1, "the delegated reflex goal was done");
    assert!(f.reason.contains("timeout"), "reason: {}", f.reason);

    // Resume, not restart: the delegated goal was asked once and its
    // answer kept; the dead primary was asked once and never retried.
    assert_eq!(calls_for(&log, "acme-second", &goals[0]), 1);
    assert_eq!(calls_for(&log, "acme-second", &goals[1]), 1);
    assert_eq!(calls_to(&log, "acme-prime"), 1);
    assert!(topology.delegations[0].delegated);
    assert!(topology.delegations[0].ok);
    assert_eq!(
        topology.delegations[0].answer.as_deref(),
        Some("ans-from-acme-second")
    );

    // The failed-over goal records who actually served it and that
    // worker's price — cost on each model stays attributable.
    let rescued = &topology.delegations[1];
    assert!(!rescued.delegated);
    assert!(rescued.ok);
    assert_eq!(rescued.provider, "acme-second");
    assert_eq!(rescued.effective_cost_usd, Some(0.002));
    assert_eq!(rescued.answer.as_deref(), Some("ans-from-acme-second"));

    // The synthesis ran on the successor and is the mission's answer.
    assert!(topology.synthesis_ok);
    assert_eq!(answer, "ans-from-acme-second");
    assert_eq!(topology.primary, "acme-prime", "elected primary unchanged");
}

#[test]
fn primary_failover_resume_synthesis_switches_to_successor() {
    // Everything delegated cleanly; the primary dies only on the
    // synthesis call — the switch must still be elected and recorded,
    // with no goal in flight.
    let goals = vec!["summarize the release notes for the team update".to_string()];
    let ranked_for = |_: TaskClass| -> Vec<Ranked> {
        vec![
            ranked("acme-prime", Some(0.001)),
            ranked("acme-second", Some(0.001)),
        ]
    };
    let log: CallLog = Arc::new(Mutex::new(Vec::new()));
    let dispatch = {
        let log = Arc::clone(&log);
        move |provider: &str, prompt: &str| -> Result<String, String> {
            log.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((provider.to_string(), prompt.to_string()));
            if provider == "acme-prime" && prompt.contains("Results to synthesize") {
                Err("acme-prime: connection reset".to_string())
            } else {
                Ok(format!("ans-from-{provider}"))
            }
        }
    };

    let (topology, answer) = orchestration::orchestrate(
        "mission beta",
        &goals,
        &ranked_for,
        &no_headroom,
        &dispatch,
        200,
    )
    .expect("orchestrate");

    assert_eq!(topology.failovers.len(), 1, "{topology:?}");
    let f = &topology.failovers[0];
    assert_eq!(f.from, "acme-prime");
    assert_eq!(f.to.as_deref(), Some("acme-second"));
    assert_eq!(f.goal, None, "the synthesis call carries no goal");
    assert_eq!(f.completed_goals, 1);
    assert!(topology.synthesis_ok);
    assert_eq!(answer, "ans-from-acme-second");
    // The secondary answered its goal once and then the retried
    // synthesis once — the completed goal was never re-asked.
    assert_eq!(calls_to(&log, "acme-second"), 2);
}

#[test]
fn primary_failover_resume_exhausted_field_fails_cleanly() {
    // A sole candidate dies mid-mission: no successor exists, so the
    // record says the field was exhausted — the work stays undone, the
    // mission reports honestly instead of pretending, and nothing loops.
    let goals = vec!["summarize the release notes for the team update".to_string()];
    let ranked_for = |_: TaskClass| -> Vec<Ranked> { vec![ranked("acme-prime", Some(0.001))] };
    let log: CallLog = Arc::new(Mutex::new(Vec::new()));
    let dispatch = fake_dispatch(&["acme-prime"], Arc::clone(&log));

    let (topology, _answer) = orchestration::orchestrate(
        "mission gamma",
        &goals,
        &ranked_for,
        &no_headroom,
        &dispatch,
        300,
    )
    .expect("orchestrate returns the degraded record, not a crash");

    assert!(!topology.delegations[0].ok);
    assert!(!topology.synthesis_ok);
    assert!(topology.failovers.iter().all(|f| f.to.is_none()));
    assert!(
        topology.failovers.iter().all(|f| f.from == "acme-prime"),
        "{:?}",
        topology.failovers
    );
    // The dead primary is re-asked only at the next primary duty —
    // a bounded number of calls, never an unbounded retry loop.
    assert!(calls_to(&log, "acme-prime") <= 2);
}

#[test]
fn primary_failover_resume_journaled_switch_round_trips() {
    // The switch is durable: appended to the orchestrations journal, the
    // failover record — both models, the work done, the goal in flight —
    // reads back intact alongside per-model cost attribution.
    let dir = scratch_dir("journal");
    let journal = dir.join("orchestrations.jsonl");
    let goals = vec![
        "summarize the release notes for the team update".to_string(),
        "refactor the parser to handle empty input".to_string(),
    ];
    let ranked_for = |class: TaskClass| -> Vec<Ranked> {
        match class {
            TaskClass::Code => vec![
                ranked("acme-prime", Some(0.001)),
                unfit("acme-second", Some(0.002)),
                ranked("acme-third", Some(0.005)),
            ],
            _ => vec![
                ranked("acme-prime", Some(0.001)),
                ranked("acme-second", Some(0.001)),
                ranked("acme-third", Some(0.005)),
            ],
        }
    };
    let log: CallLog = Arc::new(Mutex::new(Vec::new()));
    let dispatch = fake_dispatch(&["acme-prime"], Arc::clone(&log));

    let (topology, _) = orchestration::orchestrate(
        "mission delta",
        &goals,
        &ranked_for,
        &no_headroom,
        &dispatch,
        400,
    )
    .expect("orchestrate");
    orchestration::append(&journal, &topology).expect("append");

    let loaded = orchestration::load_last(&journal).expect("journal round-trips");
    // `answer` is serde-skipped (the journal keeps topology, not
    // payloads) — compare the durable record field by field.
    assert_eq!(loaded.mission_class, topology.mission_class);
    assert_eq!(loaded.primary, topology.primary);
    assert_eq!(loaded.fanned_out, topology.fanned_out);
    assert_eq!(loaded.synthesis_ok, topology.synthesis_ok);
    assert_eq!(loaded.failovers, topology.failovers);
    assert_eq!(loaded.failovers.len(), 1);
    assert_eq!(loaded.failovers[0].from, "acme-prime");
    assert_eq!(loaded.failovers[0].to.as_deref(), Some("acme-second"));
    assert_eq!(loaded.failovers[0].completed_goals, 1);
    // Cost on each model: the delegated goal prices acme-second, the
    // failed-over goal prices the successor — both readable off the
    // journaled delegations.
    assert_eq!(loaded.delegations[0].provider, "acme-second");
    assert_eq!(loaded.delegations[0].effective_cost_usd, Some(0.001));
    assert_eq!(loaded.delegations[1].provider, "acme-second");
    assert_eq!(loaded.delegations[1].effective_cost_usd, Some(0.002));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn primary_failover_resume_production_wiring() {
    // The failover is on the production path, not test-side scaffolding:
    // `orchestrate` routes every primary duty through the failover loop,
    // the successor is elected over survivors with the same ballot the
    // scout uses, the switch is a journaled topology field, and the bus
    // arm still reaches `orchestrate_mission`.
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/orchestration.rs"))
        .expect("orchestration.rs");
    assert!(
        src.matches("serve_on_primary").count() >= 4,
        "primary-served goals, rescue and synthesis must all fail over"
    );
    assert!(
        src.contains("primary_election::elect(&survivors"),
        "the successor must be elected over the surviving field"
    );
    assert!(
        src.contains("pub failovers: Vec<Failover>"),
        "the switch must be a journaled topology field"
    );
    assert!(
        src.contains("orchestrate(") && src.contains("pub fn orchestrate_mission"),
        "orchestrate_mission must run the failover-carrying path"
    );
    let handler =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/plane_handler.rs"))
            .expect("plane_handler.rs");
    assert!(
        handler.contains("gemi.orchestrate.mission") && handler.contains("orchestrate_mission"),
        "the bus arm must reach the orchestrator"
    );
}
