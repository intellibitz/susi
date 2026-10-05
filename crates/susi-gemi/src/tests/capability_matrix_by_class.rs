//! A measured capability matrix per model and task class (VC-202-022,
//! T-DEEPSEEK-125). The question is never which model is best but which
//! model is best at *this*: the matrix carries success rate, latency and
//! cost per verified outcome per (provider, class), the scheduled scout
//! refreshes it every run, and `susi brain matrix` renders it for a human.

use crate::capability_matrix::{build_in, persist};
use crate::engines::brain::{Store, TaskClass};

fn store_with(provider: &str, class: TaskClass, oks: u32, fails: u32) -> Store {
    let mut s = Store::default();
    for _ in 0..oks {
        s.record(provider, class, true, 100);
    }
    for _ in 0..fails {
        s.record(provider, class, false, 0);
    }
    s
}

#[test]
fn capability_matrix_by_class_is_per_class_not_one_global_score() {
    // A is proven at Chat but a Code failure; B the reverse. A matrix
    // answers "best at this": A leads Chat, B leads Code — neither has a
    // single global score that could blur the two.
    let mut s = Store::default();
    for _ in 0..8 {
        s.record("acme-a", TaskClass::Chat, true, 120);
        s.record("acme-b", TaskClass::Code, true, 90);
    }
    for _ in 0..8 {
        s.record("acme-a", TaskClass::Code, false, 0);
        s.record("acme-b", TaskClass::Chat, false, 0);
    }
    let names = vec!["acme-a".to_string(), "acme-b".to_string()];
    let m = build_in(&s, &names, 1_000);
    let chat = &m.classes["chat"];
    let code = &m.classes["code"];
    assert_eq!(chat[0].provider, "acme-a", "proven-at-chat leads Chat");
    assert_eq!(code[0].provider, "acme-b", "proven-at-code leads Code");
}

#[test]
fn capability_matrix_by_class_carries_the_measured_dimensions() {
    let s = store_with("acme-a", TaskClass::Chat, 6, 2);
    let names = vec!["acme-a".to_string()];
    let m = build_in(&s, &names, 1_000);
    let cell = &m.classes["chat"][0];
    assert_eq!(cell.samples, 8);
    assert_eq!(cell.success_rate, Some(0.75));
    assert_eq!(cell.avg_latency_ms, Some(100));
    // An untried provider has no measured rate — honest None, not a guess.
    let fresh = build_in(&s, &["acme-never".to_string()], 1_000);
    let empty = &fresh.classes["chat"][0];
    assert_eq!(empty.samples, 0);
    assert_eq!(empty.success_rate, None);
    assert_eq!(empty.avg_latency_ms, None);
}

#[test]
fn capability_matrix_by_class_is_refreshed_by_the_scout_run() {
    let _env = crate::engines::env_test_lock();
    let evidence = std::env::temp_dir().join(format!("susi-cat-matrix-{}", std::process::id()));
    let _ = std::fs::remove_file(&evidence);
    // SAFETY: serialized by env_test_lock; restored before drop.
    unsafe {
        std::env::set_var("SUSI_BRAIN_EVIDENCE_FILE", &evidence);
    }
    let _ = crate::engines::brain::reset();
    crate::engines::brain::record_outcome("acme-a", TaskClass::Chat, true, 150);

    let dir = std::env::temp_dir().join(format!("susi-matrix-run-{}", std::process::id()));
    let journal = dir.join("runs.jsonl");
    let _ = std::fs::remove_dir_all(&dir);
    crate::scout_schedule::finish_run(1_000, &journal, &["acme-a".to_string()], &[]);
    // The matrix file lands next to the journal — refreshed by the scout,
    // not remembered.
    let matrix_path = dir.join("brain_capability_matrix.json");
    let text = std::fs::read_to_string(&matrix_path).expect("matrix persisted");
    let m: serde_json::Value = serde_json::from_str(&text).expect("matrix parses");
    let cell = &m["classes"]["chat"][0];
    assert_eq!(cell["provider"], "acme-a");
    assert_eq!(cell["samples"], 1);
    assert_eq!(cell["success_rate"], 1.0);
    assert_eq!(cell["avg_latency_ms"], 150);

    unsafe {
        std::env::remove_var("SUSI_BRAIN_EVIDENCE_FILE");
    }
    let _ = crate::engines::brain::reset();
}

#[test]
fn capability_matrix_by_class_round_trips_and_is_human_readable() {
    let s = store_with("acme-a", TaskClass::Chat, 3, 0);
    let m = build_in(&s, &["acme-a".to_string()], 1_000);
    let dir = std::env::temp_dir().join(format!("susi-matrix-json-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("matrix.json");
    persist(&path, &m).expect("persist");
    let text = std::fs::read_to_string(&path).expect("readable");
    assert!(
        text.contains("\"success_rate\""),
        "pretty JSON a human reads"
    );
    let back: crate::capability_matrix::CapabilityMatrix =
        serde_json::from_str(&text).expect("round-trips");
    assert_eq!(back, m);
}

#[test]
fn capability_matrix_by_class_production_surface_wired() {
    // The matrix is refreshed by the scheduled scout (not only on demand)
    // and readable by a human through `susi brain matrix`.
    let sched = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/scout_schedule.rs"
    ))
    .expect("scout_schedule.rs readable");
    assert!(
        sched.contains("brain_capability_matrix.json"),
        "the scout refresh writes the matrix artifact"
    );
    assert!(
        sched.contains("capability_matrix::build_in"),
        "the matrix the scout writes is the same rank data the cascade uses"
    );
    let cli = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/cli/brain_cli.rs"
    ))
    .expect("brain_cli.rs readable");
    assert!(
        cli.contains("BrainCommands::Matrix"),
        "`susi brain matrix` renders it for a human"
    );
}
