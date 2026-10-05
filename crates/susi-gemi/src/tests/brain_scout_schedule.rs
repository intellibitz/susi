//! The scout runs on a schedule and reports drift (VC-202-003,
//! T-DEEPSEEK-107). Scouting once at start-up is not scouting: the run is
//! cadence-gated by the daemon's shared probe scheduler, each run snapshots
//! the leader per task class into an append-only journal — the previous
//! ranking kept as evidence — and drift (a leader change, a dead provider)
//! is reported from the diff, not memory.

use crate::scout_schedule::{
    append, drift_between, interval_secs, load_last, ScoutDrift, ScoutProviderResult, ScoutRun,
};
use std::collections::BTreeMap;

fn run(
    unix: u64,
    leaders: &[(&str, Option<&str>)],
    providers: Vec<ScoutProviderResult>,
) -> ScoutRun {
    ScoutRun {
        unix,
        leaders: leaders
            .iter()
            .map(|(k, v)| (k.to_string(), v.map(str::to_string)))
            .collect::<BTreeMap<_, _>>(),
        providers,
        catalogue: BTreeMap::new(),
    }
}

fn prov(
    name: &str,
    verified: usize,
    probes: usize,
    transport_failures: usize,
) -> ScoutProviderResult {
    ScoutProviderResult {
        provider: name.to_string(),
        verified,
        probes,
        transport_failures,
    }
}

#[test]
fn brain_scout_schedule_leader_change_is_reported_drift() {
    let prev = run(
        100,
        &[("chat", Some("acme-old")), ("code", Some("acme-same"))],
        vec![],
    );
    let cur = run(
        200,
        &[("chat", Some("acme-new")), ("code", Some("acme-same"))],
        vec![],
    );
    let drift = drift_between(&prev, &cur);
    assert_eq!(drift.len(), 1, "only the changed class drifts");
    assert_eq!(
        drift[0],
        ScoutDrift::LeaderChanged {
            class: "chat".to_string(),
            was: Some("acme-old".to_string()),
            now: Some("acme-new".to_string()),
        }
    );
}

#[test]
fn brain_scout_schedule_dead_provider_is_reported_drift() {
    let prev = run(
        100,
        &[],
        vec![prov("acme-live", 5, 6, 1), prov("acme-still", 6, 6, 0)],
    );
    let cur = run(
        200,
        &[],
        vec![prov("acme-live", 0, 6, 6), prov("acme-still", 6, 6, 0)],
    );
    let drift = drift_between(&prev, &cur);
    assert_eq!(
        drift,
        vec![ScoutDrift::ProviderDied {
            provider: "acme-live".to_string()
        }],
        "answered last run, all-transport-failure now = a dead key/endpoint"
    );
    // A provider absent last run is new, not dead — no death report.
    let fresh = run(200, &[], vec![prov("acme-brandnew", 0, 6, 6)]);
    assert!(drift_between(&prev, &fresh)
        .iter()
        .all(|d| !matches!(d, ScoutDrift::ProviderDied { .. })));
}

#[test]
fn brain_scout_schedule_identical_runs_report_no_drift() {
    let prev = run(
        100,
        &[("chat", Some("acme-a"))],
        vec![prov("acme-a", 6, 6, 0)],
    );
    let cur = run(
        200,
        &[("chat", Some("acme-a"))],
        vec![prov("acme-a", 6, 6, 0)],
    );
    assert!(drift_between(&prev, &cur).is_empty());
}

#[test]
fn brain_scout_schedule_journal_keeps_the_previous_ranking() {
    let dir = std::env::temp_dir().join(format!("susi-scout-journal-{}", std::process::id()));
    let journal = dir.join("runs.jsonl");
    let _ = std::fs::remove_dir_all(&dir);
    let first = run(
        100,
        &[("chat", Some("acme-a"))],
        vec![prov("acme-a", 6, 6, 0)],
    );
    let second = run(
        200,
        &[("chat", Some("acme-b"))],
        vec![prov("acme-b", 6, 6, 0)],
    );
    append(&journal, &first).expect("append first");
    append(&journal, &second).expect("append second");
    // The journal holds both runs, one JSON line each — previous rankings
    // survive as evidence, not just the latest.
    let lines = std::fs::read_to_string(&journal).expect("journal readable");
    assert_eq!(lines.lines().filter(|l| !l.trim().is_empty()).count(), 2);
    let last = load_last(&journal).expect("last run loads");
    assert_eq!(last.unix, 200);
    // And the diff against an earlier run reproduces the recorded drift.
    let drift = drift_between(&first, &last);
    assert!(matches!(
        drift[0],
        ScoutDrift::LeaderChanged { ref class, .. } if class == "chat"
    ));
}

#[test]
fn brain_scout_schedule_cadence_is_bounded_and_overridable() {
    let _env = crate::engines::env_test_lock();
    // SAFETY: serialized by env_test_lock.
    unsafe {
        std::env::remove_var("SUSI_BRAIN_SCOUT_INTERVAL_SECS");
    }
    let default = interval_secs();
    assert!(
        (3_600..=86_400).contains(&default),
        "a scout cadence stays in the daily-or-better band: {default}"
    );
    unsafe {
        std::env::set_var("SUSI_BRAIN_SCOUT_INTERVAL_SECS", "900");
        assert_eq!(interval_secs(), 900);
        std::env::set_var("SUSI_BRAIN_SCOUT_INTERVAL_SECS", "garbage");
        assert_eq!(interval_secs(), default, "bad env falls back, not panics");
        std::env::remove_var("SUSI_BRAIN_SCOUT_INTERVAL_SECS");
    }
}

#[test]
fn brain_scout_schedule_production_wiring() {
    // The schedule must be a real cadence on the live daemon loop — a
    // journal nobody writes is scaffolding.
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../susi-daemon/src/runtime_admin.rs"
    ))
    .expect("runtime_admin.rs readable");
    assert!(
        src.contains("brain_scout"),
        "the maintenance tick must carry a brain_scout probe subject"
    );
    assert!(
        src.contains("run_scheduled"),
        "the maintenance tick must perform the scheduled sweep"
    );
    let sched = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../susi-daemon/src/eco_probe_scheduler.rs"
    ))
    .expect("eco_probe_scheduler.rs readable");
    assert!(
        sched.contains("Decision::Run"),
        "the cadence is the shared probe scheduler's decide/record_run"
    );
}
