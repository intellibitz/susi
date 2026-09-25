//! Dead-Letter Queue (Swarm OS Bullet 34)
//!
//! Captures undeliverable inter-cell messages for debugging and retry.

use std::sync::RwLock;

#[derive(Debug, Clone)]
pub struct UndeliveredMessage {
    pub from_cell: String,
    pub to_cell: String,
    pub payload: Vec<u8>,
    pub reason: String,
}

pub struct DeadLetterQueue {
    queue: RwLock<Vec<UndeliveredMessage>>,
}

impl Default for DeadLetterQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl DeadLetterQueue {
    pub fn new() -> Self {
        Self {
            queue: RwLock::new(Vec::new()),
        }
    }

    pub fn push(&self, msg: UndeliveredMessage) {
        let mut q = self.queue.write().unwrap_or_else(|e| e.into_inner());
        q.push(msg);
    }
}
