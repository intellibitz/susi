//! Spend ceilings that refuse before the call is made (VC-202-005,
//! T-DEEPSEEK-102).
//!
//! The deliverable is pre-dispatch enforcement, not after-the-fact
//! reporting: `spend_tracker::check` runs inside `try_providers` before
//! `provider.generate`, a would-be over-budget call is refused with a
//! typed `BudgetRefusal`, the ladder steps down to a cheaper rung (or out
//! of cloud entirely), and every attempted call lands in the persisted
//! usage ledger attributed to agent, mission and task class.

use crate::engines::runtime::GemiEngine;
use crate::spend_tracker::{self, BudgetCeilings, BudgetRefusal, BudgetWindow};
use crate::susi_core::provider::{BoxFuture, Provider};
use crate::susi_core::registry::CapabilityRegistry;
use crate::susi_core::susi_error::EaiResult;
use crate::usage_accounting::{UsageLedger, UsageOutcome};
use std::collections::BTreeMap;

fn outcome(provider: &str, mission: &str, usd: f64, unix_ms: u64) -> UsageOutcome {
    UsageOutcome {
        provider: provider.to_string(),
        task_class: "chat".to_string(),
        usage: crate::usage_accounting::TokenUsage::default(),
        success: true,
        latency_ms: 0,
        unix_ms,
        agent: "TESTAGENT".to_string(),
        mission: mission.to_string(),
        usd,
    }
}

fn ceilings() -> BudgetCeilings {
    BudgetCeilings {
        hourly_usd: None,
        daily_usd: None,
        mission_usd: None,
        task_usd: None,
        vendor_daily_usd: BTreeMap::new(),
    }
}

const NOW: u64 = 1_800_000_000_000;

#[test]
fn budget_ceiling_task_cap_refuses_the_call_itself() {
    let c = BudgetCeilings {
        task_usd: Some(0.01),
        ..ceilings()
    };
    let err = spend_tracker::check_in(&UsageLedger::new(), &c, "openai-x", 0.02, "", NOW)
        .expect_err("a $0.02 call breaks a $0.01 per-task cap");
    assert_eq!(err.window, BudgetWindow::Task);
    assert_eq!(err.spent_usd, 0.0, "task cap refuses before any spend");
    assert!((err.projected_usd - 0.02).abs() < 1e-9);
    spend_tracker::check_in(&UsageLedger::new(), &c, "openai-x", 0.005, "", NOW)
        .expect("under-cap call passes");
}

#[test]
fn budget_ceiling_hourly_window_sums_only_in_window_spend() {
    let c = BudgetCeilings {
        hourly_usd: Some(1.0),
        ..ceilings()
    };
    let mut ledger = UsageLedger::new();
    // $0.90 spent inside the hour; $50 spent a week ago — only the hour counts.
    ledger.record(outcome("p", "m", 0.9, NOW - 1_000));
    ledger.record(outcome("p", "m", 50.0, NOW - 604_800_000));
    let err = spend_tracker::check_in(&ledger, &c, "p", 0.2, "", NOW)
        .expect_err("0.9 spent + 0.2 expected crosses the $1 hourly cap");
    assert_eq!(err.window, BudgetWindow::Hourly);
    assert!((err.spent_usd - 0.9).abs() < 1e-9, "stale records excluded");
    spend_tracker::check_in(&ledger, &c, "p", 0.05, "", NOW)
        .expect("0.9 + 0.05 stays under the cap");
}

#[test]
fn budget_ceiling_daily_window_rolls_over() {
    let c = BudgetCeilings {
        daily_usd: Some(2.0),
        ..ceilings()
    };
    let mut ledger = UsageLedger::new();
    ledger.record(outcome("p", "m", 1.9, NOW - 1_000));
    ledger.record(outcome("p", "m", 99.0, NOW - 86_400_001));
    assert!(
        spend_tracker::check_in(&ledger, &c, "p", 0.2, "", NOW).is_err(),
        "1.9 + 0.2 > $2 daily"
    );
    assert!(
        spend_tracker::check_in(&ledger, &c, "p", 0.2, "", NOW + 86_400_000).is_ok(),
        "a day later the window resets"
    );
}

#[test]
fn budget_ceiling_mission_spend_is_scoped_to_that_mission() {
    let c = BudgetCeilings {
        mission_usd: Some(1.0),
        ..ceilings()
    };
    let mut ledger = UsageLedger::new();
    ledger.record(outcome("p", "mission-A", 0.99, NOW - 1_000));
    assert!(
        spend_tracker::check_in(&ledger, &c, "p", 0.05, "mission-A", NOW).is_err(),
        "mission A is at its cap"
    );
    spend_tracker::check_in(&ledger, &c, "p", 0.05, "mission-B", NOW)
        .expect("mission B starts with a fresh budget");
    // A call outside any mission is not mission-capped.
    spend_tracker::check_in(&ledger, &c, "p", 5.0, "", NOW)
        .expect("unattributed calls skip the mission axis");
}

#[test]
fn budget_ceiling_vendor_daily_is_keyed_per_vendor() {
    let mut vendor_daily = BTreeMap::new();
    vendor_daily.insert("openai".to_string(), 1.0);
    let c = BudgetCeilings {
        vendor_daily_usd: vendor_daily,
        ..ceilings()
    };
    let mut ledger = UsageLedger::new();
    ledger.record(outcome("openai-gpt", "m", 0.95, NOW - 1_000));
    assert!(
        spend_tracker::check_in(&ledger, &c, "openai-gpt", 0.1, "", NOW)
            .expect_err("openai over its vendor cap")
            .window
            == BudgetWindow::VendorDaily
    );
    spend_tracker::check_in(&ledger, &c, "anthropic-claude", 5.0, "", NOW)
        .expect("a different vendor has no cap");
}

#[test]
fn budget_ceiling_refusal_is_typed_and_readable() {
    let c = BudgetCeilings {
        daily_usd: Some(1.0),
        ..ceilings()
    };
    let mut ledger = UsageLedger::new();
    ledger.record(outcome("p", "m", 0.997, NOW - 1_000));
    let refusal: BudgetRefusal =
        spend_tracker::check_in(&ledger, &c, "p", 0.01, "", NOW).expect_err("refused");
    assert_eq!(refusal.window, BudgetWindow::Daily);
    assert_eq!(refusal.cap_usd, 1.0);
    let text = refusal.describe();
    assert!(text.contains("daily ceiling"), "{text}");
    assert!(text.contains("$1.00"), "{text}");
    // Typed means serializable — the ladder can persist it, not just print it.
    let json = serde_json::to_string(&refusal).expect("serializes");
    assert!(json.contains("\"daily\""), "{json}");
}

#[test]
fn budget_ceiling_uncapped_governor_never_refuses() {
    let mut ledger = UsageLedger::new();
    ledger.record(outcome("p", "m", 9_999.0, NOW - 1_000));
    spend_tracker::check_in(&ledger, &ceilings(), "p", 1e6, "any", NOW)
        .expect("no configured ceiling means no refusal");
    assert!(ceilings().is_uncapped());
}

struct AnsweringProvider {
    name: &'static str,
    reply: &'static str,
}

impl Provider for AnsweringProvider {
    fn name(&self) -> &str {
        self.name
    }
    fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
        Box::pin(async { Ok(true) })
    }
    fn generate(&self, _prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
        let reply = self.reply.to_string();
        Box::pin(async move { Ok(reply) })
    }
    fn embed(&self, _text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Install the shared test fixtures. One file per env var, identical content
/// no matter which test writes it, so a parallel reader can never observe
/// torn state: the catalog includes every model id the suite prices, the
/// ceilings file caps the daily window at $0.02, and the usage file starts
/// empty (foreign `try_providers` callers may append records; only records
/// for THIS test's providers are asserted on).
fn install_fixtures() {
    let dir = std::env::temp_dir().join("susi-budget-shared");
    std::fs::create_dir_all(&dir).expect("temp dir");

    // Identical content to expected_cost_with_cache::install_catalog's file —
    // whichever path `SUSI_PRICE_CATALOG_FILE` resolves to mid-race, every
    // model id in the suite prices the same.
    let mut entries = BTreeMap::new();
    for (id, i, o, hit, miss) in [
        ("cheapmodel", 1.0, 4.0, Some(0.1), Some(1.0)),
        ("premiummodel", 1.0, 10.0, Some(0.1), Some(1.0)),
        ("budgetmodel", 0.01, 0.01, None, None),
        ("pricymodel", 5.0, 50.0, None, None),
        ("bargainmodel", 0.01, 0.01, None, None),
    ] {
        entries.insert(
            id.to_string(),
            crate::models::price_catalog::PriceEntry {
                model_id: id.to_string(),
                input_usd_per_1m: i,
                output_usd_per_1m: o,
                cache_hit_usd_per_1m: hit,
                cache_miss_usd_per_1m: miss,
            },
        );
    }
    let catalog = crate::models::price_catalog::PriceCatalog {
        version: 1,
        entries,
    };
    std::fs::write(
        dir.join("model_prices.json"),
        catalog.to_json().expect("catalog json"),
    )
    .expect("catalog file");

    std::fs::write(dir.join("budget_ceilings.json"), r#"{"daily_usd": 0.005}"#)
        .expect("ceilings file");

    // The usage ledger is test-scoped, not shared — each cascade test seeds
    // its own so a foreign record from another test cannot flip a cap.
    // SAFETY: env mutations are serialized by env_test_lock at each call
    // site; the catalog/ceilings content is identical across writers.
    unsafe {
        std::env::set_var("SUSI_PRICE_CATALOG_FILE", dir.join("model_prices.json"));
        std::env::set_var(
            "SUSI_BUDGET_CEILINGS_FILE",
            dir.join("budget_ceilings.json"),
        );
    }
}

#[test]
fn budget_ceiling_cascade_steps_down_to_the_affordable_rung() {
    let _env = crate::engines::env_test_lock();
    install_fixtures();
    let usage = std::env::temp_dir().join("susi-budget-cascade-usage.json");
    std::fs::remove_file(&usage).ok();
    // SAFETY: serialized by env_test_lock.
    unsafe {
        std::env::set_var("SUSI_USAGE_FILE", &usage);
    }

    let registry = CapabilityRegistry::new();
    // "hello there" classifies Reflex (512 in + 128 out): pricymodel at
    // $5/$50 ≈ $0.009 — over the $0.005 daily cap; bargainmodel at
    // $0.01/$0.01 ≈ $0.000006 — under. The pricey name sorts first
    // alphabetically so the cascade evaluates its refusal before the
    // affordable rung gets to serve (the cascade exits on first success).
    registry.register_provider(AnsweringProvider {
        name: "acme-apricymodel",
        reply: "premium answer",
    });
    registry.register_provider(AnsweringProvider {
        name: "acme-bargainmodel",
        reply: "budget answer",
    });

    let (out, ladder) =
        GemiEngine::try_providers(&registry, "hello there", None, None, &|_| {}, &|_| {});
    assert_eq!(
        out.as_deref(),
        Some("budget answer"),
        "the affordable rung serves when the pricey one is over budget"
    );
    let pricey_step = ladder
        .steps
        .iter()
        .find(|s| s.candidate.contains("pricymodel"))
        .expect("pricy rung recorded");
    assert!(
        pricey_step.reason.contains("daily ceiling"),
        "step-down names the refused ceiling: {}",
        pricey_step.reason
    );
}

#[test]
fn budget_ceiling_cascade_records_spend_with_attribution() {
    let _env = crate::engines::env_test_lock();
    install_fixtures();
    let usage = std::env::temp_dir().join("susi-budget-attr-usage.json");
    std::fs::remove_file(&usage).ok();
    // SAFETY: serialized by env_test_lock.
    unsafe {
        std::env::set_var("SUSI_USAGE_FILE", &usage);
        std::env::set_var("SUSI_AGENT", "TESTAGENT");
    }

    let registry = CapabilityRegistry::new();
    registry.register_provider(AnsweringProvider {
        name: "acme-bargainmodel",
        reply: "cheap answer",
    });
    let (out, _ladder) =
        GemiEngine::try_providers(&registry, "hello there", None, None, &|_| {}, &|_| {});
    assert_eq!(out.as_deref(), Some("cheap answer"));

    let ledger = UsageLedger::load(&usage).expect("ledger written");
    let rec = ledger
        .records()
        .iter()
        .find(|r| r.provider == "acme-bargainmodel")
        .expect("the served call recorded its spend");
    assert_eq!(rec.agent, "TESTAGENT", "spend attributed to the agent");
    assert_eq!(
        rec.task_class, "reflex",
        "spend attributed to the task class"
    );
    assert!(rec.success, "successful call recorded as such");
    assert!(
        rec.usd > 0.0 && rec.usd < 0.001,
        "est. spend recorded: {}",
        rec.usd
    );
    assert!(rec.unix_ms > 0);
}

#[test]
fn budget_ceiling_all_over_cap_leaves_nothing_priced() {
    let _env = crate::engines::env_test_lock();
    install_fixtures();
    let usage = std::env::temp_dir().join("susi-budget-empty-usage.json");
    std::fs::remove_file(&usage).ok();
    // SAFETY: serialized by env_test_lock.
    unsafe {
        std::env::set_var("SUSI_USAGE_FILE", &usage);
    }

    let registry = CapabilityRegistry::new();
    registry.register_provider(AnsweringProvider {
        name: "acme-apricymodel",
        reply: "never",
    });
    let (out, ladder) =
        GemiEngine::try_providers(&registry, "hello there", None, None, &|_| {}, &|_| {});
    assert!(
        out.is_none(),
        "every priced rung over the cap leaves the cascade unanswered — the local rung outside it stays free"
    );
    assert!(
        ladder
            .steps
            .iter()
            .any(|s| s.reason.contains("daily ceiling")),
        "the refusal is on the record, not silent"
    );
    // No provider.generate ran, so no spend record exists for it.
    let ledger = UsageLedger::load(&usage).expect("ledger readable");
    assert!(
        ledger
            .records()
            .iter()
            .all(|r| r.provider != "acme-apricymodel"),
        "a refused call records no spend"
    );
}

#[test]
fn budget_ceiling_production_wiring() {
    let runtime_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/engines/runtime.rs"
    ))
    .expect("runtime.rs readable");
    assert!(
        runtime_src.contains("spend_tracker::check"),
        "the ceiling check must run inside the production cascade before generate"
    );
    assert!(
        runtime_src.contains("spend_tracker::record"),
        "every dispatched call must record attributed spend"
    );
    assert!(
        runtime_src.contains("EvidenceSession"),
        "mission attribution must come from the live evidence session"
    );
    let spend_src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/spend_tracker.rs"))
            .expect("spend_tracker.rs readable");
    for needle in [
        "hourly_usd",
        "daily_usd",
        "mission_usd",
        "task_usd",
        "BudgetRefusal",
    ] {
        assert!(spend_src.contains(needle), "missing {needle}");
    }
}
