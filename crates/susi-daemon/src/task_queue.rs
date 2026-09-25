//! Distributed Prioritized Task Queues (Swarm OS Bullet 64)
//!
//! A shared max-heap keyed by priority, so the highest-priority pending
//! task is always the next one popped regardless of arrival order.

use std::collections::BinaryHeap;
use std::sync::RwLock;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Task {
    pub priority: usize,
    pub id: String,
}

pub struct TaskQueue {
    queue: RwLock<BinaryHeap<Task>>,
}

impl Default for TaskQueue {
    fn default() -> Self {
        Self {
            queue: RwLock::new(BinaryHeap::new()),
        }
    }
}

impl TaskQueue {
    pub fn push(&self, task: Task) {
        self.queue
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(task);
    }

    pub fn pop(&self) -> Option<Task> {
        self.queue.write().unwrap_or_else(|e| e.into_inner()).pop()
    }

    pub fn len(&self) -> usize {
        self.queue.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pops_in_priority_order_regardless_of_push_order() {
        let queue = TaskQueue::default();
        queue.push(Task {
            priority: 1,
            id: "low".to_string(),
        });
        queue.push(Task {
            priority: 5,
            id: "high".to_string(),
        });
        queue.push(Task {
            priority: 3,
            id: "mid".to_string(),
        });

        assert_eq!(queue.pop().map(|t| t.id), Some("high".to_string()));
        assert_eq!(queue.pop().map(|t| t.id), Some("mid".to_string()));
        assert_eq!(queue.pop().map(|t| t.id), Some("low".to_string()));
        assert!(queue.is_empty());
    }

    #[test]
    fn empty_queue_pops_none() {
        let queue = TaskQueue::default();
        assert_eq!(queue.pop(), None);
    }
}
