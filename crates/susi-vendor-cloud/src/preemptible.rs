//! Recover supported work from preemptible capacity (VC-201-057).
//!
//! Checkpointable jobs are parked at their real progress on eviction and
//! resume only under a current fencing token — a stale fencing token means
//! another owner has the capacity and the resume is refused. Jobs that
//! cannot checkpoint report `Interrupted` honestly; every preempted second
//! of duplicate compute lands in `CostLedger` so preemption is never free
//! in accounting.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: String,
    /// Only checkpointable jobs are eligible for preemption recovery.
    pub checkpointable: bool,
    /// 0.0–1.0 real progress at last checkpoint.
    pub progress: f64,
    /// Compute seconds already burned.
    pub compute_secs: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EvictOutcome {
    /// Parked at real progress — resumable under the same fencing token.
    Checkpointed { id: String, progress: f64 },
    /// Cannot be paused — reported, not silently dropped.
    Interrupted { id: String },
}

#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub id: String,
    pub progress: f64,
    /// Fencing token that owned the capacity at checkpoint time.
    pub fencing_token: u64,
}

/// What a preemptible deployment held when capacity was pulled.
pub struct PreemptibleSet {
    pub jobs: BTreeMap<String, Job>,
    /// Monotonic fencing token for the current capacity grant.
    pub fencing_token: u64,
    pub checkpoints: Vec<Checkpoint>,
    /// Cost accounting — duplicate compute is explicitly included.
    pub ledger: CostLedger,
}

#[derive(Debug, Default, Clone)]
pub struct CostLedger {
    /// Compute that produced kept work.
    pub productive_secs: u64,
    /// Compute thrown away by preemption — still billed.
    pub duplicate_secs: u64,
}

impl PreemptibleSet {
    #[must_use]
    pub fn new(fencing_token: u64) -> Self {
        Self {
            jobs: BTreeMap::new(),
            fencing_token,
            checkpoints: Vec::new(),
            ledger: CostLedger::default(),
        }
    }

    /// Capacity pulled: checkpoint eligible jobs at real progress,
    /// interrupt the rest, and move *all* in-flight compute into the
    /// duplicate bucket — none of it produced a completed result.
    pub fn evict(&mut self) -> Vec<EvictOutcome> {
        let mut outcomes = Vec::new();
        for job in std::mem::take(&mut self.jobs).into_values() {
            self.ledger.duplicate_secs += job.compute_secs;
            if job.checkpointable {
                self.checkpoints.push(Checkpoint {
                    id: job.id.clone(),
                    progress: job.progress,
                    fencing_token: self.fencing_token,
                });
                outcomes.push(EvictOutcome::Checkpointed {
                    id: job.id,
                    progress: job.progress,
                });
            } else {
                outcomes.push(EvictOutcome::Interrupted { id: job.id });
            }
        }
        outcomes
    }

    /// Hand parked work back after re-admission. `current_token` must be
    /// the fencing token the new capacity grant was issued under — a
    /// checkpoint taken under a *newer* token belongs to a later owner and
    /// resuming it would duplicate live work.
    pub fn resume(&mut self, current_token: u64, retry_budget: u32) -> Vec<Job> {
        let mut resumed = Vec::new();
        let cps = std::mem::take(&mut self.checkpoints);
        for (i, cp) in cps.into_iter().enumerate() {
            if i as u32 >= retry_budget {
                // Retry policy bound hit — stays parked for the next grant.
                self.checkpoints.push(cp);
                continue;
            }
            if cp.fencing_token > current_token {
                self.checkpoints.push(cp);
                continue;
            }
            resumed.push(Job {
                id: cp.id,
                checkpointable: true,
                progress: cp.progress,
                compute_secs: 0,
            });
        }
        for job in &resumed {
            self.jobs.insert(job.id.clone(), job.clone());
        }
        resumed
    }

    /// Total billed compute, including what preemption wasted.
    #[must_use]
    pub fn billed_secs(&self) -> u64 {
        self.ledger.productive_secs + self.ledger.duplicate_secs
    }
}
