//! The candidate set keeps itself current (VC-202-003, T-DEEPSEEK-128).
//! Every scout run snapshots the catalogue — which providers exist and the
//! price the ranker would charge them — and the diff against the previous
//! run is drift: a model that appeared is reported (it was already probed
//! by the same sweep), a retired model is reported and marked `Gone` in the
//! brain so a re-appearance starts from evidence rather than innocence —
//! history is never deleted — and a re-priced model is reported because a
//! ranking input moved.

use crate::scout_probe::{ProbeResult, ScoutReport};
use crate::scout_schedule::{drift_between, finish_run, CatalogueEntry, ScoutDrift, ScoutRun};
use std::collections::BTreeMap;

fn cat(provider: &str, input: f64, output: f64) -> (String, CatalogueEntry) {
    (
        provider.to_string(),
        CatalogueEntry {
            input_usd_per_1m: Some(input),
            output_usd_per_1m: Some(output),
            cache_hit_usd_per_1m: None,
        },
    )
}

fn run_with_catalogue(unix: u64, catalogue: Vec<(String, CatalogueEntry)>) -> ScoutRun {
    ScoutRun {
        unix,
        leaders: BTreeMap::new(),
        providers: Vec::new(),
        catalogue: catalogue.into_iter().collect(),
    }
}

#[test]
fn model_catalogue_drift_appeared_retired_repriced() {
    let prev = run_with_catalogue(
        100,
        vec![cat("acme-stay", 1.0, 2.0), cat("acme-old", 3.0, 4.0)],
    );
    let cur = run_with_catalogue(
        200,
        vec![cat("acme-stay", 1.5, 2.0), cat("acme-new", 5.0, 6.0)],
    );
    let drift = drift_between(&prev, &cur);
    assert!(
        drift.contains(&ScoutDrift::ModelAppeared {
            provider: "acme-new".to_string()
        }),
        "a model that appeared since the last scout is drift: {drift:?}"
    );
    assert!(
        drift.contains(&ScoutDrift::ModelRetired {
            provider: "acme-old".to_string()
        }),
        "a model that disappeared since the last scout is drift: {drift:?}"
    );
    assert!(drift.iter().any(
        |d| matches!(d, ScoutDrift::ModelPriceChanged { provider, .. } if provider == "acme-stay")
    ), "a price change is drift: {drift:?}");
    assert_eq!(drift.len(), 3, "exactly the three catalogue events");
}

#[test]
fn model_catalogue_drift_identical_catalogue_reports_nothing() {
    let prev = run_with_catalogue(100, vec![cat("acme-a", 1.0, 2.0)]);
    let cur = run_with_catalogue(200, vec![cat("acme-a", 1.0, 2.0)]);
    assert!(drift_between(&prev, &cur).is_empty());
}

fn empty_report(provider: &str) -> ScoutReport {
    ScoutReport {
        provider: provider.to_string(),
        probes: 0,
        verified: 0,
        transport_failures: 0,
        results: Vec::<ProbeResult>::new(),
    }
}

#[test]
fn model_catalogue_drift_retired_model_is_marked_gone_not_deleted() {
    let _env = crate::engines::env_test_lock();
    let evidence =
        std::env::temp_dir().join(format!("susi-cat-drift-brain-{}-{}", std::process::id(), 1));
    let _ = std::fs::remove_file(&evidence);
    // SAFETY: serialized by env_test_lock; restored before drop.
    unsafe {
        std::env::set_var("SUSI_BRAIN_EVIDENCE_FILE", &evidence);
    }
    let _ = crate::engines::brain::reset();

    let dir = std::env::temp_dir().join(format!("susi-cat-drift-{}", std::process::id()));
    let journal = dir.join("runs.jsonl");
    let _ = std::fs::remove_dir_all(&dir);

    // Run 1: acme-gone is part of the catalogue.
    let names1 = vec!["acme-gone".to_string()];
    finish_run(100, &journal, &names1, &[empty_report("acme-gone")]);
    // Run 2: acme-gone is no longer registered — retired.
    let names2: Vec<String> = Vec::new();
    let (_, drift) = finish_run(200, &journal, &names2, &[]);
    assert!(
        drift.contains(&ScoutDrift::ModelRetired {
            provider: "acme-gone".to_string()
        }),
        "retirement is reported drift: {drift:?}"
    );
    // …and the brain marks it Gone — history kept, fitness marked — so a
    // re-appearance under the same name is not trusted on sight.
    let store = crate::engines::brain::load();
    let v = serde_json::to_value(&store).expect("store serializes");
    assert_eq!(
        v["health"]["acme-gone"]["kind"], "gone",
        "retired provider marked Gone in persisted brain health: {v}"
    );

    unsafe {
        std::env::remove_var("SUSI_BRAIN_EVIDENCE_FILE");
    }
    let _ = crate::engines::brain::reset();
}

#[test]
fn model_catalogue_drift_finish_run_snapshots_membership() {
    let _env = crate::engines::env_test_lock();
    let dir = std::env::temp_dir().join(format!("susi-cat-snap-{}", std::process::id()));
    let journal = dir.join("runs.jsonl");
    let _ = std::fs::remove_dir_all(&dir);
    let names = vec!["acme-a".to_string(), "acme-b".to_string()];
    let (run, drift) = finish_run(
        100,
        &journal,
        &names,
        &[empty_report("acme-a"), empty_report("acme-b")],
    );
    assert!(drift.is_empty(), "first run has no previous snapshot");
    assert_eq!(
        run.catalogue.len(),
        2,
        "every registered provider is in the snapshot"
    );
    assert!(
        run.catalogue.values().all(|e| e.input_usd_per_1m.is_none()),
        "unpriced providers still appear — membership is the catalogue axis"
    );
}
