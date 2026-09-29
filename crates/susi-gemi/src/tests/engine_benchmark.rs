//! Deterministic engine benchmark harness tests.

use crate::engine_benchmark::{
    estimate_tokens, run_engine_benchmark, write_bench_evidence, FakeProvider,
};
use std::time::Duration;

#[test]
fn engine_benchmark_fake_provider_records_tps_latency_success() {
    let fake = FakeProvider {
        engine: "fake-engine".into(),
        model: "fake-model".into(),
        reply: "one two three four five".into(),
        latency: Duration::from_millis(12),
        fail: false,
    };
    let samples = run_engine_benchmark(&fake, &["ping", "pong"]);
    assert_eq!(samples.len(), 2);
    assert!(samples.iter().all(|s| s.success));
    assert!(samples.iter().all(|s| s.tokens_per_sec > 0.0));
    assert!(samples.iter().all(|s| s.first_token_latency_ms == 12));
    assert_eq!(samples[0].engine, "fake-engine");
    assert_eq!(samples[0].model, "fake-model");
}

#[test]
fn engine_benchmark_records_failure() {
    let fake = FakeProvider {
        engine: "e".into(),
        model: "m".into(),
        reply: String::new(),
        latency: Duration::from_millis(1),
        fail: true,
    };
    let samples = run_engine_benchmark(&fake, &["x"]);
    assert_eq!(samples.len(), 1);
    assert!(!samples[0].success);
    assert_eq!(samples[0].tokens_per_sec, 0.0);
}

#[test]
fn engine_benchmark_writes_evidence_file() {
    let dir = std::env::temp_dir().join(format!(
        "susi-bench-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let path = dir.join("bench.jsonl");
    let fake = FakeProvider {
        engine: "e".into(),
        model: "m".into(),
        reply: "a b c".into(),
        latency: Duration::from_millis(1),
        fail: false,
    };
    let samples = run_engine_benchmark(&fake, &["hi"]);
    write_bench_evidence(&path, &samples).expect("write");
    let raw = std::fs::read_to_string(&path).expect("read");
    assert!(raw.contains("\"e\""));
    assert_eq!(estimate_tokens("a b c"), 3);
    let _ = std::fs::remove_dir_all(&dir);
}
