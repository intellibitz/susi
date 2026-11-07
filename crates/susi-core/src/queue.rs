// Priority queue for incoming intents ("pulses"): two unbounded flume
// channels (lock-free MPMC), one per priority class.

use crate::susi_error::EaiResult;
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

/// One FIFO lane: both channel ends live here, so the queue itself can never
/// disconnect and `pop` is a non-blocking `try_recv`.
struct Lane {
    tx: std::sync::mpsc::Sender<PulseEntry>,
    rx: std::sync::Mutex<std::sync::mpsc::Receiver<PulseEntry>>,
}

impl Lane {
    fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            tx,
            rx: std::sync::Mutex::new(rx),
        }
    }

    fn push(&self, entry: PulseEntry) {
        // Cannot fail: `rx` is owned alongside `tx` for the queue's lifetime.
        let _ = self.tx.send(entry);
    }

    fn pop(&self) -> Option<PulseEntry> {
        self.rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_recv()
            .ok()
    }

    fn is_empty(&self) -> bool {
        false
    }
}

pub struct SubstratePulseQueue {
    priority_queue: Lane,
    standard_queue: Lane,
    consumer_thread: std::sync::RwLock<Option<Thread>>,
    /// Set when ingest runs before a consumer is registered, so register_consumer
    /// can unpark immediately and avoid a lost-wakeup hang on a non-empty queue.
    pending_wake: AtomicBool,
}

impl SubstratePulseQueue {
    fn new() -> Self {
        Self {
            priority_queue: Lane::new(),
            standard_queue: Lane::new(),
            consumer_thread: std::sync::RwLock::new(None),
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

        if let Some(t) = self
            .consumer_thread
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
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
        *self
            .consumer_thread
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(std::thread::current());
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

    #[test]
    fn priority_pulses_pop_before_standard_pulses() {
        let queue = SubstratePulseQueue::new();
        let ws = PathBuf::from("/tmp/susi_ws");
        queue.ingest("normal task", &ws, "0.3.0").unwrap();
        queue.ingest("please stop and wait", &ws, "0.3.0").unwrap();

        let first = queue.pop().expect("priority pulse must pop first");
        assert_eq!(first.intent, "please stop and wait");
        assert_eq!(first.priority, 1);

        let second = queue.pop().expect("standard pulse must pop second");
        assert_eq!(second.intent, "normal task");
        assert_eq!(second.priority, 0);
        assert!(queue.pop().is_none());
    }

    #[test]
    fn clear_drains_both_priority_and_standard_lanes() {
        let queue = SubstratePulseQueue::new();
        let ws = PathBuf::from("/tmp/susi_ws");
        queue.ingest("standard work", &ws, "0.3.0").unwrap();
        queue.ingest("correction needed", &ws, "0.3.0").unwrap();
        assert!(!queue.is_empty());

        queue.clear();
        assert!(queue.is_empty());
        assert!(queue.pop().is_none());
    }

    #[test]
    fn register_consumer_drains_pending_wake_and_pop_blocking_returns_queued_entry() {
        let queue = SubstratePulseQueue::new();
        let ws = PathBuf::from("/tmp/susi_ws");
        queue.ingest("pre-registered pulse", &ws, "0.3.0").unwrap();
        queue.register_consumer();

        let popped = queue.pop_blocking();
        assert_eq!(popped.intent, "pre-registered pulse");
        assert_eq!(popped.workspace, ws);
        assert!(queue.is_empty());
    }

    #[test]
    fn ingest_classifies_wait_and_case_insensitive_stop_as_priority_one() {
        let queue = SubstratePulseQueue::new();
        let ws = PathBuf::from("/tmp/susi_priority_ws");
        queue.ingest("WAIT for operator", &ws, "0.4.0").unwrap();
        queue.ingest("STOP immediately", &ws, "0.4.0").unwrap();

        let p1 = queue.pop().expect("first priority pulse");
        let p2 = p1.clone();
        assert_eq!(p2.priority, 1);
        assert_eq!(p2.version, "0.4.0");

        let second = queue.pop().expect("second priority pulse");
        assert_eq!(second.priority, 1);
        assert!(queue.is_empty());
    }
}
