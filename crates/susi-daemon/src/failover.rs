//! High-Availability Host Failover (Swarm OS Bullet 99)
//!
//! Protocol for migrating leadership and responsibilities when a daemon crashes.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostState {
    Leader,
    Follower,
}

use std::sync::RwLock;

pub struct FailoverManager {
    state: RwLock<HostState>,
}

impl Default for FailoverManager {
    fn default() -> Self {
        Self::new()
    }
}

impl FailoverManager {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(HostState::Follower),
        }
    }

    pub fn assume_leadership(&self) {
        let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
        *st = HostState::Leader;
    }

    pub fn is_leader(&self) -> bool {
        *self.state.read().unwrap_or_else(|e| e.into_inner()) == HostState::Leader
    }
}
