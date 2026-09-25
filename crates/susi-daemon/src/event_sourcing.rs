//! Event Sourcing for Agent State (Swarm OS Bullet 81)
//!
//! Cell state changes are appended as immutable events rather than
//! overwritten in place; `replay` folds the log through a caller-supplied
//! reducer to derive state at any point — the same pattern `commit_log`
//! uses to derive `ClusterState` from the replicated consensus ledger.

use std::sync::RwLock;

#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub seq: u64,
    pub ts: u64,
    pub payload: Vec<u8>,
}

pub struct EventStore {
    events: RwLock<Vec<StoredEvent>>,
}

impl Default for EventStore {
    fn default() -> Self {
        Self::new()
    }
}

impl EventStore {
    pub fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
        }
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Appends an event, returning its assigned sequence number.
    pub fn append_event(&self, payload: &[u8]) -> u64 {
        let mut events = self.events.write().unwrap_or_else(|e| e.into_inner());
        let seq = events.len() as u64;
        events.push(StoredEvent {
            seq,
            ts: Self::now(),
            payload: payload.to_vec(),
        });
        seq
    }

    /// Folds every event through `reducer`, starting from `init`, in
    /// append order — the canonical event-sourcing state derivation.
    pub fn replay<S>(&self, init: S, reducer: impl Fn(S, &StoredEvent) -> S) -> S {
        let events = self.events.read().unwrap_or_else(|e| e.into_inner());
        events.iter().fold(init, reducer)
    }

    pub fn len(&self) -> usize {
        self.events.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_get_monotonic_sequence_numbers() {
        let store = EventStore::new();
        assert_eq!(store.append_event(b"a"), 0);
        assert_eq!(store.append_event(b"b"), 1);
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn replay_folds_events_in_order() {
        let store = EventStore::new();
        store.append_event(b"deposit:10");
        store.append_event(b"deposit:5");
        store.append_event(b"withdraw:3");

        let balance = store.replay(0i64, |acc, event| {
            let text = String::from_utf8_lossy(&event.payload);
            if let Some(amount) = text.strip_prefix("deposit:") {
                acc + amount.parse::<i64>().unwrap_or(0)
            } else if let Some(amount) = text.strip_prefix("withdraw:") {
                acc - amount.parse::<i64>().unwrap_or(0)
            } else {
                acc
            }
        });
        assert_eq!(balance, 12);
    }
}
