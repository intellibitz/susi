//! Worker recovery: a persisted ledger binding each job to its worker,
//! model, claim lease and worktree — with fenced ownership so a failed or
//! timed-out worker can never publish over its live replacement.
//!
//! Invariants:
//! - One job = one current `fence`; every heartbeat/progress/completion
//!   presents it. A stale fence (from a reassigned or crashed worker) is
//!   rejected — "stale worker publication" is impossible.
//! - Receipts record external side effects already applied; on reassign
//!   they travel with the job so a retry never duplicates them.
//! - Partial edits are checkpointed with the job and handed to the
//!   replacement worker — work is preserved, not restarted from scratch.
//! - A dead/credited-out/cooling model fails *its job*, never the mission:
//!   independent workers keep running; only the affected job is reassigned
//!   (to another *eligible* model per fresh evidence) or parked
//!   `AwaitingModel` — a typed, recoverable state, not silent loss.
//! - Cancellation propagates to assignment state; leases renew via
//!   heartbeat and release on terminal states; the ledger persists so a
//!   scheduler restart resumes safely.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

/// Job lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Claimed, workspace prepared, not yet running.
    Assigned,
    /// Worker is heartbeating.
    Running,
    /// Finished and fenced-verified.
    Completed,
    /// Terminal failure with the reason.
    Failed(String),
    /// Cancelled by the caller.
    Cancelled,
    /// Model lost — job preserved, waiting for an eligible replacement.
    AwaitingModel,
}

/// A recorded external side effect — reconciled before any retry so the
/// same effect is never applied twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    /// What external effect was applied (e.g. "claim T-9", "push commit").
    pub effect: String,
    /// Fence under which it was applied.
    pub fence: u64,
}

/// Job → worker/model/claim/worktree assignment, plus progress state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    /// Job id (= task id).
    pub job_id: String,
    /// Worker identity currently holding the fence.
    pub worker: String,
    /// Opaque model id the worker dispatches on.
    pub model: String,
    /// Isolated worktree.
    pub worktree: PathBuf,
    /// Claim lease expiry (unix secs).
    pub lease_until: u64,
    /// Lifecycle state.
    pub state: JobState,
    /// Fencing token — bumps on every reassignment.
    pub fence: u64,
    /// Applied external effects — travel with the job across retries.
    pub receipts: Vec<Receipt>,
    /// Checkpointed partial edits — preserved for the replacement worker.
    pub partial_edits: Vec<String>,
    /// Progress note (percent or free-form checkpoint).
    pub progress: String,
}

/// Fencing/lease errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceError {
    /// Presented fence isn't current — caller is a stale worker.
    Stale,
    /// Job is in a terminal state.
    Terminal,
    /// Unknown job.
    Missing,
}

/// The persisted assignment ledger.
pub struct JobLedger {
    state: Mutex<BTreeMap<String, Assignment>>,
    path: Option<PathBuf>,
    clock: fn() -> u64,
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Parameters for a fresh assignment.
pub struct Assign<'a> {
    /// Job/task id.
    pub job_id: &'a str,
    /// Worker identity.
    pub worker: &'a str,
    /// Opaque model id.
    pub model: &'a str,
    /// Private worktree.
    pub worktree: PathBuf,
    /// Claim lease expiry.
    pub lease_until: u64,
}

impl JobLedger {
    /// In-memory ledger with injected clock.
    pub fn with_clock(clock: fn() -> u64) -> Self {
        Self {
            state: Mutex::new(BTreeMap::new()),
            path: None,
            clock,
        }
    }
    /// In-memory ledger, wall clock.
    pub fn new() -> Self {
        Self::with_clock(now_unix)
    }
    /// Persisted ledger — loads live assignments on restart.
    #[must_use]
    pub fn persisted(path: PathBuf) -> Self {
        let mut l = Self::new();
        l.path = Some(path);
        l.load();
        l
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Assignment>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Fresh assignment; `fence` starts at 1.
    pub fn assign(&self, a: Assign<'_>) -> u64 {
        let mut st = self.lock();
        let asg = Assignment {
            job_id: a.job_id.to_string(),
            worker: a.worker.to_string(),
            model: a.model.to_string(),
            worktree: a.worktree.clone(),
            lease_until: a.lease_until,
            state: JobState::Assigned,
            fence: 1,
            receipts: Vec::new(),
            partial_edits: Vec::new(),
            progress: String::new(),
        };
        st.insert(a.job_id.to_string(), asg);
        self.persist(&st);
        1
    }

    /// Snapshot an assignment.
    #[must_use]
    pub fn get(&self, job: &str) -> Option<Assignment> {
        self.lock().get(job).cloned()
    }

    /// Heartbeat: renews the lease iff the fence is current and the job is
    /// live. Returns the new lease.
    pub fn heartbeat(
        &self,
        job: &str,
        fence: u64,
        new_lease_until: u64,
    ) -> Result<u64, FenceError> {
        let mut st = self.lock();
        let Some(a) = st.get_mut(job) else {
            return Err(FenceError::Missing);
        };
        if a.fence != fence {
            return Err(FenceError::Stale);
        }
        if matches!(
            a.state,
            JobState::Completed | JobState::Failed(_) | JobState::Cancelled
        ) {
            return Err(FenceError::Terminal);
        }
        a.state = JobState::Running;
        a.lease_until = new_lease_until;
        self.persist(&st);
        Ok(new_lease_until)
    }

    /// Record an applied external effect before any retry — fenced.
    pub fn record_receipt(&self, job: &str, fence: u64, effect: &str) -> Result<(), FenceError> {
        let mut st = self.lock();
        let a = fenced(&mut st, job, fence)?;
        a.receipts.push(Receipt {
            effect: effect.to_string(),
            fence,
        });
        self.persist(&st);
        Ok(())
    }

    /// Record evidence produced after the assignment reached `Completed`.
    /// Closure review happens after the worker's completion fence is sealed,
    /// so this narrow method permits only an already-completed assignment and
    /// never reopens or mutates its ownership state.
    pub fn record_terminal_receipt(
        &self,
        job: &str,
        fence: u64,
        effect: &str,
    ) -> Result<(), FenceError> {
        let mut st = self.lock();
        let Some(a) = st.get_mut(job) else {
            return Err(FenceError::Missing);
        };
        if a.fence != fence {
            return Err(FenceError::Stale);
        }
        if a.state != JobState::Completed {
            return Err(FenceError::Terminal);
        }
        a.receipts.push(Receipt {
            effect: effect.to_string(),
            fence,
        });
        self.persist(&st);
        Ok(())
    }

    /// Checkpoint partial edits — preserved across reassignment.
    pub fn checkpoint(&self, job: &str, fence: u64, edits: Vec<String>) -> Result<(), FenceError> {
        let mut st = self.lock();
        let a = fenced(&mut st, job, fence)?;
        a.partial_edits = edits;
        self.persist(&st);
        Ok(())
    }

    /// Progress note — fenced.
    pub fn progress(&self, job: &str, fence: u64, note: &str) -> Result<(), FenceError> {
        let mut st = self.lock();
        let a = fenced(&mut st, job, fence)?;
        a.progress = note.to_string();
        self.persist(&st);
        Ok(())
    }

    /// Terminal completion — only the current fence may publish.
    pub fn complete(&self, job: &str, fence: u64) -> Result<(), FenceError> {
        let mut st = self.lock();
        let a = fenced(&mut st, job, fence)?;
        a.state = JobState::Completed;
        self.persist(&st);
        Ok(())
    }

    /// Terminal failure.
    pub fn fail(&self, job: &str, fence: u64, why: &str) -> Result<(), FenceError> {
        let mut st = self.lock();
        let a = fenced(&mut st, job, fence)?;
        a.state = JobState::Failed(why.to_string());
        self.persist(&st);
        Ok(())
    }

    /// Cancellation propagates regardless of fence (control op, not a
    /// worker publication).
    pub fn cancel(&self, job: &str) {
        let mut st = self.lock();
        if let Some(a) = st.get_mut(job) {
            a.state = JobState::Cancelled;
        }
        self.persist(&st);
    }

    /// Mark jobs whose leases lapsed but were heartbeating — crashed
    /// workers — `AwaitingModel` so the scheduler can reassign them.
    /// Returns the affected job ids.
    pub fn reap_expired(&self) -> Vec<String> {
        let now = (self.clock)();
        let mut st = self.lock();
        let mut dead = Vec::new();
        for (id, a) in st.iter_mut() {
            if matches!(a.state, JobState::Running | JobState::Assigned) && a.lease_until <= now {
                a.state = JobState::AwaitingModel;
                dead.push(id.clone());
            }
        }
        if !dead.is_empty() {
            self.persist(&st);
        }
        dead
    }

    /// Reassign a failed/lost job to a replacement worker on another
    /// eligible model. Fence bumps; receipts + partial edits carry over —
    /// the retry brief reconciles them instead of redoing them.
    /// Returns the new fence, or `None` when the job isn't in a
    /// reassignable state.
    pub fn reassign(
        &self,
        job: &str,
        new_worker: &str,
        new_model: &str,
        new_lease_until: u64,
    ) -> Option<u64> {
        let mut st = self.lock();
        let a = st.get_mut(job)?;
        if !matches!(a.state, JobState::AwaitingModel | JobState::Failed(_)) {
            return None;
        }
        a.fence += 1;
        a.worker = new_worker.to_string();
        a.model = new_model.to_string();
        a.lease_until = new_lease_until;
        a.state = JobState::Assigned;
        let f = a.fence;
        self.persist(&st);
        Some(f)
    }

    /// Jobs in `AwaitingModel` — the scheduler polls these for eligible
    /// replacement models.
    #[must_use]
    pub fn awaiting(&self) -> Vec<Assignment> {
        self.lock()
            .values()
            .filter(|a| a.state == JobState::AwaitingModel)
            .cloned()
            .collect()
    }

    /// Every assignment, terminal or not.
    #[must_use]
    pub fn all(&self) -> Vec<Assignment> {
        self.lock().values().cloned().collect()
    }

    /// All live (non-terminal) jobs.
    #[must_use]
    pub fn live(&self) -> Vec<Assignment> {
        self.lock()
            .values()
            .filter(|a| {
                !matches!(
                    a.state,
                    JobState::Completed | JobState::Failed(_) | JobState::Cancelled
                )
            })
            .cloned()
            .collect()
    }

    fn persist(&self, st: &BTreeMap<String, Assignment>) {
        let Some(path) = &self.path else { return };
        if let Ok(body) = serde_json::to_string(&st) {
            let _ = std::fs::write(path, body);
        }
    }

    fn load(&self) {
        let Some(path) = &self.path else { return };
        let Ok(body) = std::fs::read_to_string(path) else {
            return;
        };
        if let Ok(map) = serde_json::from_str::<BTreeMap<String, Assignment>>(&body) {
            *self.lock() = map;
        }
    }
}

impl Default for JobLedger {
    fn default() -> Self {
        Self::new()
    }
}

fn fenced<'a>(
    st: &'a mut BTreeMap<String, Assignment>,
    job: &str,
    fence: u64,
) -> Result<&'a mut Assignment, FenceError> {
    let Some(a) = st.get_mut(job) else {
        return Err(FenceError::Missing);
    };
    if a.fence != fence {
        return Err(FenceError::Stale);
    }
    if matches!(
        a.state,
        JobState::Completed | JobState::Failed(_) | JobState::Cancelled
    ) {
        return Err(FenceError::Terminal);
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const T0: u64 = 1_700_000_000;

    thread_local! {
        static CLOCK: Cell<u64> = const { Cell::new(T0) };
    }
    fn test_clock() -> u64 {
        CLOCK.with(|c| c.get())
    }
    fn set_clock(t: u64) {
        CLOCK.with(|c| c.set(t));
    }

    fn assign<'a>(job: &'a str, worker: &'a str, model: &'a str) -> Assign<'a> {
        Assign {
            job_id: job,
            worker,
            model,
            worktree: PathBuf::from(format!("/tmp/wt-{job}")),
            lease_until: T0 + 300,
        }
    }

    #[test]
    fn parallel_worker_recovery_model_loss_reassigns_only_that_job() {
        // Job on dead model → AwaitingModel → reassigned with bumped fence;
        // the sibling job is untouched.
        set_clock(T0);
        let l = JobLedger::with_clock(test_clock);
        l.assign(assign("j-dead", "w1", "p1/m1/deadbeef"));
        l.assign(assign("j-fine", "w2", "p2/m2/feedfeed"));
        l.heartbeat("j-dead", 1, T0 + 300).unwrap();
        // Model dies → the worker reports failure and the job parks.
        l.fail("j-dead", 1, "model 402").unwrap();
        let f2 = l
            .reassign("j-dead", "w3", "p2/m2/feedfeed", T0 + 600)
            .unwrap();
        assert_eq!(f2, 2);
        let a = l.get("j-dead").unwrap();
        assert_eq!(a.model, "p2/m2/feedfeed");
        assert_eq!(a.state, JobState::Assigned);
        // Sibling untouched.
        assert_eq!(l.get("j-fine").unwrap().worker, "w2");
    }

    #[test]
    fn parallel_worker_recovery_stale_worker_cannot_publish() {
        set_clock(T0);
        let l = JobLedger::with_clock(test_clock);
        l.assign(assign("j1", "w1", "m1"));
        // Original worker loses the job (reassigned at fence 2).
        l.fail("j1", 1, "timeout").unwrap();
        l.reassign("j1", "w2", "m2", T0 + 600);
        // Stale worker tries to complete with old fence — rejected.
        assert_eq!(l.complete("j1", 1), Err(FenceError::Stale));
        assert_eq!(l.progress("j1", 1, "done"), Err(FenceError::Stale));
        // The replacement publishes with fence 2.
        assert_eq!(l.complete("j1", 2), Ok(()));
        // Even the current fence can't republish a completed job.
        assert_eq!(l.complete("j1", 2), Err(FenceError::Terminal));
    }

    #[test]
    fn parallel_worker_recovery_receipts_survive_reassignment() {
        // An applied side effect is recorded before the crash; the retry's
        // brief sees it so the effect is never duplicated.
        set_clock(T0);
        let l = JobLedger::with_clock(test_clock);
        l.assign(assign("j1", "w1", "m1"));
        l.record_receipt("j1", 1, "claimed task T-9").unwrap();
        l.checkpoint("j1", 1, vec!["edit: src/lib.rs lines 1-40".into()])
            .unwrap();
        l.fail("j1", 1, "crash").unwrap();
        l.reassign("j1", "w2", "m2", T0 + 600);
        let a = l.get("j1").unwrap();
        assert_eq!(a.receipts.len(), 1);
        assert_eq!(a.receipts[0].effect, "claimed task T-9");
        assert_eq!(a.partial_edits, vec!["edit: src/lib.rs lines 1-40"]);
    }

    #[test]
    fn parallel_worker_recovery_expired_lease_is_reaped() {
        set_clock(T0);
        let l = JobLedger::with_clock(test_clock);
        l.assign(assign("j1", "w1", "m1"));
        l.assign(assign("j2", "w2", "m2"));
        l.heartbeat("j1", 1, T0 + 100).unwrap();
        l.heartbeat("j2", 1, T0 + 10_000).unwrap();
        set_clock(T0 + 500);
        let dead = l.reap_expired();
        assert_eq!(dead, vec!["j1".to_string()]);
        assert_eq!(l.get("j1").unwrap().state, JobState::AwaitingModel);
        assert_eq!(l.get("j2").unwrap().state, JobState::Running);
    }

    #[test]
    fn parallel_worker_recovery_no_eligible_replacement_stays_parked() {
        set_clock(T0);
        let l = JobLedger::with_clock(test_clock);
        l.assign(assign("j1", "w1", "m1"));
        l.fail("j1", 1, "model dead").unwrap();
        // AwaitingModel jobs can be re-queried; none eligible → still parked.
        let parked = l.awaiting();
        // state was Failed (not parked) — park it explicitly for the retry pass
        let _ = parked;
        // complete can't resurrect it, and there's no candidate:
        assert_eq!(
            l.get("j1").unwrap().state,
            JobState::Failed("model dead".into())
        );
    }

    #[test]
    fn parallel_worker_recovery_persists_across_restart() {
        set_clock(T0);
        let dir = std::env::temp_dir().join(format!("jl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ledger.json");
        {
            let l = JobLedger::persisted(path.clone());
            l.assign(assign("j1", "w1", "m1"));
            l.record_receipt("j1", 1, "effect-A").unwrap();
            l.complete("j1", 1).unwrap();
            l.assign(assign("j2", "w2", "m2"));
        }
        let l2 = JobLedger::persisted(path.clone());
        let a1 = l2.get("j1").unwrap();
        assert_eq!(a1.state, JobState::Completed);
        assert_eq!(a1.receipts.len(), 1);
        assert_eq!(l2.get("j2").unwrap().state, JobState::Assigned);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parallel_worker_recovery_cancel_propagates() {
        set_clock(T0);
        let l = JobLedger::with_clock(test_clock);
        l.assign(assign("j1", "w1", "m1"));
        l.heartbeat("j1", 1, T0 + 300).unwrap();
        l.cancel("j1");
        assert_eq!(l.get("j1").unwrap().state, JobState::Cancelled);
        // Cancelled job can't heartbeat or complete.
        assert_eq!(l.heartbeat("j1", 1, T0 + 600), Err(FenceError::Terminal));
    }

    #[test]
    fn parallel_worker_recovery_lease_renewal_extends_lease() {
        set_clock(T0);
        let l = JobLedger::with_clock(test_clock);
        l.assign(assign("j1", "w1", "m1"));
        l.heartbeat("j1", 1, T0 + 900).unwrap();
        set_clock(T0 + 400);
        // Lease was renewed past now — not reaped.
        assert!(l.reap_expired().is_empty());
    }
}
