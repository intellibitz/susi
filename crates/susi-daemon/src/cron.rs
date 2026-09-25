//! Swarm Cron Daemon (Swarm OS Bullet 66)
//!
//! A scheduling daemon for scheduled cell awakenings and periodic tasks.

use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Debug, Clone)]
pub struct CronJob {
    pub job_id: String,
    pub cell_id: String,
    pub schedule: String, // e.g. "0 * * * *"
}

pub struct CronDaemon {
    jobs: RwLock<HashMap<String, CronJob>>,
}

impl Default for CronDaemon {
    fn default() -> Self {
        Self::new()
    }
}

impl CronDaemon {
    pub fn new() -> Self {
        Self {
            jobs: RwLock::new(HashMap::new()),
        }
    }

    pub fn schedule_job(&self, job: CronJob) {
        let mut map = self.jobs.write().unwrap_or_else(|e| e.into_inner());
        map.insert(job.job_id.clone(), job);
    }
}
