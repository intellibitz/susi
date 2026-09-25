//! Zero-Copy Ring Buffer for Message Passing (Swarm OS Bullet 13)
//!
//! Provides a lock-free or lightweight bounded circular buffer
//! intended for extremely fast, zero-copy IPC between cells on the same host.

use std::sync::atomic::{AtomicUsize, Ordering};

/// A fixed-capacity ring buffer for message passing between sandboxed cells.
pub struct RingBuffer<T> {
    buffer: Vec<Option<T>>,
    capacity: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
}

impl<T: Clone> RingBuffer<T> {
    /// Creates a new ring buffer with the specified capacity.
    pub fn new(capacity: usize) -> Self {
        let mut buffer = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            buffer.push(None);
        }
        
        Self {
            buffer,
            capacity,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Attempts to push an item onto the buffer.
    /// Returns `Err(item)` if the buffer is full.
    pub fn push(&mut self, item: T) -> Result<(), T> {
        let current_tail = self.tail.load(Ordering::Relaxed);
        let next_tail = (current_tail + 1) % self.capacity;
        
        if next_tail == self.head.load(Ordering::Acquire) {
            // Buffer is full
            return Err(item);
        }
        
        self.buffer[current_tail] = Some(item);
        self.tail.store(next_tail, Ordering::Release);
        
        Ok(())
    }

    /// Attempts to pop an item from the buffer.
    /// Returns `None` if the buffer is empty.
    pub fn pop(&mut self) -> Option<T> {
        let current_head = self.head.load(Ordering::Relaxed);
        
        if current_head == self.tail.load(Ordering::Acquire) {
            // Buffer is empty
            return None;
        }
        
        let item = self.buffer[current_head].take();
        self.head.store((current_head + 1) % self.capacity, Ordering::Release);
        
        item
    }

    /// Returns the number of items currently in the buffer.
    pub fn len(&self) -> usize {
        let h = self.head.load(Ordering::Relaxed);
        let t = self.tail.load(Ordering::Relaxed);
        
        if t >= h {
            t - h
        } else {
            self.capacity - h + t
        }
    }

    /// Returns `true` if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ring_buffer_operations() {
        let mut rb: RingBuffer<u32> = RingBuffer::new(3); // Capacity 3 means it holds 2 items (one slot for wrap logic)
        
        assert!(rb.is_empty());
        
        assert!(rb.push(10).is_ok());
        assert!(rb.push(20).is_ok());
        
        // 3rd push should fail because capacity is N-1
        assert!(rb.push(30).is_err());
        
        assert_eq!(rb.len(), 2);
        
        assert_eq!(rb.pop(), Some(10));
        assert_eq!(rb.pop(), Some(20));
        assert_eq!(rb.pop(), None);
        
        assert!(rb.is_empty());
    }
}
