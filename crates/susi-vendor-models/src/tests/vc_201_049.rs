//! Drain local AI workloads for suspend and maintenance (VC-201-049).

use crate::workload_drain::{DrainManager, DrainOutcome, Job, JobKind};

fn job(id: &str, kind: JobKind, progress: f64) -> Job {
    Job {
        id: id.to_string(),
        kind,
        progress,
    }
}

#[test]
fn vc_201_049_drain_quiesces_and_checkpoints_resumable() {
    let mut d = DrainManager::new();
    d.submit(job("dl-1", JobKind::Resumable, 0.6)).unwrap();
    d.submit(job("req-9", JobKind::OneShot, 0.0)).unwrap();
    let out = d.drain();
    assert!(!d.accepting());
    assert!(d.submit(job("late", JobKind::OneShot, 0.0)).is_err());
    assert!(out.iter().any(|o| matches!(
        o,
        DrainOutcome::Checkpointed { id, progress } if id == "dl-1" && (*progress - 0.6).abs() < f64::EPSILON
    )));
    assert!(d
        .checkpoints
        .iter()
        .any(|c| c.id == "dl-1" && (c.progress - 0.6).abs() < f64::EPSILON));
}

#[test]
fn vc_201_049_interrupted_requests_report_their_actual_outcome() {
    let mut d = DrainManager::new();
    d.submit(job("req-1", JobKind::OneShot, 0.0)).unwrap();
    let out = d.drain();
    assert!(out
        .iter()
        .any(|o| matches!(o, DrainOutcome::Interrupted { id } if id == "req-1")));
    // No fabricated success — the only outcomes are checkpoint/interrupt.
    assert!(out.iter().all(|o| matches!(
        o,
        DrainOutcome::Interrupted { .. } | DrainOutcome::Checkpointed { .. }
    )));
}

#[test]
fn vc_201_049_stale_gpu_readiness_is_discarded_on_drain() {
    let mut d = DrainManager::new();
    d.mark_ready("cuda0");
    d.mark_ready("npu0");
    assert!(d.is_ready("cuda0"));
    d.drain();
    assert!(!d.is_ready("cuda0"), "drain discards all readiness");
    assert!(!d.is_ready("npu0"));
}

#[test]
fn vc_201_049_resume_reprobes_and_replays_checkpoints() {
    let mut d = DrainManager::new();
    d.mark_ready("cuda0");
    d.submit(job("dl-1", JobKind::Resumable, 0.3)).unwrap();
    d.submit(job("dl-2", JobKind::Resumable, 0.9)).unwrap();
    d.drain();

    // Driver reset: cuda0 is gone, only npu0 comes back.
    let cps = d.resume(&|| vec!["npu0".to_string()]);
    assert!(d.accepting());
    assert!(
        !d.is_ready("cuda0"),
        "runtime missing from probe is not ready"
    );
    assert!(d.is_ready("npu0"));
    assert_eq!(cps.len(), 2);
    // Resumed job continues from recorded progress, not zero.
    let dl2 = cps.into_iter().find(|c| c.id == "dl-2").unwrap();
    d.resume_job(dl2);
    let j = d.job("dl-2").unwrap();
    assert!((j.progress - 0.9).abs() < f64::EPSILON);
}

#[test]
fn vc_201_049_drain_on_empty_workload_is_safe() {
    let mut d = DrainManager::new();
    let out = d.drain();
    assert!(out.is_empty());
    assert!(d.resume(&|| vec![]).is_empty());
    assert!(d.accepting());
}
