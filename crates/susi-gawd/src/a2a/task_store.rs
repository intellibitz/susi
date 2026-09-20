//! A2A task store for susi-gawd
//!
//! Manages A2A task lifecycle and persistence.

use std::sync::Arc;

/// susi-gawd's A2A task store
/// For now, this is a simple placeholder that can be extended with actual persistence
#[derive(Clone)]
pub struct GawdTaskStore {
    // Placeholder for actual task store implementation
    // The ra2a crate provides InMemoryTaskStore but we'll implement our own later
    inner: Arc<()>,
}

impl GawdTaskStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(()),
        }
    }

    pub fn inner(&self) -> Arc<()> {
        self.inner.clone()
    }
}

impl Default for GawdTaskStore {
    fn default() -> Self {
        Self::new()
    }
}
