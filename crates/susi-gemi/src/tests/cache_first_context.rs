//! Cache-first context assembly, measured (VC-202-006, T-DEEPSEEK-103).
//!
//! Provider prompt caches hit on byte-identical shared prefixes. Assembly
//! puts the stable content first — instruction turns hoisted, history
//! verbatim — and measures each call's cacheable fraction and priced saving
//! into `context_cache.jsonl`, so a cache-defeating shape is a journaled
//! defect rather than an accident.

use crate::context_assembly::{assemble, cache_report_at, record_assembly_at, CacheDefect};

fn turns(spec: &[(&str, &str)]) -> Vec<(String, String)> {
    spec.iter()
        .map(|(r, c)| (r.to_string(), c.to_string()))
        .collect()
}

fn big(n: usize) -> String {
    "context ".repeat(n)
}

#[test]
fn cache_first_context_late_instruction_is_hoisted_with_defect() {
    let ctx = assemble(&turns(&[
        ("user", "first"),
        ("assistant", "reply"),
        ("system", "Be brief"),
        ("user", "second"),
    ]));
    assert!(
        ctx.prompt.starts_with("system: Be brief"),
        "an instruction arriving mid-conversation must still lead the prompt: {}",
        ctx.prompt
    );
    assert!(
        ctx.defects.contains(&CacheDefect::LateInstructionReordered),
        "the input order was the defect — it is recorded, not hidden: {:?}",
        ctx.defects
    );
    // The reorder keeps every turn — nothing is dropped to save the prefix.
    for needle in ["user: first", "assistant: reply", "user: second"] {
        assert!(ctx.prompt.contains(needle), "missing {needle}");
    }
}

#[test]
fn cache_first_context_repeated_turns_extend_one_prefix() {
    // The property a provider cache actually pays on: turn N's prompt is a
    // byte-identical prefix of turn N+1's.
    let conversation = turns(&[
        ("system", &big(300)),
        ("user", "q1"),
        ("assistant", "a1"),
        ("user", "q2"),
    ]);
    let prior = assemble(&conversation[..3]);
    let next = assemble(&conversation);
    assert!(
        next.prompt.starts_with(&prior.prompt),
        "the next call must share the previous call's whole prompt as a prefix"
    );
    assert!(
        next.cacheable_fraction > 0.9,
        "history plus instruction is nearly all cacheable: {}",
        next.cacheable_fraction
    );
    assert!(
        !next.defects.contains(&CacheDefect::NoInstructionPrefix),
        "an instructed conversation records no missing-prefix defect"
    );
}

#[test]
fn cache_first_context_defeating_shape_is_flagged() {
    // No instruction turn and a trivial prefix: even perfect ordering
    // buys nothing under the provider cache threshold.
    let ctx = assemble(&turns(&[
        ("user", "hi"),
        ("assistant", "yo"),
        ("user", "again"),
    ]));
    assert!(
        ctx.defects.contains(&CacheDefect::NoInstructionPrefix),
        "no instruction block is a defect with evidence: {:?}",
        ctx.defects
    );
    assert!(
        ctx.defects.contains(&CacheDefect::PrefixUnderThreshold),
        "a sub-threshold prefix is a defect with evidence: {:?}",
        ctx.defects
    );
    assert!(
        ctx.stable_prefix_tokens < 1024,
        "the measured prefix is honestly below the cache threshold: {}",
        ctx.stable_prefix_tokens
    );
}

#[test]
fn cache_first_context_journal_measures_hit_rate_and_reports() {
    let dir = std::env::temp_dir().join(format!("susi-ctx-cache-{}", std::process::id()));
    let journal = dir.join("context_cache.jsonl");
    let _ = std::fs::remove_file(&journal);

    let ctx = assemble(&turns(&[
        ("system", &big(300)),
        ("user", "q1"),
        ("assistant", "a1"),
        ("user", "q2"),
    ]));
    record_assembly_at(&journal, Some("acme-cheapmodel"), &ctx);
    record_assembly_at(&journal, Some("acme-cheapmodel"), &ctx);

    let report = cache_report_at(&journal);
    assert_eq!(report.calls, 2, "each assembled context is measured once");
    assert!(
        (report.mean_cacheable_fraction - ctx.cacheable_fraction).abs() < 1e-9,
        "the report's hit rate is the mean of the measured calls: {}",
        report.mean_cacheable_fraction
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cache_first_context_saving_priced_against_catalog() {
    // Same shared-catalog convention the cost tests use — identical path
    // and bytes make parallel writers consistent.
    let dir = std::env::temp_dir().join("susi-prices-shared");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("model_prices.json");
    let mut cat = crate::models::price_catalog::PriceCatalog::empty(1);
    cat.insert(crate::models::price_catalog::PriceEntry {
        model_id: "cheapmodel".to_string(),
        input_usd_per_1m: 1.0,
        output_usd_per_1m: 4.0,
        cache_hit_usd_per_1m: Some(0.1),
        cache_miss_usd_per_1m: Some(1.0),
    });
    std::fs::write(&path, cat.to_json().expect("catalog json")).expect("catalog file");
    // SAFETY: test-only env mutation; identical path and content as the
    // sibling cost tests, so a torn write still reads the same catalog.
    unsafe {
        std::env::set_var("SUSI_PRICE_CATALOG_FILE", &path);
    }

    // 10K shared-prefix tokens at $1/1M miss vs $0.10/1M hit saves $0.009.
    let saving = crate::context_assembly::estimated_saving_usd("acme-cheapmodel", 10_000)
        .expect("a priced provider yields a priced saving");
    assert!(
        (saving - 0.009).abs() < 1e-9,
        "hit-vs-miss delta on the shared prefix is the saving: {saving}"
    );
    assert_eq!(
        crate::context_assembly::estimated_saving_usd("acme-unpriced", 10_000),
        None,
        "an unpriced provider yields no invented saving"
    );
    // SAFETY: restore — see above.
    unsafe {
        std::env::remove_var("SUSI_PRICE_CATALOG_FILE");
    }
}

#[test]
fn cache_first_context_production_wiring() {
    // The assembly must run on the production chat path — a module nobody
    // calls is scaffold, not delivery.
    let server_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../susi-server/src/lib.rs"
    ))
    .expect("susi-server lib.rs readable");
    assert!(
        server_src.contains("gemi.context.assemble"),
        "the chat-completions path must assemble context through the GEMI plane"
    );
    let handler_src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/plane_handler.rs"))
            .expect("plane_handler.rs readable");
    assert!(
        handler_src.contains("record_assembly"),
        "the assemble arm must journal the measured cache profile"
    );
    let cli_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/cli/brain_cli.rs"
    ))
    .expect("brain_cli.rs readable");
    assert!(
        cli_src.contains("cache_report"),
        "the measured hit rate and saving must be reported on a surface"
    );
}
