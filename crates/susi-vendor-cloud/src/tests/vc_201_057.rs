use crate::preemptible::{EvictOutcome, Job, PreemptibleSet};

fn job(id: &str, checkpointable: bool, progress: f64, secs: u64) -> Job {
    Job {
        id: id.into(),
        checkpointable,
        progress,
        compute_secs: secs,
    }
}

#[test]
fn vc_201_057_eviction_checkpoints_eligible_and_interrupts_rest() {
    let mut set = PreemptibleSet::new(7);
    set.jobs.insert("a".into(), job("a", true, 0.4, 30));
    set.jobs.insert("b".into(), job("b", false, 0.9, 50));
    let out = set.evict();
    assert_eq!(
        out,
        vec![
            EvictOutcome::Checkpointed {
                id: "a".into(),
                progress: 0.4
            },
            EvictOutcome::Interrupted { id: "b".into() },
        ]
    );
    // All thrown-away compute is billed as duplicate.
    assert_eq!(set.ledger.duplicate_secs, 80);
    assert_eq!(set.billed_secs(), 80);
}

#[test]
fn vc_201_057_resume_resumes_at_progress_not_zero() {
    let mut set = PreemptibleSet::new(3);
    set.jobs.insert("j".into(), job("j", true, 0.75, 10));
    set.evict();
    let resumed = set.resume(3, 10);
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].progress, 0.75);
    assert_eq!(set.jobs["j"].progress, 0.75);
}

#[test]
fn vc_201_057_stale_or_newer_fencing_token_never_resumes() {
    let mut set = PreemptibleSet::new(9);
    set.jobs.insert("j".into(), job("j", true, 0.5, 5));
    set.evict();
    // A grant issued under an *older* token than the checkpoint's owner
    // must not resurrect work a newer owner already holds — stays parked.
    let resumed = set.resume(8, 10);
    assert!(resumed.is_empty());
    assert_eq!(set.checkpoints.len(), 1);
    // The rightful token resumes.
    assert_eq!(set.resume(9, 10).len(), 1);
}

#[test]
fn vc_201_057_retry_budget_bounds_resumes_per_grant() {
    let mut set = PreemptibleSet::new(1);
    for i in 0..3 {
        set.jobs
            .insert(format!("j{i}"), job("j", true, 0.1 * (i as f64 + 1.0), 1));
    }
    set.evict();
    let resumed = set.resume(1, 2);
    assert_eq!(resumed.len(), 2);
    assert_eq!(set.checkpoints.len(), 1, "over-budget stays parked");
    // Next grant picks it up.
    assert_eq!(set.resume(1, 2).len(), 1);
}
