use super::*;
use crate::pricing::ModelPrice;

fn catalogue() -> PriceCatalogue {
    let mut models = BTreeMap::new();
    models.insert(
        "cheap".to_string(),
        ModelPrice {
            input_miss: MicroUsd::from_micros(100_000),
            input_hit: MicroUsd::from_micros(10_000),
            output: MicroUsd::from_micros(200_000),
        },
    );
    models.insert(
        "mid".to_string(),
        ModelPrice {
            input_miss: MicroUsd::from_micros(300_000),
            input_hit: MicroUsd::from_micros(30_000),
            output: MicroUsd::from_micros(600_000),
        },
    );
    models.insert(
        "capable".to_string(),
        ModelPrice {
            input_miss: MicroUsd::from_micros(1_000_000),
            input_hit: MicroUsd::from_micros(100_000),
            output: MicroUsd::from_micros(2_000_000),
        },
    );
    PriceCatalogue {
        schema: crate::catalog::PRICE_SCHEMA.to_string(),
        models,
    }
}

fn candidates() -> Vec<String> {
    vec!["capable".into(), "cheap".into(), "mid".into()]
}

#[test]
fn candidates_rank_cheapest_first_without_history() {
    let ranked = rank_models(
        &catalogue(),
        &candidates(),
        UsageProfile::agent(),
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        ranked.iter().map(|c| c.model.as_str()).collect::<Vec<_>>(),
        vec!["cheap", "mid", "capable"]
    );
    assert!(ranked[0].expected_cost < ranked[1].expected_cost);
}

#[test]
fn a_cheap_model_that_fails_sinks_below_a_reliable_one() {
    let mut history = BTreeMap::new();
    history.insert(
        "cheap".to_string(),
        ModelHistory {
            calls: 4,
            failures: 4,
            cost: MicroUsd::from_micros(4_000),
            ..ModelHistory::default()
        },
    );
    history.insert(
        "capable".to_string(),
        ModelHistory {
            calls: 4,
            failures: 0,
            cost: MicroUsd::from_micros(40_000),
            ..ModelHistory::default()
        },
    );
    let ranked = rank_models(&catalogue(), &candidates(), UsageProfile::agent(), &history).unwrap();
    let order: Vec<&str> = ranked.iter().map(|c| c.model.as_str()).collect();
    assert!(
        order.iter().position(|m| *m == "capable") < order.iter().position(|m| *m == "cheap"),
        "a model that never succeeds must rank behind a reliable one: {order:?}"
    );
}

#[test]
fn a_cheap_reliable_model_outranks_an_expensive_one() {
    let mut history = BTreeMap::new();
    history.insert(
        "cheap".to_string(),
        ModelHistory {
            calls: 2,
            failures: 0,
            cost: MicroUsd::from_micros(1_000),
            ..ModelHistory::default()
        },
    );
    history.insert(
        "capable".to_string(),
        ModelHistory {
            calls: 2,
            failures: 0,
            cost: MicroUsd::from_micros(4_000),
            ..ModelHistory::default()
        },
    );
    let ranked = rank_models(&catalogue(), &candidates(), UsageProfile::agent(), &history).unwrap();
    assert_eq!(ranked[0].model, "cheap");
}

#[test]
fn an_unpriced_candidate_ranks_last_instead_of_failing_the_run() {
    let ranked = rank_models(
        &catalogue(),
        &["mystery".into(), "cheap".into()],
        UsageProfile::agent(),
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(ranked[0].model, "cheap");
    assert_eq!(ranked[1].model, "mystery");
}

fn row(
    (task, attempt): (&str, u32),
    model: &str,
    provider: &str,
    at_ms: u64,
    outcome: Outcome,
) -> LedgerEntry {
    LedgerEntry::settle(crate::ledger::Dispatch {
        mission: "m".to_string(),
        task: task.to_string(),
        attempt,
        model: model.to_string(),
        provider: provider.to_string(),
        usage: crate::pricing::TokenUsage::new(1000, 800, 100),
        price: ModelPrice::default(),
        started_unix_ms: at_ms,
        ended_unix_ms: at_ms,
        outcome,
    })
}

fn rejected() -> Outcome {
    Outcome::Rejected {
        reason: "tests red".to_string(),
    }
}

fn failed(class: FailureClass) -> Outcome {
    Outcome::ModelFailed { class }
}

#[test]
fn provider_context_errors_are_classified_as_overflow() {
    for message in [
        "400: This model's maximum context length is 128000 tokens",
        r#"{"error":{"code":"context_length_exceeded"}}"#,
        "prompt is too long: 210000 tokens > 200000 maximum",
        "The input token count (1100000) exceeds the maximum number of tokens allowed",
    ] {
        assert_eq!(
            FailureKind::classify(message),
            FailureKind::ContextOverflow,
            "{message}"
        );
    }
    assert_eq!(
        FailureKind::classify("Rate limit reached for tokens per min"),
        FailureKind::Transient
    );
}

#[test]
fn a_context_overflow_skips_to_a_larger_window() {
    let ranked: Vec<String> = ["small", "also-small", "unknown", "large"]
        .into_iter()
        .map(String::from)
        .collect();
    let contexts: BTreeMap<String, u64> = [
        ("small".to_string(), 128_000),
        ("also-small".to_string(), 128_000),
        ("large".to_string(), 1_000_000),
    ]
    .into_iter()
    .collect();
    let mut ladder = Ladder::new(ranked.clone(), Policy::default()).with_contexts(contexts.clone());
    let NextAttempt::Attempt(first) = ladder.next_attempt() else {
        panic!("expected an attempt");
    };
    ladder.record(&first.model, FailureKind::ContextOverflow, "context length");
    let NextAttempt::Attempt(second) = ladder.next_attempt() else {
        panic!("expected an attempt");
    };
    // `also-small` holds no more, so it is skipped; an unknown window may.
    assert_eq!(second.model, "unknown");
    ladder.record(
        &second.model,
        FailureKind::ContextOverflow,
        "context length",
    );
    let NextAttempt::Attempt(third) = ladder.next_attempt() else {
        panic!("expected an attempt");
    };
    assert_eq!(third.model, "large");
    ladder.record(&third.model, FailureKind::ContextOverflow, "context length");
    assert!(matches!(
        ladder.next_attempt(),
        NextAttempt::Stop(StopReason::Stuck { .. })
    ));
}

#[test]
fn latency_is_the_mean_of_completed_provider_calls() {
    let timed = |at: u64, took: u64, outcome: Outcome| {
        let mut entry = row(("T", 1), "m", "p", at, outcome);
        entry.started_unix_ms = at;
        entry.ended_unix_ms = at + took;
        entry
    };
    let mut cached = timed(50, 1, Outcome::Completed);
    cached.cached = true;
    let entries = vec![
        timed(0, 300, Outcome::Completed),
        timed(1_000, 500, Outcome::Completed),
        // Neither a failure nor a gateway-cache answer is a timed call.
        timed(2_000, 90_000, failed(FailureClass::Timeout)),
        cached,
    ];
    assert_eq!(history(&entries)["m"].mean_latency_ms(), Some(400));
    assert_eq!(latencies(&entries).get("m"), Some(&400));
    assert_eq!(ModelHistory::default().mean_latency_ms(), None);
}

#[test]
fn latency_under_an_alias_counts_for_the_registry_model() {
    let timed = |model: &str, task: &str, took: u64| {
        let mut entry = row((task, 1), model, "deepseek", 0, Outcome::Completed);
        entry.ended_unix_ms = took;
        entry
    };
    let entries = vec![
        timed("deepseek-flash", "A", 100),
        timed("coder", "B", 300),
        timed("susi/deepseek-flash", "C", 500),
    ];
    let measured = latencies(&entries);
    assert_eq!(measured.get("deepseek-flash"), Some(&300), "{measured:?}");
    assert!(!measured.contains_key("coder"), "{measured:?}");
}

#[test]
fn a_light_workers_failure_goes_to_the_strong_fixers_first() {
    let ladder: Vec<String> = [
        "deepseek-flash",
        "gemini-flash",
        "kimi-flash",
        "deepseek-pro",
        "gemini-pro",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let strong = |model: &str| model.ends_with("-pro");
    let order = escalation_order(&ladder, &strong);
    assert_eq!(
        order,
        vec![
            "deepseek-pro",
            "gemini-pro",
            "deepseek-flash",
            "gemini-flash",
            "kimi-flash"
        ]
    );
    // A task on gemini-flash: its own light model first, then the fixers.
    let task = arrange(
        "gemini-flash",
        &order,
        &Standing {
            history: &BTreeMap::new(),
            cooling: &BTreeSet::new(),
            failed: &BTreeMap::new(),
        },
        &|_| None,
    );
    assert_eq!(&task[..3], &["gemini-flash", "deepseek-pro", "gemini-pro"]);
}

#[test]
fn a_models_last_real_call_decides_whether_it_works() {
    let entries = vec![
        // Failed, then recovered: works.
        row(
            ("T", 1),
            "healed",
            "p",
            10,
            failed(FailureClass::RateLimited),
        ),
        row(("T", 1), "healed", "p", 20, Outcome::Completed),
        // Worked, then its key was refused: inactive, whatever else is
        // true of it.
        row(("T", 1), "broken", "p", 10, Outcome::Completed),
        row(("T", 1), "broken", "p", 30, failed(FailureClass::NoCredit)),
        // Worked, then hit an overload: it failed, but only for now.
        row(("T", 1), "busy", "p", 10, Outcome::Completed),
        row(("T", 1), "busy", "p", 35, failed(FailureClass::Other)),
        // A rejected task is about the task, not the model's API.
        row(("T", 1), "judged", "p", 10, Outcome::Completed),
        row(("T", 1), "judged", "p", 40, rejected()),
    ];
    let failed = failed_models(&entries);
    assert_eq!(failed.keys().collect::<Vec<_>>(), vec!["broken", "busy"]);
    assert_eq!(failed["broken"].at_unix_ms, 30);
    assert!(failed["broken"].definitive());
    assert!(!failed["busy"].definitive());
    // A model whose key was refused is left out of a task's order, even
    // the task's own; one that failed transiently only goes to the back.
    let ladder = vec![
        "broken".to_string(),
        "busy".to_string(),
        "healed".to_string(),
    ];
    let order = arrange(
        "broken",
        &ladder,
        &Standing {
            history: &BTreeMap::new(),
            cooling: &BTreeSet::new(),
            failed: &failed,
        },
        &|_| None,
    );
    assert_eq!(order, vec!["healed".to_string(), "busy".to_string()]);
}

/// The observed failure: one model, one 429 that outlived the same-model
/// retry, and every later task of the mission failed with no call at all,
/// because the model's last row was a failure and it was dropped.
#[test]
fn a_fleet_of_one_model_still_runs_it_after_a_transient_failure() {
    for class in [
        FailureClass::RateLimited,
        FailureClass::Timeout,
        FailureClass::Other,
    ] {
        let entries = vec![
            row(("T1", 1), "only", "p", 10, Outcome::Completed),
            row(("T2", 2), "only", "p", 20, failed(class)),
        ];
        let order = arrange(
            "only",
            &["only".to_string()],
            &Standing {
                history: &history(&entries),
                cooling: &cooling_providers(&entries, 20),
                failed: &failed_models(&entries),
            },
            &|_| Some("p".to_string()),
        );
        assert_eq!(order, vec!["only".to_string()], "{class:?}");
    }
}

#[test]
fn a_transient_failure_cools_for_the_cooldown_and_a_refusal_lasts() {
    let entries = vec![
        row(
            ("T", 1),
            "busy",
            "p",
            1_000,
            failed(FailureClass::RateLimited),
        ),
        row(
            ("T", 1),
            "refused",
            "q",
            1_000,
            failed(FailureClass::NoCredit),
        ),
    ];
    let soon = in_force(failed_models(&entries), 1_000 + COOLDOWN_MS);
    assert_eq!(soon.keys().collect::<Vec<_>>(), vec!["busy", "refused"]);
    let later = in_force(failed_models(&entries), 1_001 + COOLDOWN_MS);
    assert_eq!(later.keys().collect::<Vec<_>>(), vec!["refused"]);
}

/// A gateway client may call a model by an alias or with the `susi/`
/// prefix; the ledger keeps that name, but the fleet asks by registry id.
#[test]
fn a_refused_call_through_an_alias_takes_the_registry_model_off() {
    let refused = vec![row(
        ("T", 1),
        "coder",
        "deepseek",
        10,
        failed(FailureClass::NoCredit),
    )];
    let failed = failed_models(&refused);
    assert!(failed["deepseek-flash"].definitive(), "{failed:?}");
    let order = arrange(
        "deepseek-flash",
        &["deepseek-flash".to_string(), "deepseek-pro".to_string()],
        &Standing {
            history: &BTreeMap::new(),
            cooling: &BTreeSet::new(),
            failed: &failed,
        },
        &|_| None,
    );
    assert_eq!(order, vec!["deepseek-pro".to_string()]);
    // A later good call under another name brings it back.
    let mut healed = refused.clone();
    healed.push(row(
        ("T", 2),
        "susi/deepseek-flash",
        "deepseek",
        20,
        Outcome::Completed,
    ));
    assert!(failed_models(&healed).is_empty());
}

#[test]
fn history_counts_attempts_not_calls() {
    // Attempt 1 made three calls and was rejected; attempt 2 made two
    // calls and was accepted. Two attempts, one failure.
    let entries = vec![
        row(("T", 1), "cheap", "p", 1, Outcome::Completed),
        row(("T", 1), "cheap", "p", 2, Outcome::Completed),
        row(("T", 1), "cheap", "p", 3, Outcome::Completed),
        row(("T", 1), "cheap", "p", 4, rejected()),
        row(("T", 2), "cheap", "p", 5, Outcome::Completed),
        row(("T", 2), "cheap", "p", 6, Outcome::Completed),
        row(("T", 2), "cheap", "p", 7, Outcome::Accepted),
    ];
    let record = history(&entries)["cheap"];
    assert_eq!(record.calls, 2);
    assert_eq!(record.failures, 1);
    // Every row read 1000 input with 800 from cache.
    assert_eq!(record.cache_share_permille(), Some(800));
}

#[test]
fn a_model_failing_most_attempts_is_unreliable_only_with_evidence() {
    let two_failures = vec![
        row(("A", 1), "m", "p", 1, rejected()),
        row(("B", 1), "m", "p", 2, rejected()),
    ];
    assert!(
        !history(&two_failures)["m"].unreliable(),
        "two attempts are too few"
    );
    let mut three = two_failures.clone();
    three.push(row(("C", 1), "m", "p", 3, Outcome::Accepted));
    assert!(history(&three)["m"].unreliable(), "one in three succeeded");
    three.push(row(("D", 1), "m", "p", 4, Outcome::Accepted));
    assert!(!history(&three)["m"].unreliable(), "half succeeded");
}

#[test]
fn a_measured_cache_share_lowers_the_expected_cost() {
    let mut cached = BTreeMap::new();
    cached.insert(
        "cheap".to_string(),
        ModelHistory {
            input: 1000,
            cache_hit: 900,
            ..ModelHistory::default()
        },
    );
    let plain = rank_models(
        &catalogue(),
        &["cheap".into()],
        UsageProfile::agent(),
        &BTreeMap::new(),
    )
    .unwrap();
    let warm = rank_models(
        &catalogue(),
        &["cheap".into()],
        UsageProfile::agent(),
        &cached,
    )
    .unwrap();
    assert!(warm[0].expected_cost < plain[0].expected_cost);
}

#[test]
fn providers_cool_down_at_once_on_no_credit_or_a_rate_limit_and_on_two_other_failures() {
    let now = 1_000_000_000;
    let entries = vec![
        row(
            ("T", 1),
            "a1",
            "broke",
            now - 1000,
            failed(FailureClass::NoCredit),
        ),
        row(
            ("T", 1),
            "b1",
            "busy",
            now - 3000,
            failed(FailureClass::RateLimited),
        ),
        row(
            ("T", 1),
            "b1",
            "busy",
            now - 2000,
            failed(FailureClass::RateLimited),
        ),
        // One rate limit is enough: the provider closed its window, and a
        // mission that kept retrying it lost every served turn before it.
        row(
            ("T", 1),
            "g1",
            "throttled",
            now - 500,
            failed(FailureClass::RateLimited),
        ),
        row(
            ("T", 1),
            "c1",
            "blip",
            now - 1000,
            failed(FailureClass::Other),
        ),
        row(
            ("T", 1),
            "d1",
            "healed",
            now - 3000,
            failed(FailureClass::Other),
        ),
        row(
            ("T", 1),
            "d1",
            "healed",
            now - 2000,
            failed(FailureClass::Other),
        ),
        row(("T", 1), "d1", "healed", now - 1000, Outcome::Completed),
        row(
            ("T", 1),
            "e1",
            "old",
            now - COOLDOWN_MS - 1,
            failed(FailureClass::NoCredit),
        ),
        row(("T", 1), "f1", "rejected", now - 1000, rejected()),
    ];
    let cooling = cooling_providers(&entries, now);
    assert_eq!(
        cooling.into_iter().collect::<Vec<_>>(),
        vec![
            "broke".to_string(),
            "busy".to_string(),
            "throttled".to_string()
        ]
    );
}

#[test]
fn arranging_a_ladder_again_keeps_the_attempts_it_spent() {
    let mut ladder = Ladder::new(
        vec!["claude-sonnet".to_string(), "deepseek-pro".to_string()],
        Policy::default(),
    );
    let first = ladder.next_attempt();
    assert!(matches!(&first, NextAttempt::Attempt(attempt) if attempt.model == "claude-sonnet"));
    // The ledger now says anthropic has no credit: the rebuilt order drops
    // it, and the spent attempt still counts.
    ladder.arrange(vec!["deepseek-pro".to_string()]);
    assert_eq!(ladder.used(), 1, "re-arranging does not refund an attempt");
    let next = ladder.next_attempt();
    assert!(matches!(&next, NextAttempt::Attempt(attempt) if attempt.model == "deepseek-pro"));
}

#[test]
fn arrange_keeps_the_fleet_order_and_only_demotes() {
    let ladder: Vec<String> = ["fast", "cached", "rescue", "cooling", "flaky"]
        .into_iter()
        .map(String::from)
        .collect();
    let mut records = BTreeMap::new();
    records.insert(
        "flaky".to_string(),
        ModelHistory {
            calls: 4,
            failures: 3,
            ..ModelHistory::default()
        },
    );
    records.insert(
        "rescue".to_string(),
        ModelHistory {
            calls: 4,
            failures: 3,
            ..ModelHistory::default()
        },
    );
    let cooling: BTreeSet<String> = ["down".to_string()].into_iter().collect();
    let provider_of =
        |model: &str| Some(if model == "cooling" { "down" } else { "up" }.to_string());
    let order = arrange(
        "cached",
        &ladder,
        &Standing {
            history: &records,
            cooling: &cooling,
            failed: &BTreeMap::new(),
        },
        &provider_of,
    );
    // The preferred model leads, healthy models keep the fleet's order,
    // the unreliable ones follow in the fleet's order, cooling is last.
    assert_eq!(order, vec!["cached", "fast", "rescue", "flaky", "cooling"]);
    // A fleet of one cooling model still runs it.
    let alone = arrange(
        "cooling",
        &[],
        &Standing {
            history: &records,
            cooling: &cooling,
            failed: &BTreeMap::new(),
        },
        &provider_of,
    );
    assert_eq!(alone, vec!["cooling"]);
}

#[test]
fn no_candidates_is_an_error() {
    assert!(rank_models(&catalogue(), &[], UsageProfile::agent(), &BTreeMap::new()).is_err());
}

#[test]
fn a_semantic_failure_escalates_immediately() {
    let mut ladder = Ladder::new(vec!["cheap".into(), "capable".into()], Policy::default());
    let first = ladder.next_attempt();
    assert!(matches!(
        first,
        NextAttempt::Attempt(Attempt { ref model, number: 1 }) if model == "cheap"
    ));
    ladder.record("cheap", FailureKind::Semantic, "tests failed");
    let second = ladder.next_attempt();
    assert!(matches!(
        second,
        NextAttempt::Attempt(Attempt { ref model, number: 2 }) if model == "capable"
    ));
}

#[test]
fn a_transient_failure_retries_the_same_model_once() {
    let mut ladder = Ladder::new(vec!["cheap".into(), "capable".into()], Policy::default());
    ladder.next_attempt();
    ladder.record("cheap", FailureKind::Transient, "429 rate limit");
    let second = ladder.next_attempt();
    assert!(matches!(
        second,
        NextAttempt::Attempt(Attempt { ref model, number: 2 }) if model == "cheap"
    ));
    ladder.record("cheap", FailureKind::Transient, "429 rate limit");
    let third = ladder.next_attempt();
    assert!(matches!(
        third,
        NextAttempt::Attempt(Attempt { ref model, number: 3 }) if model == "capable"
    ));
}

#[test]
fn the_same_failure_on_a_different_model_stops_the_ladder() {
    let mut ladder = Ladder::new(
        vec!["cheap".into(), "capable".into(), "third".into()],
        Policy::default(),
    );
    ladder.next_attempt();
    ladder.record("cheap", FailureKind::Semantic, "acceptance failed");
    ladder.next_attempt();
    ladder.record("capable", FailureKind::Semantic, "acceptance failed");
    assert!(matches!(
        ladder.next_attempt(),
        NextAttempt::Stop(StopReason::Stuck { .. })
    ));
    assert_eq!(ladder.used(), 2);
}

#[test]
fn the_attempt_cap_is_a_hard_stop() {
    let policy = Policy {
        max_attempts: 2,
        ..Policy::default()
    };
    let mut ladder = Ladder::new(vec!["cheap".into(), "mid".into(), "capable".into()], policy);
    assert!(matches!(ladder.next_attempt(), NextAttempt::Attempt(_)));
    assert!(matches!(ladder.next_attempt(), NextAttempt::Attempt(_)));
    ladder.record("mid", FailureKind::Semantic, "no");
    assert_eq!(
        ladder.next_attempt(),
        NextAttempt::Stop(StopReason::Exhausted)
    );
}

#[test]
fn a_timeout_repeat_across_models_stops() {
    let mut ladder = Ladder::new(vec!["cheap".into(), "capable".into()], Policy::default());
    ladder.next_attempt();
    ladder.record("cheap", FailureKind::Timeout, "agent timed out");
    ladder.next_attempt();
    ladder.record("capable", FailureKind::Timeout, "agent timed out");
    assert!(matches!(
        ladder.next_attempt(),
        NextAttempt::Stop(StopReason::Stuck { .. })
    ));
}

#[test]
fn an_executor_error_stops_immediately() {
    let mut ladder = Ladder::new(vec!["cheap".into()], Policy::default());
    ladder.next_attempt();
    ladder.record("cheap", FailureKind::Executor, "spawn failed");
    assert!(matches!(
        ladder.next_attempt(),
        NextAttempt::Stop(StopReason::Stuck { .. })
    ));
}

#[test]
fn failure_messages_are_classified() {
    assert_eq!(
        FailureKind::classify("429 rate limit exceeded"),
        FailureKind::Transient
    );
    assert_eq!(
        FailureKind::classify("provider overloaded"),
        FailureKind::Transient
    );
    assert_eq!(
        FailureKind::classify("request timed out"),
        FailureKind::Timeout
    );
    assert_eq!(
        FailureKind::classify("acceptance failed: 2 tests red"),
        FailureKind::Semantic
    );
}

#[test]
fn signatures_are_stable() {
    assert_eq!(
        signature(FailureKind::Semantic, "Tests failed: 2 red!"),
        signature(FailureKind::Semantic, "tests failed 2 red")
    );
    assert_eq!(signature(FailureKind::Timeout, "a"), "timeout");
}
