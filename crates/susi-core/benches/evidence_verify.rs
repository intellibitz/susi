use criterion::{black_box, criterion_group, criterion_main, Criterion};
use susi_core::evidence::{Claim, EvidenceRecord, EvidenceSource};
use susi_core::truth::TruthTransformer;
use tempfile::TempDir;

fn bench_evidence_record_signature(c: &mut Criterion) {
    c.bench_function("evidence_record_new_and_signature", |b| {
        b.iter(|| {
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
            black_box(record.signature)
        })
    });
}

fn bench_truth_transformer_verify(c: &mut Criterion) {
    let dir = TempDir::new().expect("tempdir");
    let workspace = dir.path();
    let record =
        TruthTransformer::mission_evidence_record("goal", "agent", "completed with evidence");
    c.bench_function("truth_transformer_verify_evidence", |b| {
        b.iter(|| {
            let _ = TruthTransformer::verify_evidence(black_box(&record), black_box(workspace));
        })
    });
}

criterion_group!(
    benches,
    bench_evidence_record_signature,
    bench_truth_transformer_verify
);
criterion_main!(benches);
