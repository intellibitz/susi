use super::*;

fn rec(provider: &str, class: &str, p: u64, c: u64, ok: bool) -> UsageOutcome {
    UsageOutcome {
        provider: provider.to_string(),
        task_class: class.to_string(),
        usage: TokenUsage {
            prompt_tokens: p,
            completion_tokens: c,
        },
        success: ok,
        latency_ms: 100,
        unix_ms: 0,
        agent: String::new(),
        mission: String::new(),
        usd: 0.0,
    }
}

#[test]
fn usage_accounting_aggregates_per_provider_class() {
    let mut l = UsageLedger::new();
    l.record(rec("openai", "code", 100, 50, true));
    l.record(rec("openai", "code", 200, 50, true));
    l.record(rec("openai", "chat", 900, 100, true));
    l.record(rec("local", "code", 10, 5, true));
    let s = l.summary("openai", "code", None);
    assert_eq!(s.calls, 2);
    assert_eq!(s.prompt_tokens, 300);
    assert_eq!(s.tokens_per_success, 200.0);
    assert!(s.usd_per_success.is_none());
}

#[test]
fn usage_accounting_measured_cost_per_success() {
    let mut l = UsageLedger::new();
    // openai: 2 successes costing (1000+1000)in + (500+500)out
    l.record(rec("openai", "", 1000, 500, true));
    l.record(rec("openai", "", 1000, 500, true));
    l.record(rec("local", "", 5000, 5000, true));
    let mut prices = BTreeMap::new();
    prices.insert(
        "openai".to_string(),
        TokenPrice {
            input_per_million: 2.0,
            output_per_million: 10.0,
        },
    );
    prices.insert(
        "local".to_string(),
        TokenPrice {
            input_per_million: 0.0,
            output_per_million: 0.0,
        },
    );
    let ranked = l.measured_cost_per_success(&prices);
    // openai per success: (1000*2 + 500*10)/1e6 = 0.007
    assert_eq!(ranked[0].0, "local");
    assert!((ranked[1].1 - 0.007).abs() < 1e-9);
}

#[test]
fn usage_accounting_failures_count_calls_not_success() {
    let mut l = UsageLedger::new();
    l.record(rec("p", "", 100, 50, false));
    l.record(rec("p", "", 100, 50, true));
    let s = l.summary(
        "p",
        "",
        Some(TokenPrice {
            input_per_million: 1.0,
            output_per_million: 1.0,
        }),
    );
    assert_eq!(s.calls, 2);
    assert_eq!(s.successes, 1);
    // cost of only the successful call / 1 success
    assert!((s.usd_per_success.unwrap_or(0.0) - 0.00015).abs() < 1e-9);
}

#[test]
fn usage_accounting_persists_roundtrip() {
    let dir = std::env::temp_dir().join(format!("usage-ledger-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("u.json");
    let mut l = UsageLedger::new();
    l.record(rec("p", "c", 1, 2, true));
    l.save(&path).unwrap();
    let loaded = UsageLedger::load(&path).unwrap();
    assert_eq!(loaded.len(), 1);
    // absent file -> empty ledger
    assert!(UsageLedger::load(&dir.join("nope.json"))
        .unwrap()
        .is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn usage_accounting_empty_summary_is_zeroed() {
    let l = UsageLedger::new();
    let s = l.summary("nobody", "code", None);
    assert_eq!(s.calls, 0);
    assert_eq!(s.tokens_per_success, 0.0);
}
