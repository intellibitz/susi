//! Offline queue (Swarm OS Bullet 50)
//!
//! While offline, submitted payloads stay queued. Transitioning online
//! drains them in order. Submitting while already online returns the
//! payload as applied and stores nothing. Local inference itself stays
//! in `susi-gemi`; this is the sync half of offline operation.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submit {
    Applied(Vec<u8>),
    Queued,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfflineQueue {
    online: bool,
    pending: Vec<Vec<u8>>,
}

impl Default for OfflineQueue {
    fn default() -> Self {
        Self::offline()
    }
}

impl OfflineQueue {
    pub fn offline() -> Self {
        Self {
            online: false,
            pending: Vec::new(),
        }
    }

    pub fn submit(&mut self, payload: Vec<u8>) -> Submit {
        if self.online {
            Submit::Applied(payload)
        } else {
            self.pending.push(payload);
            Submit::Queued
        }
    }

    /// Drains the queue when `online` becomes true. A transition that
    /// stays offline returns an empty vec and keeps the pending payloads.
    pub fn set_online(&mut self, online: bool) -> Vec<Vec<u8>> {
        self.online = online;
        if online {
            std::mem::take(&mut self.pending)
        } else {
            Vec::new()
        }
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_payloads_flush_in_order_when_connectivity_returns() {
        let mut queue = OfflineQueue::offline();
        assert_eq!(queue.submit(b"a".to_vec()), Submit::Queued);
        assert_eq!(queue.submit(b"b".to_vec()), Submit::Queued);
        assert!(queue.set_online(false).is_empty());
        assert_eq!(queue.pending_len(), 2);
        assert_eq!(queue.set_online(true), vec![b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(queue.submit(b"c".to_vec()), Submit::Applied(b"c".to_vec()));
        assert_eq!(queue.pending_len(), 0);
    }
}
