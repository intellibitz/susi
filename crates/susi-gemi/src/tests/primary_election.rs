//! Elect the primary brain from the fully-working candidates (VC-202-022,
//! T-DEEPSEEK-121). The primary for a task class is elected only from
//! providers both available and capable above the floor, chosen by cost
//! per verified outcome — and the election is journaled with the evidence
//! for the choice, re-run by the scout whenever health, price or
//! capability moves.

use crate::engines::brain::{FailureKind, Ranked, TaskClass};
use crate::primary_election::{elect, Ballot, Rejection};

fn candidate(
    provider: &str,
    meets_floor: bool,
    unfit: bool,
    cost_per_outcome: Option<f64>,
) -> Ranked {
    Ranked {
        provider: provider.to_string(),
        meets_floor,
        capability: if meets_floor { "reasoning" } else { "basic" },
        unfit,
        last_failure: unfit.then_some((FailureKind::Transport, 2)),
        score: 1.0,
        cost_tier: "low",
        expected_cost_usd: cost_per_outcome,
        cost_per_outcome_usd: cost_per_outcome,
        billing: "metered",
        quota_remaining: None,
        effective_cost_usd: cost_per_outcome,
        samples: 5,
        success_rate: Some(0.9),
        avg_latency_ms: Some(100),
    }
}

#[test]
fn primary_election_picks_cheapest_per_verified_outcome_and_records_why() {
    // `rank` order is already cheapest cost per verified outcome first;
    // the election takes the first *fully working* entry and journals both
    // the winner's evidence and why the runner-up lost.
    let ranked = vec![
        candidate("acme-cheap", true, false, Some(0.001)),
        candidate("acme-pricy", true, false, Some(0.009)),
    ];
    let e = elect(&ranked, TaskClass::Chat, 1_000).expect("a working field elects");
    assert_eq!(e.primary, "acme-cheap");
    assert_eq!(e.cost_per_outcome_usd, Some(0.001));
    assert_eq!(e.success_rate, Some(0.9));
    assert_eq!(e.samples, 5);
    assert_eq!(e.field, 2);
    assert_eq!(
        e.rejected,
        vec![Rejection::Pricier {
            provider: "acme-pricy".to_string(),
            cost_per_outcome_usd: Some(0.009),
        }],
        "the loser is named with the price it lost on"
    );
}

#[test]
fn primary_election_never_elects_unfit_or_below_floor() {
    // Health and capability are eligibility, not a discount factor: an
    // unfit provider and a below-floor one lose to a working candidate at
    // any price, each with its reason recorded.
    let ranked = vec![
        candidate("acme-dead", true, true, Some(0.0001)),
        candidate("acme-weak", false, false, Some(0.0002)),
        candidate("acme-working", true, false, Some(0.01)),
    ];
    let e = elect(&ranked, TaskClass::Code, 1_000).expect("one working elects");
    assert_eq!(
        e.primary, "acme-working",
        "the only fully-working candidate"
    );
    assert!(e.rejected.iter().any(
        |r| matches!(r, Rejection::Unfit { provider, last_failure: Some(f) }
            if provider == "acme-dead" && f.contains("Transport"))
    ));
    assert!(e.rejected.iter().any(
        |r| matches!(r, Rejection::BelowFloor { provider, capability }
            if provider == "acme-weak" && capability == "basic")
    ));
}

#[test]
fn primary_election_sole_working_candidate_is_primary_by_default() {
    // One fully working model is primary by default, not by exception —
    // the election is a normal result, not a degenerate case.
    let ranked = vec![candidate("acme-only", true, false, Some(0.005))];
    let e = elect(&ranked, TaskClass::Chat, 1_000).expect("sole candidate elects");
    assert_eq!(e.primary, "acme-only");
    assert_eq!(e.field, 1);
    assert!(e.rejected.is_empty());
}

#[test]
fn primary_election_no_fully_working_candidate_elects_nobody() {
    // All unhealthy or all below the floor: there is no primary — the
    // ladder may still step down at dispatch, but nobody is *elected*.
    let ranked = vec![
        candidate("acme-dead", true, true, Some(0.001)),
        candidate("acme-weak", false, false, Some(0.001)),
    ];
    assert_eq!(elect(&ranked, TaskClass::Reasoning, 1_000), None);
    assert_eq!(elect(&[], TaskClass::Chat, 1_000), None);
}

#[test]
fn primary_election_reelects_and_journals_when_health_moves() {
    let _env = crate::engines::env_test_lock();
    let evidence = std::env::temp_dir().join(format!("susi-elect-{}", std::process::id()));
    let _ = std::fs::remove_file(&evidence);
    // SAFETY: serialized by env_test_lock; restored before drop.
    unsafe {
        std::env::set_var("SUSI_BRAIN_EVIDENCE_FILE", &evidence);
    }
    let _ = crate::engines::brain::reset();
    let names = vec!["acme-eld-a".to_string(), "acme-eld-b".to_string()];
    // Both proven at Chat; alphabetical rank order elects acme-eld-a while
    // the evidence cannot discriminate — static order is the tiebreak.
    for _ in 0..4 {
        crate::engines::brain::record_outcome("acme-eld-a", TaskClass::Chat, true, 100);
        crate::engines::brain::record_outcome("acme-eld-b", TaskClass::Chat, true, 100);
    }
    let dir = std::env::temp_dir().join(format!("susi-elect-run-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let journal = dir.join("runs.jsonl");
    crate::scout_schedule::finish_run(1_000, &journal, &names, &[]);
    let elections_path = dir.join("brain_elections.jsonl");
    let first =
        crate::primary_election::load_last(&elections_path).expect("first ballot journaled");
    let chat = first
        .elections
        .iter()
        .find(|e| e.class == "chat")
        .expect("chat elected");
    assert_eq!(chat.primary, "acme-eld-a");
    assert_eq!(chat.field, 2, "both proven providers were eligible");

    // The primary dies — health moved, so the next scout re-elects the
    // runner-up and journals a second ballot; the first stays as evidence.
    // Unfit needs ≥3 samples below the 0.34 rate plus a recent failure:
    // 4 oks + 9 fails = 0.31 — genuinely dead, not merely outscored.
    for _ in 0..9 {
        crate::engines::brain::record_outcome("acme-eld-a", TaskClass::Chat, false, 0);
    }
    crate::engines::brain::note_failure("acme-eld-a", FailureKind::Transport);
    crate::scout_schedule::finish_run(2_000, &journal, &names, &[]);
    let second =
        crate::primary_election::load_last(&elections_path).expect("second ballot journaled");
    let chat2 = second
        .elections
        .iter()
        .find(|e| e.class == "chat")
        .expect("chat re-elected");
    assert_eq!(
        chat2.primary, "acme-eld-b",
        "the dead primary is succeeded by the surviving fit candidate"
    );
    assert!(chat2
        .rejected
        .iter()
        .any(|r| matches!(r, Rejection::Unfit { provider, .. } if provider == "acme-eld-a")));
    let text = std::fs::read_to_string(&elections_path).expect("journal readable");
    assert_eq!(
        text.lines().filter(|l| !l.trim().is_empty()).count(),
        2,
        "one ballot per scout run — the re-election keeps the first as evidence"
    );

    unsafe {
        std::env::remove_var("SUSI_BRAIN_EVIDENCE_FILE");
    }
    let _ = crate::engines::brain::reset();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn primary_election_ballot_round_trips() {
    let ballot = Ballot {
        unix: 1_000,
        elections: vec![elect(
            &[candidate("acme-a", true, false, Some(0.001))],
            TaskClass::Chat,
            1_000,
        )
        .expect("elects")],
    };
    let dir = std::env::temp_dir().join(format!("susi-elect-json-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("elections.jsonl");
    crate::primary_election::append(&path, &ballot).expect("append");
    let back = crate::primary_election::load_last(&path).expect("round-trips");
    assert_eq!(back, ballot);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn primary_election_production_wiring() {
    // The scheduled scout re-elects every run over the same fresh evidence
    // — health, price and capability moves re-elect automatically — and the
    // ballot is journaled where a human can read it.
    let sched = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/scout_schedule.rs"
    ))
    .expect("scout_schedule.rs readable");
    assert!(
        sched.contains("primary_election::elect_all"),
        "the scout re-elects the primary every run"
    );
    assert!(
        sched.contains("brain_elections.jsonl"),
        "each election's evidence is journaled"
    );
}
