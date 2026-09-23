// Priority queue for incoming intents ("pulses"), backed by crossbeam's
// lock-free SegQueue.

use crate::susi_error::EaiResult;
use crossbeam::queue::SegQueue;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::thread::Thread;
use tracing::info;

#[derive(Debug, Clone)]
pub struct PulseEntry {
    pub intent: String,
    pub workspace: PathBuf,
    pub version: String,
    pub priority: u8, // 0: Normal, 1: Correction, 2: High
}

pub struct SubstratePulseQueue {
    priority_queue: SegQueue<PulseEntry>,
    standard_queue: SegQueue<PulseEntry>,
    consumer_thread: parking_lot::RwLock<Option<Thread>>,
    /// Set when ingest runs before a consumer is registered, so register_consumer
    /// can unpark immediately and avoid a lost-wakeup hang on a non-empty queue.
    pending_wake: AtomicBool,
}

impl SubstratePulseQueue {
    fn new() -> Self {
        Self {
            priority_queue: SegQueue::new(),
            standard_queue: SegQueue::new(),
            consumer_thread: parking_lot::RwLock::new(None),
            pending_wake: AtomicBool::new(false),
        }
    }

    pub fn global() -> &'static Self {
        static INSTANCE: OnceLock<SubstratePulseQueue> = OnceLock::new();
        INSTANCE.get_or_init(Self::new)
    }

    pub fn ingest(&self, intent: &str, workspace: &Path, version: &str) -> EaiResult<()> {
        info!(intent = %intent, "Ingesting new pulse into lock-free substrate queue");

        let priority = if intent.to_lowercase().contains("stop")
            || intent.to_lowercase().contains("wait")
            || intent.to_lowercase().contains("correction")
        {
            1
        } else {
            0
        };

        let entry = PulseEntry {
            intent: intent.to_string(),
            workspace: workspace.to_path_buf(),
            version: version.to_string(),
            priority,
        };

        if priority > 0 {
            self.priority_queue.push(entry);
        } else {
            self.standard_queue.push(entry);
        }

        if let Some(t) = self.consumer_thread.read().as_ref() {
            t.unpark();
        } else {
            self.pending_wake.store(true, Ordering::SeqCst);
        }

        Ok(())
    }

    /// Workspace a consumer must pass to swarm solve — always the ingest
    /// caller's path, never a daemon boot workspace substitute.
    pub fn execution_workspace(pulse: &PulseEntry) -> &Path {
        &pulse.workspace
    }

    pub fn pop(&self) -> Option<PulseEntry> {
        if let Some(entry) = self.priority_queue.pop() {
            Some(entry)
        } else {
            self.standard_queue.pop()
        }
    }

    /// Block until a pulse is available. Always re-checks the queue after wake
    /// (handles park tokens, spurious wakes, and ingest-before-register).
    pub fn pop_blocking(&self) -> PulseEntry {
        loop {
            if let Some(entry) = self.pop() {
                return entry;
            }
            std::thread::park();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.priority_queue.is_empty() && self.standard_queue.is_empty()
    }

    pub fn clear(&self) {
        while self.priority_queue.pop().is_some() {}
        while self.standard_queue.pop().is_some() {}
    }

    pub fn register_consumer(&self) {
        *self.consumer_thread.write() = Some(std::thread::current());
        // Ingest may have raced ahead of registration: drain the pending-wake
        // flag and also wake if work is already queued.
        let pending = self.pending_wake.swap(false, Ordering::SeqCst);
        if pending || !self.is_empty() {
            std::thread::current().unpark();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_preserves_ingesting_workspace_not_a_substitute() {
        let queue = SubstratePulseQueue::new();
        let folder_b = PathBuf::from("/tmp/susi_workspace_b");
        queue.ingest("refactor this", &folder_b, "0.3.0").unwrap();
        let pulse = queue.pop().expect("pulse must be queued");
        assert_eq!(pulse.workspace, folder_b);
        assert_eq!(
            SubstratePulseQueue::execution_workspace(&pulse),
            folder_b.as_path(),
            "daemon consumers must execute against the ingest cwd"
        );
        assert_eq!(pulse.version, "0.3.0");
    }
}
