//! Jobs status and control surface: what the CLI/API reports for every
//! parallel model job — and the controls to pause, resume, cancel, and
//! rebalance them.
//!
//! Reporting rules:
//! - Model is the *opaque* `provider/model/credfp8` id; credentials never
//!   appear.
//! - A queued job always reports *why* it can't start (which scope/cap is
//!   saturated), not a bare "waiting".
//! - Partial execution is never reported as success: only a ledger
//!   `Completed` state (fence-verified) shows as done.
//! - `pause` stops NEW dispatch; running jobs are untouched — nothing is
//!   abandoned silently.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use serde::{Deserialize, Serialize};

use crate::parallel_admission::{AdmissionController, Backpressure};
use crate::worker_recovery::{JobLedger, JobState};

/// Status of a dispatched job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    /// Waiting in the bounded admission queue — `reason` says exactly what
    /// capacity is missing.
    Queued,
    /// Worker actively running.
    Running,
    /// Fence-verified completion.
    Completed,
    /// Terminal failure.
    Failed,
    /// Model lost; job preserved awaiting an eligible replacement.
    Recovering,
    /// Claimed but not yet heartbeating.
    Assigned,
    /// Cancelled by operator.
    Cancelled,
}

/// A job's reported status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStatus {
    /// Job/task id.
    pub job_id: String,
    /// Current worker.
    pub worker: String,
    /// Opaque model reference — `provider/model/credfp8`, no key material.
    pub model: String,
    /// Isolated worktree path.
    pub worktree: String,
    /// Claim lease expiry (unix secs).
    pub lease_until: u64,
    /// Lifecycle.
    pub state: StatusKind,
    /// Why it's queued, when it is.
    pub blocked_reason: Option<String>,
    /// Progress note.
    pub progress: String,
    /// Effects already applied (receipt count — the audit trail).
    pub receipts: usize,
}

/// Dispatch-wide view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchStatus {
    /// Every job's status.
    pub jobs: Vec<JobStatus>,
    /// Jobs actually executing right now.
    pub running: usize,
    /// Jobs queued and their reasons.
    pub queued: usize,
    /// New dispatch paused?
    pub paused: bool,
    /// Configured worker bound.
    pub max_workers: u32,
    /// Admission queue depth.
    pub admission_queue: usize,
}

/// The controller: status reads over the ledger + admission queue; controls
/// for dispatch pause, concurrency, cancellation.
pub struct JobsController<'a> {
    ledger: &'a JobLedger,
    admission: Option<&'a AdmissionController>,
    paused: AtomicBool,
    max_workers: AtomicU32,
}

impl<'a> JobsController<'a> {
    /// Build the controller over the shared stores.
    pub fn new(
        ledger: &'a JobLedger,
        admission: Option<&'a AdmissionController>,
        max_workers: u32,
    ) -> Self {
        Self {
            ledger,
            admission,
            paused: AtomicBool::new(false),
            max_workers: AtomicU32::new(max_workers),
        }
    }

    /// Full dispatch view.
    #[must_use]
    pub fn status(&self) -> DispatchStatus {
        let paused = self.paused.load(Ordering::SeqCst);
        let mut jobs = Vec::new();
        let mut running = 0usize;
        let mut queued = 0usize;
        for a in self.ledger.all() {
            let (mut state, mut reason) = match &a.state {
                JobState::Assigned => (StatusKind::Assigned, self.queue_reason(&a.job_id)),
                JobState::Running => (StatusKind::Running, None),
                JobState::AwaitingModel => (
                    StatusKind::Recovering,
                    Some("model lost; awaiting eligible replacement".into()),
                ),
                JobState::Completed => (StatusKind::Completed, None),
                JobState::Failed(w) => (StatusKind::Failed, Some(w.clone())),
                JobState::Cancelled => (StatusKind::Cancelled, None),
            };
            if matches!(a.state, JobState::Running) {
                running += 1;
            }
            // Assigned-but-blocked and paused-new-dispatch surface as Queued.
            if matches!(a.state, JobState::Assigned) {
                if paused {
                    state = StatusKind::Queued;
                    reason = Some("dispatch paused".into());
                } else if reason.is_some() {
                    state = StatusKind::Queued;
                }
                if state == StatusKind::Queued {
                    queued += 1;
                }
            }
            jobs.push(JobStatus {
                job_id: a.job_id.clone(),
                worker: a.worker.clone(),
                model: a.model.clone(),
                worktree: a.worktree.display().to_string(),
                lease_until: a.lease_until,
                state,
                blocked_reason: reason,
                progress: a.progress.clone(),
                receipts: a.receipts.len(),
            });
        }
        DispatchStatus {
            jobs,
            running,
            queued,
            paused,
            max_workers: self.max_workers.load(Ordering::SeqCst),
            admission_queue: self.admission.map_or(0, |a| a.queued()),
        }
    }

    /// Why a job can't start right now (admission denial) — `None` when it
    /// would admit cleanly.
    #[must_use]
    pub fn queue_reason(&self, _job_id: &str) -> Option<String> {
        let adm = self.admission?;
        let req = crate::parallel_admission::AdmitRequest {
            mission: "probe",
            job: "__status_probe__",
            scopes: vec![],
            cpu_millis: 0,
            ram_mb: 0,
            vram_mb: 0,
            subprocesses: 0,
            holds_job_slot: true,
        };
        match adm.probe(&req) {
            Ok(()) => None,
            Err(Backpressure::Queued { position }) => {
                Some(format!("queued at position {position}"))
            }
            Err(Backpressure::GlobalFull) => Some("global concurrency cap reached".into()),
            Err(Backpressure::MissionFull) => Some("mission concurrency cap reached".into()),
            Err(Backpressure::LocalExhausted) => Some("local CPU/RAM/VRAM exhausted".into()),
            Err(Backpressure::ScopeFull { scope }) => Some(format!("scope {scope} saturated")),
            Err(Backpressure::QueueFull) => Some("admission queue full".into()),
        }
    }

    /// Stop admitting new jobs; running jobs finish undisturbed.
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }
    /// Resume dispatch.
    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }
    /// Is dispatch paused?
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
    /// Update the worker bound for subsequent dispatches.
    pub fn set_concurrency(&self, workers: u32) {
        self.max_workers.store(workers.max(1), Ordering::SeqCst);
    }
    /// Current worker bound.
    #[must_use]
    pub fn concurrency(&self) -> u32 {
        self.max_workers.load(Ordering::SeqCst)
    }
    /// Cancel a job — propagates through the ledger; the running worker's
    /// next heartbeat will see `Cancelled`.
    pub fn cancel(&self, job_id: &str) {
        self.ledger.cancel(job_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel_admission::{AdmissionLimits, AdmitRequest};
    use crate::worker_recovery::Assign;
    use std::path::PathBuf;

    const T0: u64 = 1_700_000_000;
    fn clock() -> u64 {
        T0
    }

    fn assign<'a>(job: &'a str, worker: &'a str, model: &'a str) -> Assign<'a> {
        Assign {
            job_id: job,
            worker,
            model,
            worktree: PathBuf::from(format!("/wt/{job}")),
            lease_until: T0 + 300,
        }
    }

    #[test]
    fn parallel_jobs_status_reports_redacted_multi_model_view() {
        let l = JobLedger::with_clock(clock);
        l.assign(assign("j1", "w1", "prov-a/m1/deadbeef"));
        l.assign(assign("j2", "w2", "prov-b/m2/cafef00d"));
        l.heartbeat("j1", 1, T0 + 300).unwrap();
        let c = JobsController::new(&l, None, 8);
        let s = c.status();
        assert_eq!(s.running, 1);
        assert_eq!(s.jobs.len(), 2);
        let j1 = s.jobs.iter().find(|j| j.job_id == "j1").unwrap();
        assert_eq!(j1.state, StatusKind::Running);
        // Opaque model only — no key material anywhere in the report.
        let ser = serde_json::to_string(&s).unwrap();
        assert!(ser.contains("prov-a/m1/deadbeef"));
        assert!(!ser.contains("api_key"));
        assert!(!ser.contains("sk-"));
    }

    #[test]
    fn parallel_jobs_status_queued_jobs_report_reasons() {
        // All workers blocked: global cap held by one job; status explains.
        let l = JobLedger::with_clock(clock);
        l.assign(assign("busy", "w1", "m1"));
        l.heartbeat("busy", 1, T0 + 300).unwrap();
        l.assign(assign("j-wait", "w2", "m2"));
        let lim = AdmissionLimits {
            global_jobs: 1,
            ..Default::default()
        };
        let adm = AdmissionController::new(lim);
        let _t = adm
            .admit(&AdmitRequest {
                mission: "m",
                job: "busy",
                scopes: vec![],
                cpu_millis: 0,
                ram_mb: 0,
                vram_mb: 0,
                subprocesses: 0,
                holds_job_slot: true,
            })
            .unwrap();
        let c = JobsController::new(&l, Some(&adm), 8);
        let s = c.status();
        let jw = s.jobs.iter().find(|j| j.job_id == "j-wait").unwrap();
        assert_eq!(jw.state, StatusKind::Queued);
        assert_eq!(
            jw.blocked_reason.as_deref(),
            Some("global concurrency cap reached")
        );
    }

    #[test]
    fn parallel_jobs_status_pause_stops_dispatch_not_running_work() {
        let l = JobLedger::with_clock(clock);
        l.assign(assign("j1", "w1", "m1"));
        l.heartbeat("j1", 1, T0 + 300).unwrap();
        l.assign(assign("j2", "w2", "m2"));
        let c = JobsController::new(&l, None, 8);
        c.pause();
        let s = c.status();
        assert!(s.paused);
        assert_eq!(s.running, 1, "running work untouched by pause");
        let j2 = s.jobs.iter().find(|j| j.job_id == "j2").unwrap();
        assert_eq!(j2.blocked_reason.as_deref(), Some("dispatch paused"));
        c.resume();
        assert!(!c.is_paused());
    }

    #[test]
    fn parallel_jobs_status_cancel_and_recovery_states() {
        let l = JobLedger::with_clock(clock);
        l.assign(assign("j1", "w1", "m1"));
        l.assign(assign("j2", "w2", "m2"));
        let c = JobsController::new(&l, None, 8);
        c.cancel("j1");
        l.fail("j2", 1, "model lost credit").unwrap();
        l.reassign("j2", "w3", "m3", T0 + 600);
        let s = c.status();
        let j1 = s.jobs.iter().find(|j| j.job_id == "j1").unwrap();
        let j2 = s.jobs.iter().find(|j| j.job_id == "j2").unwrap();
        assert_eq!(j1.state, StatusKind::Cancelled);
        assert_eq!(j2.state, StatusKind::Assigned); // reassigned, runnable
        assert_eq!(j2.worker, "w3");
    }

    #[test]
    fn parallel_jobs_status_concurrency_control() {
        let l = JobLedger::with_clock(clock);
        let c = JobsController::new(&l, None, 4);
        assert_eq!(c.concurrency(), 4);
        c.set_concurrency(2);
        assert_eq!(c.status().max_workers, 2);
        c.set_concurrency(0); // clamped to 1 — never zero workers
        assert_eq!(c.concurrency(), 1);
    }

    #[test]
    fn parallel_jobs_status_partial_never_reports_success() {
        // A failed-then-reassigned job is Assigned, never Completed; only a
        // fence-verified complete reports done.
        let l = JobLedger::with_clock(clock);
        l.assign(assign("j1", "w1", "m1"));
        l.record_receipt("j1", 1, "applied edit").unwrap();
        l.fail("j1", 1, "crash").unwrap();
        let c = JobsController::new(&l, None, 8);
        let s = c.status();
        let j1 = s.jobs.iter().find(|j| j.job_id == "j1").unwrap();
        assert_ne!(j1.state, StatusKind::Completed);
        assert_eq!(j1.receipts, 1);
    }
}
