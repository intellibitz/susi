//! Drain local AI workloads for suspend and maintenance (VC-201-049).
//!
//! `drain` stops accepting new work, checkpoints resumable jobs at their
//! real progress, and reports interrupted one-shot requests with their
//! actual outcome — never a fabricated success. GPU/runtime readiness is
//! discarded on suspend; `resume` rebuilds it only from a fresh probe so
//! a driver reset can never resurrect a stale "ready" flag.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    /// Progress is checkpointed; the job can continue after resume.
    Resumable,
    /// Cannot be paused; interrupted by drain.
    OneShot,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: String,
    pub kind: JobKind,
    /// 0.0–1.0 real progress at last update.
    pub progress: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DrainOutcome {
    /// Resumable job parked at its real progress.
    Checkpointed { id: String, progress: f64 },
    /// One-shot request interrupted mid-flight — its honest outcome.
    Interrupted { id: String },
}

#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub id: String,
    pub progress: f64,
}

pub struct DrainManager {
    accepting: bool,
    jobs: BTreeMap<String, Job>,
    /// Runtime ids believed ready; discarded wholesale on drain.
    ready_runtimes: BTreeSet<String>,
    pub checkpoints: Vec<Checkpoint>,
}

impl DrainManager {
    pub fn new() -> Self {
        Self {
            accepting: true,
            jobs: BTreeMap::new(),
            ready_runtimes: BTreeSet::new(),
            checkpoints: Vec::new(),
        }
    }

    pub fn mark_ready(&mut self, runtime: &str) {
        self.ready_runtimes.insert(runtime.to_string());
    }

    #[must_use]
    pub fn is_ready(&self, runtime: &str) -> bool {
        self.ready_runtimes.contains(runtime)
    }

    pub fn submit(&mut self, job: Job) -> Result<(), String> {
        if !self.accepting {
            return Err(format!("draining: refusing new job {}", job.id));
        }
        self.jobs.insert(job.id.clone(), job);
        Ok(())
    }

    /// Quiesce: refuse new work, checkpoint resumable jobs, interrupt
    /// one-shots, and drop all readiness (it may not survive suspend).
    pub fn drain(&mut self) -> Vec<DrainOutcome> {
        self.accepting = false;
        self.ready_runtimes.clear(); // stale GPU readiness is discarded
        let mut outcomes = Vec::new();
        for job in std::mem::take(&mut self.jobs).into_values() {
            match job.kind {
                JobKind::Resumable => {
                    self.checkpoints.push(Checkpoint {
                        id: job.id.clone(),
                        progress: job.progress,
                    });
                    outcomes.push(DrainOutcome::Checkpointed {
                        id: job.id,
                        progress: job.progress,
                    });
                }
                JobKind::OneShot => outcomes.push(DrainOutcome::Interrupted { id: job.id }),
            }
        }
        outcomes
    }

    /// After resume/driver-reset: readiness comes only from the probe.
    /// Runtimes the probe does not return stay unready; resumable
    /// checkpoints are handed back for rescheduling.
    pub fn resume(&mut self, probe: &dyn Fn() -> Vec<String>) -> Vec<Checkpoint> {
        self.ready_runtimes = probe().into_iter().collect();
        self.accepting = true;
        std::mem::take(&mut self.checkpoints)
    }

    /// A checkpointed job picked back up becomes a live job at its
    /// recorded progress — no silent restart from zero.
    pub fn resume_job(&mut self, cp: Checkpoint) {
        self.jobs.insert(
            cp.id.clone(),
            Job {
                id: cp.id,
                kind: JobKind::Resumable,
                progress: cp.progress,
            },
        );
    }

    #[must_use]
    pub fn accepting(&self) -> bool {
        self.accepting
    }

    #[must_use]
    pub fn job(&self, id: &str) -> Option<&Job> {
        self.jobs.get(id)
    }
}

impl Default for DrainManager {
    fn default() -> Self {
        Self::new()
    }
}
