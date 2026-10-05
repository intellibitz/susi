//! Production-entry regressions for verified swarm closure (VC-201-028).
//!
//! These tests exercise the real `Gate`/`verified_closure` boundary. A worker
//! output, a zero-test command, a reviewer alias, a forged receipt, a moved
//! branch, or a failing acceptance must all leave the queue open.

use crate::agent_integration::{
    AcceptRunner, AcceptanceObservation, Gate, HeadProbe, IntegrateOutcome, IntegrationQueue,
    Integrator, MergeResult, Verdict, Verifier,
};
use crate::independent_verify::ReviewReceipt;
use crate::roadmap_agents::TaskSpec;
use crate::worker_recovery::{Assign, JobLedger};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const NOW: u64 = 1_700_000_000;

struct Queue {
    done: Mutex<BTreeSet<String>>,
}

impl Queue {
    fn new() -> Self {
        Self {
            done: Mutex::new(BTreeSet::new()),
        }
    }
}

impl IntegrationQueue for Queue {
    fn is_done(&self, id: &str) -> bool {
        self.done
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains(id)
    }

    fn deps(&self, _id: &str) -> Vec<String> {
        Vec::new()
    }

    fn finish(&self, id: &str) {
        self.done
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(id.to_string());
    }

    fn reopen(&self, id: &str, _why: &str) {
        self.done
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(id);
    }
}

struct PassAccept;
impl AcceptRunner for PassAccept {
    fn run(&self, _worktree: &Path, _command: &[String]) -> Result<(), String> {
        Ok(())
    }
}

struct ZeroTestAccept;
impl AcceptRunner for ZeroTestAccept {
    fn run(&self, _worktree: &Path, _command: &[String]) -> Result<(), String> {
        Ok(())
    }

    fn run_with_evidence(
        &self,
        _worktree: &Path,
        command: &[String],
    ) -> Result<AcceptanceObservation, String> {
        let mut observation = AcceptanceObservation::successful(command);
        observation.tests_run = 0;
        Ok(observation)
    }
}

struct FailAccept;
impl AcceptRunner for FailAccept {
    fn run(&self, _worktree: &Path, _command: &[String]) -> Result<(), String> {
        Err("exit 1: acceptance failed".into())
    }
}

struct CleanMerge;
impl Integrator for CleanMerge {
    fn integrate(&self, _repo: &Path, _branch: &str) -> Result<MergeResult, String> {
        Ok(MergeResult::Clean("merge-sha".into()))
    }
}

struct GoodReview;
impl Verifier for GoodReview {
    fn verify(&self, _worktree: &Path, _task: &TaskSpec) -> Verdict {
        Verdict::Approve
    }

    fn authenticated_identity(&self) -> Option<&str> {
        Some("reviewer-1")
    }

    fn review_receipt(
        &self,
        _worktree: &Path,
        task: &TaskSpec,
        implementer: &str,
        source_sha: &str,
        acceptance: &AcceptanceObservation,
    ) -> Option<ReviewReceipt> {
        Some(ReviewReceipt::new(
            "reviewer-1",
            implementer,
            &task.id,
            &acceptance.tool,
            &acceptance.arguments_digest,
            &acceptance.result_digest,
            source_sha,
        ))
    }
}

struct ForgedReview;
impl Verifier for ForgedReview {
    fn verify(&self, _worktree: &Path, _task: &TaskSpec) -> Verdict {
        Verdict::Approve
    }

    fn authenticated_identity(&self) -> Option<&str> {
        Some("reviewer-1")
    }

    fn review_receipt(
        &self,
        _worktree: &Path,
        task: &TaskSpec,
        implementer: &str,
        source_sha: &str,
        acceptance: &AcceptanceObservation,
    ) -> Option<ReviewReceipt> {
        let mut receipt = ReviewReceipt::new(
            "reviewer-1",
            implementer,
            &task.id,
            &acceptance.tool,
            &acceptance.arguments_digest,
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            source_sha,
        );
        receipt.review_digest = "sha256:forged".into();
        Some(receipt)
    }
}

struct AliasReview;
impl Verifier for AliasReview {
    fn verify(&self, _worktree: &Path, _task: &TaskSpec) -> Verdict {
        Verdict::Approve
    }

    fn authenticated_identity(&self) -> Option<&str> {
        Some("reviewer-1")
    }

    fn review_receipt(
        &self,
        _worktree: &Path,
        task: &TaskSpec,
        implementer: &str,
        source_sha: &str,
        acceptance: &AcceptanceObservation,
    ) -> Option<ReviewReceipt> {
        Some(ReviewReceipt::new(
            "implementer-1",
            implementer,
            &task.id,
            &acceptance.tool,
            &acceptance.arguments_digest,
            &acceptance.result_digest,
            source_sha,
        ))
    }
}

struct StaticHead;
impl HeadProbe for StaticHead {
    fn revision(&self, _worktree: &Path, _branch: &str) -> Result<String, String> {
        Ok("commit-sha".into())
    }
}

struct DriftingHead {
    calls: AtomicUsize,
}
impl HeadProbe for DriftingHead {
    fn revision(&self, _worktree: &Path, _branch: &str) -> Result<String, String> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(format!("commit-sha-{call}"))
    }
}

fn task(id: &str) -> TaskSpec {
    TaskSpec {
        id: id.into(),
        task_class: "coding".into(),
        accept: vec!["cargo".into(), "test".into(), "fixture".into()],
        title: id.into(),
    }
}

fn ledger(job: &str) -> JobLedger {
    let jobs = JobLedger::with_clock(|| NOW);
    jobs.assign(Assign {
        job_id: job,
        worker: "implementer-1",
        model: "model-1",
        worktree: std::env::temp_dir(),
        lease_until: NOW + 300,
    });
    jobs.complete(job, 1).unwrap();
    jobs
}

fn integrate(
    jobs: &JobLedger,
    queue: &Queue,
    accept: &dyn AcceptRunner,
    verifier: Option<&dyn Verifier>,
    head: &dyn HeadProbe,
) -> IntegrateOutcome {
    let gate = Gate {
        ledger: jobs,
        queue,
        accept,
        integrator: &CleanMerge,
        verifier,
        head,
    };
    gate.integrate_job("T-VERIFIED", "worker-branch", &task("T-VERIFIED"))
}

#[test]
fn swarm_gap_verified_closure_dispatch_output_cannot_finish_without_gate() {
    let jobs = ledger("T-VERIFIED");
    let queue = Queue::new();
    let outcome = integrate(&jobs, &queue, &PassAccept, None, &StaticHead);

    assert!(matches!(outcome, IntegrateOutcome::VerifyRejected { .. }));
    assert!(!queue.is_done("T-VERIFIED"));
}

#[test]
fn swarm_gap_verified_closure_zero_test_acceptance_stays_open() {
    let jobs = ledger("T-VERIFIED");
    let queue = Queue::new();
    let review = GoodReview;
    let outcome = integrate(&jobs, &queue, &ZeroTestAccept, Some(&review), &StaticHead);

    assert!(matches!(outcome, IntegrateOutcome::AcceptanceFailed { .. }));
    assert!(!queue.is_done("T-VERIFIED"));
}

#[test]
fn swarm_gap_verified_closure_forged_receipt_stays_open() {
    let jobs = ledger("T-VERIFIED");
    let queue = Queue::new();
    let outcome = integrate(&jobs, &queue, &PassAccept, Some(&ForgedReview), &StaticHead);

    assert!(matches!(outcome, IntegrateOutcome::VerifyRejected { .. }));
    assert!(!queue.is_done("T-VERIFIED"));
}

#[test]
fn swarm_gap_verified_closure_self_review_alias_stays_open() {
    let jobs = ledger("T-VERIFIED");
    let queue = Queue::new();
    let outcome = integrate(&jobs, &queue, &PassAccept, Some(&AliasReview), &StaticHead);

    assert!(matches!(outcome, IntegrateOutcome::VerifyRejected { .. }));
    assert!(!queue.is_done("T-VERIFIED"));
}

#[test]
fn swarm_gap_verified_closure_changed_sha_after_review_stays_open() {
    let jobs = ledger("T-VERIFIED");
    let queue = Queue::new();
    let review = GoodReview;
    let outcome = integrate(
        &jobs,
        &queue,
        &PassAccept,
        Some(&review),
        &DriftingHead {
            calls: AtomicUsize::new(0),
        },
    );

    assert!(matches!(outcome, IntegrateOutcome::VerifyRejected { .. }));
    assert!(!queue.is_done("T-VERIFIED"));
}

#[test]
fn swarm_gap_verified_closure_failing_acceptance_stays_open() {
    let jobs = ledger("T-VERIFIED");
    let queue = Queue::new();
    let review = GoodReview;
    let outcome = integrate(&jobs, &queue, &FailAccept, Some(&review), &StaticHead);

    assert!(matches!(outcome, IntegrateOutcome::AcceptanceFailed { .. }));
    assert!(!queue.is_done("T-VERIFIED"));
}

#[test]
fn swarm_gap_verified_closure_bound_receipt_finishes_and_is_stored() {
    let jobs = ledger("T-VERIFIED");
    let queue = Queue::new();
    let review = GoodReview;
    let outcome = integrate(&jobs, &queue, &PassAccept, Some(&review), &StaticHead);

    assert!(matches!(outcome, IntegrateOutcome::Integrated { .. }));
    assert!(queue.is_done("T-VERIFIED"));
    let receipts = jobs.get("T-VERIFIED").unwrap().receipts;
    assert_eq!(receipts.len(), 1);
    assert!(receipts[0].effect.starts_with("verified review sha256:"));
}
