//! Standard Pub-Sub Event Bus (Swarm OS Bullet 56)
//!
//! Provides a loosely coupled publisher-subscriber event bus. Cells can
//! broadcast world-state changes to topics without knowing their subscribers.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// Represents a broadcasted event on the bus.
#[derive(Debug, Clone)]
pub struct BusEvent {
    pub topic: String,
    pub publisher_id: String,
    pub payload: Vec<u8>,
}

/// A pub-sub event bus for inter-cell communication.
pub struct EventBus {
    /// Maps a topic name to a set of subscriber cell IDs.
    subscribers: RwLock<HashMap<String, HashSet<String>>>,

    /// Abstracted queue of events (In a real system, this feeds into the RingBuffer or channels)
    event_log: RwLock<Vec<BusEvent>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            subscribers: RwLock::new(HashMap::new()),
            event_log: RwLock::new(Vec::new()),
        }
    }

    /// Subscribes a cell to a specific topic.
    pub fn subscribe(&self, cell_id: &str, topic: &str) {
        let mut map = self.subscribers.write().unwrap_or_else(|e| e.into_inner());
        map.entry(topic.to_string())
            .or_default()
            .insert(cell_id.to_string());
    }

    /// Unsubscribes a cell from a specific topic.
    pub fn unsubscribe(&self, cell_id: &str, topic: &str) {
        let mut map = self.subscribers.write().unwrap_or_else(|e| e.into_inner());
        if let Some(subs) = map.get_mut(topic) {
            subs.remove(cell_id);
        }
    }

    /// Broadcasts an event to all subscribers of a topic.
    /// Returns the number of cells that were notified.
    pub fn publish(&self, publisher_id: &str, topic: &str, payload: Vec<u8>) -> usize {
        let map = self.subscribers.read().unwrap_or_else(|e| e.into_inner());

        let event = BusEvent {
            topic: topic.to_string(),
            publisher_id: publisher_id.to_string(),
            payload,
        };

        // Log the event globally
        let mut log = self.event_log.write().unwrap_or_else(|e| e.into_inner());
        log.push(event);

        // Return subscriber count (abstractly routing the message to them)
        if let Some(subs) = map.get(topic) {
            subs.len()
        } else {
            0
        }
    }

    /// Retrieves all events published so far (useful for debugging/testing).
    pub fn get_event_log(&self) -> Vec<BusEvent> {
        let log = self.event_log.read().unwrap_or_else(|e| e.into_inner());
        log.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pubsub_event_bus() {
        let bus = EventBus::new();

        bus.subscribe("cell-a", "world/state/weather");
        bus.subscribe("cell-b", "world/state/weather");
        bus.subscribe("cell-c", "system/alerts");

        // Publish to weather
        let notified = bus.publish("sensor-1", "world/state/weather", b"Rainy".to_vec());
        assert_eq!(notified, 2);

        // Publish to alerts
        let notified2 = bus.publish("monitor-1", "system/alerts", b"Intrusion".to_vec());
        assert_eq!(notified2, 1);

        // Publish to unknown
        let notified3 = bus.publish("unknown", "void", b"Null".to_vec());
        assert_eq!(notified3, 0);

        // Unsubscribe
        bus.unsubscribe("cell-a", "world/state/weather");
        let notified4 = bus.publish("sensor-1", "world/state/weather", b"Sunny".to_vec());
        assert_eq!(notified4, 1);

        let logs = bus.get_event_log();
        assert_eq!(logs.len(), 4);
    }
}
