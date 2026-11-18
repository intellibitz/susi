//! T-DEEPSEEK-126 mastery tests (VC-202-023): what a secondary produced is
//! verified to a depth set by the task's risk — a deterministic check for
//! low-risk work, an independent model verifier for code and reasoning —
//! the checker's price is inside the delegation bound, an unverified or
//! refuted delegation is never presented as the primary's own result, and
//! the record names who produced what and who checked it.

use crate::engines::brain::{Ranked, TaskClass};
use crate::orchestration::{self, assign, orchestrate, Delegation, HeldBack, Verification};

fn metered(provider: &str, effective_cost_usd: f64) -> Ranked {
    Ranked {
        provider: provider.to_string(),
        meets_floor: true,
        capability: "reasoning",
        unfit: false,
        last_failure: None,
        score: 1.0,
        cost_tier: "low",
        expected_cost_usd: Some(effective_cost_usd),
        cost_per_outcome_usd: Some(effective_cost_usd),
        billing: "metered",
        quota_remaining: None,
        effective_cost_usd: Some(effective_cost_usd),
        samples: 10,
        success_rate: Some(0.9),
        avg_latency_ms: Some(100),
    }
}

/// A subscription worker under its cap — zero marginal, unpriced by the
/// catalog.
fn seat(provider: &str) -> Ranked {
    Ranked {
        billing: "subscription",
        quota_remaining: Some(0.5),
        effective_cost_usd: None,
        expected_cost_usd: None,
        cost_per_outcome_usd: None,
        ..metered(provider, 0.0)
    }
}

fn no_headroom(_: &str) -> Option<(u64, u64)> {
    None
}

const CODE_GOAL: &str = "refactor the parser module";

#[test]
fn delegated_verification_by_risk_low_risk_uses_the_deterministic_check() {
    let calls = std::sync::Mutex::new(Vec::<String>::new());
    let dispatch = |provider: &str, _: &str| -> Result<String, String> {
        calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(provider.to_string());
        Ok("a perfectly fine chat answer".to_string())
    };
    let ranked_for =
        |_: TaskClass| vec![metered("acme-prime", 0.001), metered("acme-second", 0.001)];
    // A chat-class goal — low risk.
    let goals = vec!["summarize the release notes for the team update today".to_string()];
    let (topology, _) = orchestrate(
        "collect the weekly notes for the team update today",
        &goals,
        &ranked_for,
        &no_headroom,
        &dispatch,
        100,
    )
    .expect("orchestrates");
    let d = &topology.delegations[0];
    assert!(d.delegated && d.provider == "acme-second");
    assert_eq!(d.verify_with, None, "low risk binds no model verifier");
    assert_eq!(
        d.verification,
        Some(Verification::Deterministic { ok: true })
    );
    assert!(d.ok);
    // No verification dispatch — only the goal call and the synthesis.
    let calls = calls.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls.len(), 2, "goal + synthesis, no verifier call");
}

#[test]
fn delegated_verification_by_risk_high_risk_calls_an_independent_verifier() {
    let calls = std::sync::Mutex::new(Vec::<(String, String)>::new());
    let dispatch = |provider: &str, prompt: &str| -> Result<String, String> {
        calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((provider.to_string(), prompt.to_string()));
        if prompt.contains("VERIFIED") {
            return Ok("VERIFIED".to_string());
        }
        Ok("patched code".to_string())
    };
    // A free (subscription) secondary is the only delegation that beats
    // doing high-risk work plus verification on the primary.
    let ranked_for = |_: TaskClass| vec![metered("acme-prime", 0.001), seat("acme-seat")];
    let goals = vec![CODE_GOAL.to_string()];
    let (topology, _) = orchestrate(
        "harden the parsing layer of the system",
        &goals,
        &ranked_for,
        &no_headroom,
        &dispatch,
        100,
    )
    .expect("orchestrates");
    let d = &topology.delegations[0];
    assert!(d.delegated && d.provider == "acme-seat");
    assert_eq!(
        d.verify_with.as_deref(),
        Some("acme-prime"),
        "the checker is bound at assignment: never the producer"
    );
    assert_eq!(
        d.verification,
        Some(Verification::Model {
            verifier: "acme-prime".to_string(),
            ok: true,
            verdict: "VERIFIED".to_string(),
        }),
        "the record names who checked the result and what it decided"
    );
    assert!(d.ok);
    let calls = calls.lock().unwrap_or_else(|e| e.into_inner());
    assert!(
        calls
            .iter()
            .any(|(p, prompt)| p == "acme-prime" && prompt.contains("VERIFIED")),
        "the verifier was actually asked: {calls:?}"
    );
}

#[test]
fn delegated_verification_by_risk_verification_cost_is_in_the_routing_decision() {
    // For a code goal, delegating to a metered secondary and verifying on
    // the primary costs strictly more than the primary doing the work.
    let ranked_for =
        |_: TaskClass| vec![metered("acme-prime", 0.001), metered("acme-second", 0.0005)];
    let goals = vec![CODE_GOAL.to_string()];
    let topology =
        assign(&ranked_for, &no_headroom, TaskClass::Code, &goals, 100).expect("field elects");
    let d = &topology.delegations[0];
    assert!(!d.delegated && d.provider == "acme-prime");
    assert_eq!(
        d.why_not_delegated,
        Some(HeldBack::SecondaryPricier {
            candidate: "acme-second".to_string()
        }),
        "secondary + verifier > primary: the checker's price is in the bound"
    );

    // A free secondary under a subscription cap delegates: production
    // costs $0 plus the verifier the bound already priced.
    let ranked_for = |_: TaskClass| vec![metered("acme-prime", 0.001), seat("acme-seat")];
    let topology =
        assign(&ranked_for, &no_headroom, TaskClass::Code, &goals, 100).expect("field elects");
    assert!(topology.delegations[0].delegated);
}

#[test]
fn delegated_verification_by_risk_refuted_result_is_never_the_primaries_own() {
    let calls = std::sync::Mutex::new(Vec::<(String, String)>::new());
    let dispatch = |provider: &str, prompt: &str| -> Result<String, String> {
        calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((provider.to_string(), prompt.to_string()));
        if prompt.contains("VERIFIED") {
            return Ok("REFUTED: the patch dereferences a moved value".to_string());
        }
        Ok(match provider {
            "acme-seat" => "let p = *borrowed;".to_string(),
            _ => "let p = borrowed.clone();".to_string(),
        })
    };
    let ranked_for = |_: TaskClass| vec![metered("acme-prime", 0.001), seat("acme-seat")];
    let goals = vec![CODE_GOAL.to_string()];
    let (topology, _) = orchestrate(
        "harden the parsing layer of the system",
        &goals,
        &ranked_for,
        &no_headroom,
        &dispatch,
        100,
    )
    .expect("orchestrates");
    let d = &topology.delegations[0];
    assert!(
        matches!(&d.verification, Some(Verification::Model { ok: false, .. })),
        "the refutation is recorded: {d:?}"
    );
    assert_eq!(
        d.rescued_by.as_deref(),
        Some("acme-prime"),
        "the primary re-served what the verifier refused"
    );
    assert_eq!(
        d.answer.as_deref(),
        Some("let p = borrowed.clone();"),
        "what ships is the primary's rescue, not the refuted answer"
    );
    assert!(d.ok);
    // The synthesis brief carries the rescue — the refuted text never
    // reaches the final answer as if the primary produced it.
    let calls = calls.lock().unwrap_or_else(|e| e.into_inner());
    let brief = calls.last().map(|(_, p)| p.clone()).unwrap_or_default();
    assert!(brief.contains("borrowed.clone()"), "rescue in the brief");
    assert!(
        !brief.contains("*borrowed;"),
        "the refuted answer never enters the synthesis: {brief}"
    );
    assert!(brief.contains("refuted"), "the brief marks the rescue");
}

#[test]
fn delegated_verification_by_risk_no_independent_verifier_no_delegation() {
    // A chat mission elects a chat-floor primary; the code goal has a
    // working secondary but no independent worker that meets the code
    // floor to check it — delegation cannot be verified, so it stays home.
    let ranked_for = |class: TaskClass| match class {
        TaskClass::Code => vec![
            {
                let mut r = metered("acme-prime", 0.001);
                r.meets_floor = false;
                r.capability = "basic";
                r
            },
            metered("acme-second", 0.0001),
        ],
        _ => vec![metered("acme-prime", 0.001), metered("acme-second", 0.0001)],
    };
    let goals = vec![CODE_GOAL.to_string()];
    let topology =
        assign(&ranked_for, &no_headroom, TaskClass::Chat, &goals, 100).expect("chat field elects");
    let d = &topology.delegations[0];
    assert!(!d.delegated && d.provider == "acme-prime");
    assert_eq!(
        d.why_not_delegated,
        Some(HeldBack::NoVerifier {
            candidate: "acme-second".to_string()
        }),
        "unverifiable delegation stays with the primary: {d:?}"
    );
}

#[test]
fn delegated_verification_by_risk_journal_names_producer_and_checker() {
    let dir = std::env::temp_dir().join(format!("susi-verify-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let journal = dir.join("orchestrations.jsonl");
    let dispatch = |provider: &str, prompt: &str| -> Result<String, String> {
        if prompt.contains("VERIFIED") {
            return Ok("VERIFIED".to_string());
        }
        Ok(format!("{provider} did the work"))
    };
    let ranked_for = |_: TaskClass| vec![metered("acme-prime", 0.001), seat("acme-seat")];
    let goals = vec![CODE_GOAL.to_string()];
    let (topology, _) = orchestrate(
        "harden the parsing layer of the system",
        &goals,
        &ranked_for,
        &no_headroom,
        &dispatch,
        200,
    )
    .expect("orchestrates");
    orchestration::append(&journal, &topology).expect("append");
    let loaded: Delegation = orchestration::load_last(&journal)
        .expect("journal reads")
        .delegations
        .into_iter()
        .next()
        .expect("one delegation");
    assert_eq!(loaded.provider, "acme-seat", "who produced it");
    assert!(
        matches!(&loaded.verification, Some(Verification::Model { verifier, .. }) if verifier == "acme-prime"),
        "who checked it and the verdict are reconstructible: {loaded:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn delegated_verification_by_risk_production_wiring() {
    let orch =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/orchestration.rs"))
            .expect("orchestration.rs readable");
    for needle in [
        "Verification::Model",
        "Verification::Deterministic",
        "looks_like_error_text",
        "verify_with",
        "rescued_by",
        "REFUTED",
    ] {
        assert!(
            orch.contains(needle),
            "risk-tiered verification lives on the orchestration path ({needle})"
        );
    }
}
