//! T-DEEPSEEK-122 mastery tests (VC-202-022): the primary orchestrates
//! secondaries — each goal goes to a working, floor-meeting secondary whose
//! marginal cost does not exceed doing the work on the primary, fan-out is
//! bounded by rate allowance and price, every delegated result keeps its
//! provenance, the synthesis is the primary's own, and the whole topology
//! is reconstructible from the journaled trace.
//!
//! `assign` is the decision the production bus arm makes; `orchestrate`
//! runs it through the pinned-generation primitive. Tests inject the
//! ranking view, the headroom view and dispatch so no model is needed; the
//! wiring test asserts the production path really composes these pieces.

use crate::engines::brain::{Ranked, TaskClass};
use crate::orchestration::{self, Delegation, HeldBack};

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

fn no_headroom(_: &str) -> Option<(u64, u64)> {
    None
}

/// One ranking view: `names` in this order for every class — the primary
/// is elected as the first working candidate; secondaries follow.
fn fixed_order<'a>(names: &'a [&'a str], cost: f64) -> impl Fn(TaskClass) -> Vec<Ranked> + 'a {
    move |_| names.iter().map(|n| ranked(n, Some(cost))).collect()
}

#[test]
fn orchestration_topology_delegates_to_working_secondary() {
    let names = ["acme-prime", "acme-second"];
    let goals =
        vec!["summarize what changed in the release notes for the team update today".to_string()];
    let topology = orchestration::assign(
        &fixed_order(&names, 0.001),
        &no_headroom,
        TaskClass::Code,
        &goals,
        100,
    )
    .expect("a fully-working field elects a primary");
    assert_eq!(topology.primary, "acme-prime");
    assert_eq!(topology.delegations.len(), 1);
    let d = &topology.delegations[0];
    assert!(
        d.delegated && d.provider == "acme-second",
        "a working, equally-priced secondary takes the goal: {d:?}"
    );
    assert_eq!(d.why_not_delegated, None);
    assert_eq!(topology.fanned_out, 1);
}

#[test]
fn orchestration_topology_cost_bound_keeps_pricier_work_home() {
    let ranked_for = |class: TaskClass| match class {
        // The secondary is pricier *for this class* — delegation would cost
        // more than doing the work, so the goal stays with the primary.
        TaskClass::Code => vec![
            ranked("acme-prime", Some(0.001)),
            ranked("acme-luxe", Some(0.01)),
        ],
        _ => vec![ranked("acme-prime", Some(0.001))],
    };
    let goals = vec!["refactor the cache module now".to_string()];
    let topology = orchestration::assign(&ranked_for, &no_headroom, TaskClass::Code, &goals, 100)
        .expect("field elects");
    let d = &topology.delegations[0];
    assert!(!d.delegated && d.provider == "acme-prime");
    assert_eq!(
        d.why_not_delegated,
        Some(HeldBack::SecondaryPricier {
            candidate: "acme-luxe".to_string()
        }),
        "the trace says why the goal stayed home: {d:?}"
    );
    assert_eq!(topology.fanned_out, 0);
}

#[test]
fn orchestration_topology_fan_out_bounded_by_rate_allowance() {
    let names = ["acme-prime", "acme-second"];
    // One delegated call of rate headroom on the secondary — two goals.
    let headroom = |p: &str| {
        if p == "acme-second" {
            Some((1, 4))
        } else {
            None
        }
    };
    let goals = vec![
        "summarize what changed in the release notes for the team update today".to_string(),
        "summarize how the release schedule moved for the team update today".to_string(),
    ];
    let topology = orchestration::assign(
        &fixed_order(&names, 0.001),
        &headroom,
        TaskClass::Code,
        &goals,
        100,
    )
    .expect("field elects");
    assert_eq!(topology.fanned_out, 1, "allowance 1 admits exactly one");
    let held = &topology.delegations[1];
    assert!(!held.delegated && held.provider == "acme-prime");
    assert_eq!(
        held.why_not_delegated,
        Some(HeldBack::SecondaryRateBound {
            candidate: "acme-second".to_string()
        })
    );
}

#[test]
fn orchestration_topology_capped_secondary_never_delegated() {
    let capped = || {
        let mut r = ranked("acme-second", Some(0.0005));
        r.quota_remaining = Some(0.0);
        r.effective_cost_usd = Some(f64::MAX);
        r
    };
    let ranked_for = move |_: TaskClass| vec![ranked("acme-prime", Some(0.001)), capped()];
    let goals =
        vec!["summarize what changed in the release notes for the team update today".to_string()];
    let topology = orchestration::assign(&ranked_for, &no_headroom, TaskClass::Code, &goals, 100)
        .expect("field elects");
    let d = &topology.delegations[0];
    assert!(!d.delegated && d.provider == "acme-prime");
    assert_eq!(
        d.why_not_delegated,
        Some(HeldBack::SecondaryCapped {
            candidate: "acme-second".to_string()
        }),
        "a spent cap cannot take work even priced cheaper: {d:?}"
    );
}

#[test]
fn orchestration_topology_synthesis_is_the_primaries_own() {
    let calls = std::sync::Mutex::new(Vec::<(String, String)>::new());
    let dispatch = |provider: &str, prompt: &str| -> Result<String, String> {
        calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((provider.to_string(), prompt.to_string()));
        Ok(format!("{provider} answered"))
    };
    let names = ["acme-prime", "acme-second"];
    let goals =
        vec!["summarize what changed in the release notes for the team update today".to_string()];
    let (topology, answer) = orchestration::orchestrate(
        "repair all of the tooling in the workshop before the day ends today",
        &goals,
        &fixed_order(&names, 0.001),
        &no_headroom,
        &dispatch,
        200,
    )
    .expect("orchestration runs");
    let calls = calls.lock().unwrap_or_else(|e| e.into_inner());
    // Goal dispatch on the secondary; synthesis dispatch on the primary.
    assert_eq!(calls[0].0, "acme-second");
    assert_eq!(calls.last().map(|(p, _)| p.as_str()), Some("acme-prime"));
    assert!(
        calls
            .last()
            .is_some_and(|(_, p)| p.contains("repair all of the tooling")),
        "the synthesis brief carries the mission and its results"
    );
    assert_eq!(
        answer, "acme-prime answered",
        "the final answer is the primary's own"
    );
    assert!(topology.synthesis_ok);
    assert!(
        topology.delegations[0].ok && topology.delegations[0].answer.is_some(),
        "each delegated result keeps what its worker returned"
    );
}

#[test]
fn orchestration_topology_reconstructible_from_the_journal() {
    let dir = std::env::temp_dir().join(format!("susi-orch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let journal = dir.join("orchestrations.jsonl");
    let dispatch = |provider: &str, _: &str| Ok(format!("{provider} said so"));
    let names = ["acme-prime", "acme-second"];
    let goals = vec![
        "summarize what changed in the release notes for the team update today".to_string(),
        "summarize how the release schedule moved for the team update today".to_string(),
    ];
    let (topology, _) = orchestration::orchestrate(
        "repair all of the tooling in the workshop before the day ends today",
        &goals,
        &fixed_order(&names, 0.001),
        &no_headroom,
        &dispatch,
        300,
    )
    .expect("orchestration runs");
    orchestration::append(&journal, &topology).expect("append");
    let loaded = orchestration::load_last(&journal).expect("journal reads back");
    assert_eq!(loaded.primary, "acme-prime");
    assert_eq!(loaded.mission_class, "chat");
    assert_eq!(loaded.fanned_out, topology.fanned_out);
    // Who did what, on which model, at what cost — reconstructible.
    let delegated: Vec<&Delegation> = loaded.delegations.iter().filter(|d| d.delegated).collect();
    for d in &delegated {
        assert_eq!(d.provider, "acme-second");
        assert!(d.effective_cost_usd.is_some(), "the price is in the trace");
        assert!(d.ok);
    }
    // Payload-free: the journal keeps the topology, not answer text.
    let line = std::fs::read_to_string(&journal).expect("journal file");
    assert!(
        !line.contains("said so"),
        "the trace records who served, not what they said: {line}"
    );
    assert!(loaded.delegations.iter().all(|d| d.answer.is_none()));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn orchestration_topology_production_wiring() {
    // The orchestration must live on the production dispatch path, not
    // beside it.
    let handler =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/plane_handler.rs"))
            .expect("plane_handler.rs readable");
    assert!(
        handler.contains("gemi.orchestrate.mission"),
        "the bus exposes the orchestration entry"
    );
    assert!(
        handler.contains("orchestration::orchestrate_mission"),
        "the bus arm runs the production orchestrator"
    );

    let orch =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/orchestration.rs"))
            .expect("orchestration.rs readable");
    for needle in [
        "primary_election::elect",
        "rank_with_quota",
        "remaining_fraction_unified",
        "worker::headroom",
        "generate_reasoning_deep_with_model",
        "MissionPlanner::plan_mission",
    ] {
        assert!(
            orch.contains(needle),
            "the production orchestrator composes {needle}"
        );
    }
}
