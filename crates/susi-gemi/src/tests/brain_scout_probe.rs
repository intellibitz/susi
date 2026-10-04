//! A deterministic micro-benchmark the scout can run daily (VC-202-003,
//! T-DEEPSEEK-99): fixed prompts with offline-checkable answers, so the
//! probe measures the model — never the network, never a judge model — and
//! results stay comparable across providers and across days. Verified probe
//! outcomes land in the same evidence the live cascade records, so a probed
//! provider's cost per verified outcome reflects the measurement.

use crate::engines::brain::rank;
use crate::scout_probe::{probe_with, record_outcomes, ScoutReport, SCOUT_PROBES};

#[test]
fn brain_scout_probe_set_is_fixed_tiny_and_offline_checkable() {
    // Bounded: safe to run daily at negligible cost.
    assert!(SCOUT_PROBES.len() <= 8, "{} probes", SCOUT_PROBES.len());
    for p in SCOUT_PROBES {
        assert!(
            p.prompt.len() <= 120,
            "a scout prompt stays tiny: {}",
            p.prompt
        );
        // Every probe checks the answer itself — offline, deterministic.
        assert!(
            (p.check)(p.expected) || p.expected.contains('|'),
            "{}",
            p.prompt
        );
    }
    // Coverage spans more than one task class so per-class evidence differs.
    let classes: std::collections::BTreeSet<_> =
        SCOUT_PROBES.iter().map(|p| p.class.label()).collect();
    assert!(classes.len() >= 3, "probes span task classes: {classes:?}");
}

#[test]
fn brain_scout_probe_correct_answers_pass_wrong_answers_fail() {
    // A transport that answers every probe with its documented expectation
    // verifies fully; one that babbles verifies nothing.
    let good = probe_with("scout-good", &|_p| {
        // Deliberately not wired to expectations — answer with the probe's
        // own expected token proves checks honor the predicate, not length.
        Ok("the answer".to_string())
    });
    assert_eq!(good.verified, 0, "a wrong answer never verifies");
    let perfect = probe_with("scout-perfect", &|p| {
        let probe = SCOUT_PROBES
            .iter()
            .find(|x| x.prompt == p)
            .expect("known prompt");
        Ok(probe.expected.to_string())
    });
    assert_eq!(perfect.verified, SCOUT_PROBES.len());
    assert_eq!(perfect.verified_fraction(), Some(1.0));
}

#[test]
fn brain_scout_probe_results_are_deterministic_across_runs() {
    let complete = |p: &str| -> Result<String, String> {
        let probe = SCOUT_PROBES
            .iter()
            .find(|x| x.prompt == p)
            .expect("known prompt");
        Ok(format!("{}.", probe.expected))
    };
    let a = probe_with("scout-det", &complete);
    let b = probe_with("scout-det", &complete);
    assert_eq!(a.verified, b.verified);
    assert_eq!(
        a.results.iter().map(|r| r.correct).collect::<Vec<_>>(),
        b.results.iter().map(|r| r.correct).collect::<Vec<_>>(),
        "same provider + same probes = same verdicts"
    );
}

#[test]
fn brain_scout_probe_transport_failures_are_not_wrong_answers() {
    let report = probe_with("scout-dead", &|_p| Err("connection refused".to_string()));
    assert_eq!(report.transport_failures, SCOUT_PROBES.len());
    assert_eq!(report.verified, 0);
    assert_eq!(report.verified_fraction(), None);
    for r in &report.results {
        assert!(!r.correct && r.error.is_some() && r.answer_preview.is_none());
    }
    // Flattened error text counts as a transport failure too — adapters
    // that fold failures into Ok(text) cannot fake a verified probe.
    let flat = probe_with("scout-flat", &|_p| {
        Ok("[INFERENCE_FAILED] upstream 500".to_string())
    });
    assert_eq!(flat.transport_failures, SCOUT_PROBES.len());
}

#[test]
fn brain_scout_probe_outcomes_feed_the_evidence_ranking() {
    let _env = crate::engines::env_test_lock();
    // SAFETY: point the global evidence store at this test's own file so
    // record_outcome persists somewhere hermetic, then reset the cache.
    let dir = std::env::temp_dir().join(format!("susi-scout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    unsafe {
        std::env::set_var(
            "SUSI_BRAIN_EVIDENCE_FILE",
            dir.join("scout_probe_evidence.json"),
        );
    }
    let _ = crate::engines::brain::reset();

    let good: ScoutReport = probe_with("scout-measured-good", &|p| {
        let probe = SCOUT_PROBES
            .iter()
            .find(|x| x.prompt == p)
            .expect("known prompt");
        Ok(probe.expected.to_string())
    });
    let bad: ScoutReport = probe_with("scout-measured-bad", &|_p| Ok("banana".to_string()));
    assert_eq!(record_outcomes(&good), SCOUT_PROBES.len());
    assert_eq!(record_outcomes(&bad), SCOUT_PROBES.len());

    let mut failures = Vec::new();
    for probe in SCOUT_PROBES {
        let names = vec![
            "scout-measured-bad".to_string(),
            "scout-measured-good".to_string(),
        ];
        let ranked = rank(&names, probe.class);
        if ranked[0].provider != "scout-measured-good" {
            failures.push(probe.class.label());
        }
    }
    // Clean the shared state before asserting so a failure never leaks this
    // test's evidence file or cache into a parallel test.
    unsafe {
        std::env::remove_var("SUSI_BRAIN_EVIDENCE_FILE");
    }
    let _ = crate::engines::brain::reset();
    assert!(
        failures.is_empty(),
        "verified probe outcomes must lead: {failures:?}"
    );
}

#[test]
fn brain_scout_probe_production_wiring() {
    // The probe must be reachable outside tests — a benchmark nobody can
    // run is scaffolding.
    let cli_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/cli/brain_cli.rs"
    ))
    .expect("brain_cli.rs readable");
    assert!(
        cli_src.contains("probe_registered"),
        "`susi brain probe` must drive the real registry path"
    );
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/scout_probe.rs"))
        .expect("scout_probe.rs readable");
    assert!(
        src.contains("try_acquire"),
        "the production probe must admit through the same key arbiter as dispatch"
    );
    assert!(
        src.contains("register_configured_cloud_endpoints"),
        "the production probe must resolve configured providers from the real registry"
    );
}
