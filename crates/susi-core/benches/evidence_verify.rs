#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

use std::hint::black_box;
use std::time::{Duration, Instant};
use susi_core::evidence::{Claim, EvidenceRecord, EvidenceSource};
use susi_core::truth::TruthTransformer;
use susi_paths::tempfile::TempDir;

fn run_bench<F: FnMut()>(name: &str, iterations: u64, mut f: F) {
    let warmup = (iterations / 10).max(16);
    for _ in 0..warmup {
        f();
    }
    let started = Instant::now();
    for _ in 0..iterations {
        f();
    }
    let elapsed: Duration = started.elapsed();
    let per_iter_ns = elapsed.as_nanos() / u128::from(iterations.max(1));
    println!("{name}: {iterations} iterations in {elapsed:?} ({per_iter_ns} ns/iter)");
}

fn bench_evidence_record_signature() {
    run_bench("evidence_record_new_and_signature", 2_000, || {
        let record = EvidenceRecord::new(
            black_box("bench-agent".to_string()),
            black_box(0.95_f32),
            black_box(42_u64),
            Claim {
                subject: "file".into(),
                predicate: "exists".into(),
                value: "true".into(),
            },
            EvidenceSource::System {
                metric: "ok".into(),
                value: "1".into(),
            },
            black_box(0.99_f32),
        );
        let _ = black_box(record.signature);
    });
}

fn bench_truth_transformer_verify() {
    let dir = TempDir::new().expect("tempdir");
    let workspace = dir.path();
    let record =
        TruthTransformer::mission_evidence_record("goal", "agent", "completed with evidence");
    run_bench("truth_transformer_verify_evidence", 2_000, || {
        let _ = TruthTransformer::verify_evidence(black_box(&record), black_box(workspace));
    });
}

fn main() {
    bench_evidence_record_signature();
    bench_truth_transformer_verify();
}
