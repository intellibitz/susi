// SUSI Substrate Pulse Queue
// Mandate 30: Non-Blocking Pulse Ingestion & Mandate 31: Serialized Pulse Execution
// Refactored to use genuinely lock-free crossbeam SegQueue (Aspiration 27 & Mandate 39)

use crate::error::EaiResult;
use crossbeam::queue::SegQueue;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
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
}

impl SubstratePulseQueue {
    fn new() -> Self {
        Self {
            priority_queue: SegQueue::new(),
            standard_queue: SegQueue::new(),
        }
    }

    pub fn global() -> &'static Self {
        static INSTANCE: OnceLock<SubstratePulseQueue> = OnceLock::new();
        INSTANCE.get_or_init(Self::new)
    }

    /// Truly Lock-Free Non-Blocking Ingestion (Aspiration 27 & 31)
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

        Ok(())
    }

    pub fn pop(&self) -> Option<PulseEntry> {
        if let Some(entry) = self.priority_queue.pop() {
            Some(entry)
        } else {
            self.standard_queue.pop()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.priority_queue.is_empty() && self.standard_queue.is_empty()
    }

    pub fn clear(&self) {
        while self.priority_queue.pop().is_some() {}
        while self.standard_queue.pop().is_some() {}
    }
}
