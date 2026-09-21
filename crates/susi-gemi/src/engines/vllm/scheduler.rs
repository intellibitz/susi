use super::allocator::{BlockAllocator, PhysicalBlockId, BLOCK_SIZE};
use parking_lot::RwLock;
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SequenceId(pub u64);

#[derive(Debug)]
pub enum SequenceState {
    Waiting,
    Running,
    Swapped,
    Finished,
}

#[derive(Debug)]
pub struct Sequence {
    pub id: SequenceId,
    pub prompt: String,
    pub logical_token_ids: Vec<u32>,
    pub physical_blocks: Vec<PhysicalBlockId>,
    pub state: SequenceState,
}

pub struct ContinuousScheduler {
    pub allocator: BlockAllocator,
    pub waiting: RwLock<VecDeque<Sequence>>,
    pub running: RwLock<Vec<Sequence>>,
    pub swapped: RwLock<Vec<Sequence>>,
}

impl ContinuousScheduler {
    pub fn new(allocator: BlockAllocator) -> Self {
        Self {
            allocator,
            waiting: RwLock::new(VecDeque::new()),
            running: RwLock::new(Vec::new()),
            swapped: RwLock::new(Vec::new()),
        }
    }

    pub fn add_sequence(&self, id: u64, prompt: String, initial_tokens: Vec<u32>) {
        let seq = Sequence {
            id: SequenceId(id),
            prompt,
            logical_token_ids: initial_tokens,
            physical_blocks: Vec::new(),
            state: SequenceState::Waiting,
        };
        self.waiting.write().push_back(seq);
    }

    pub fn step(&self) -> Vec<SequenceId> {
        self.requeue_swapped();

        let mut running = self.running.write();
        let mut waiting = self.waiting.write();

        // Very basic continuous batching selection: Pull waiting sequences into running
        // until we run out of free KV blocks for their initial prompt sizes.
        while let Some(front) = waiting.front() {
            let required_blocks = front.logical_token_ids.len().div_ceil(BLOCK_SIZE);
            if self.allocator.get_available_blocks() >= required_blocks {
                let mut seq = waiting.pop_front().unwrap();
                for _ in 0..required_blocks {
                    if let Some(block) = self.allocator.allocate() {
                        seq.physical_blocks.push(block);
                    }
                }
                seq.state = SequenceState::Running;
                running.push(seq);
            } else {
                break; // VRAM Full, hold sequence in waiting queue
            }
        }

        running.iter().map(|s| s.id).collect()
    }

    pub fn append_token(&self, seq_id: SequenceId, token_id: u32) {
        let mut running = self.running.write();
        let Some(idx) = running.iter().position(|s| s.id == seq_id) else {
            return;
        };
        running[idx].logical_token_ids.push(token_id);
        // If we crossed a block boundary, allocate a new physical block
        if running[idx].logical_token_ids.len() > running[idx].physical_blocks.len() * BLOCK_SIZE {
            if let Some(new_block) = self.allocator.allocate() {
                running[idx].physical_blocks.push(new_block);
            } else {
                // VRAM exhausted for this new block: preempt via recompute.
                // Free every block this sequence already holds (real vLLM
                // recompute-based preemption releases the whole sequence's
                // cache rather than partially swapping it), move it into the
                // swapped queue, and let `step()` re-admit it from the front
                // of `waiting` once blocks free up — it will recompute its
                // KV cache from `logical_token_ids` on re-admission instead
                // of resuming a preserved cache, which is the correct
                // (if slower) fallback when there is no CPU-offload path.
                let mut seq = running.remove(idx);
                for block in seq.physical_blocks.drain(..) {
                    self.allocator.free(block);
                }
                seq.state = SequenceState::Swapped;
                self.swapped.write().push(seq);
            }
        }
    }

    /// Re-admits swapped-out sequences back into `waiting` once free blocks
    /// exist, so `step()` picks them up again. Must run before `step()`'s
    /// own admission pass in each scheduling cycle, otherwise a swapped
    /// sequence stays in `self.swapped` forever.
    pub fn requeue_swapped(&self) {
        let mut swapped = self.swapped.write();
        if swapped.is_empty() {
            return;
        }
        let mut waiting = self.waiting.write();
        for mut seq in swapped.drain(..) {
            seq.state = SequenceState::Waiting;
            waiting.push_back(seq);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic tiny allocator (bypasses the VRAM-derived sizing math
    /// in `BlockAllocator::new`, which needs real hardware-shaped inputs)
    /// with exactly `blocks` physical blocks to allocate.
    fn tiny_allocator(blocks: usize) -> BlockAllocator {
        let mut queue = VecDeque::with_capacity(blocks);
        for i in 0..blocks {
            queue.push_back(PhysicalBlockId(i));
        }
        BlockAllocator {
            total_blocks: blocks,
            free_blocks: RwLock::new(queue),
        }
    }

    #[test]
    fn test_step_admits_waiting_sequence_when_blocks_available() {
        let sched = ContinuousScheduler::new(tiny_allocator(1));
        sched.add_sequence(1, "p".to_string(), vec![0; 1]); // 1 token -> 1 block needed

        let running_ids = sched.step();

        assert_eq!(running_ids, vec![SequenceId(1)]);
        assert!(sched.waiting.read().is_empty());
        assert_eq!(sched.allocator.get_available_blocks(), 0);
    }

    #[test]
    fn test_step_holds_waiting_sequence_when_blocks_exhausted() {
        let sched = ContinuousScheduler::new(tiny_allocator(1));
        sched.add_sequence(1, "p".to_string(), vec![0; BLOCK_SIZE + 1]); // needs 2 blocks, only 1 exists

        let running_ids = sched.step();

        assert!(running_ids.is_empty());
        assert_eq!(sched.waiting.read().len(), 1);
    }

    #[test]
    fn test_append_token_allocates_new_block_on_boundary_cross() {
        let sched = ContinuousScheduler::new(tiny_allocator(2));
        sched.add_sequence(1, "p".to_string(), vec![0; 1]);
        sched.step(); // admits with 1 block, 1 free block remains

        for _ in 0..BLOCK_SIZE {
            sched.append_token(SequenceId(1), 0); // grows to BLOCK_SIZE + 1 tokens
        }

        let running = sched.running.read();
        let seq = running.iter().find(|s| s.id == SequenceId(1)).unwrap();
        assert_eq!(
            seq.physical_blocks.len(),
            2,
            "crossing the block boundary must allocate a second physical block"
        );
        assert_eq!(sched.allocator.get_available_blocks(), 0);
    }

    #[test]
    fn test_append_token_swaps_out_and_frees_blocks_when_allocator_exhausted() {
        let sched = ContinuousScheduler::new(tiny_allocator(1));
        sched.add_sequence(1, "p".to_string(), vec![0; 1]);
        sched.step(); // admits, consumes the only block; 0 free remain

        for _ in 0..BLOCK_SIZE {
            sched.append_token(SequenceId(1), 0); // crosses the boundary with no free block left
        }

        assert!(
            sched.running.read().is_empty(),
            "an unswappable sequence must not silently linger in `running` with state Swapped"
        );
        assert_eq!(sched.swapped.read().len(), 1);
        assert_eq!(
            sched.swapped.read()[0].physical_blocks.len(),
            0,
            "a preempted-by-recompute sequence must release all its blocks, not just fail to grow"
        );
        assert_eq!(
            sched.allocator.get_available_blocks(),
            1,
            "the freed block must actually return to the allocator's free list"
        );
    }

    #[test]
    fn test_requeue_swapped_moves_sequence_back_to_waiting() {
        let sched = ContinuousScheduler::new(tiny_allocator(1));
        sched.add_sequence(1, "p".to_string(), vec![0; 1]);
        sched.step();
        for _ in 0..BLOCK_SIZE {
            sched.append_token(SequenceId(1), 0);
        }
        assert_eq!(sched.swapped.read().len(), 1);

        sched.requeue_swapped();

        assert!(sched.swapped.read().is_empty());
        assert_eq!(sched.waiting.read().len(), 1);
        assert!(matches!(
            sched.waiting.read().front().unwrap().state,
            SequenceState::Waiting
        ));
    }

    #[test]
    fn test_step_calls_requeue_swapped_so_a_swapped_sequence_is_not_stuck_forever() {
        let sched = ContinuousScheduler::new(tiny_allocator(1));
        sched.add_sequence(1, "p".to_string(), vec![0; 1]);
        sched.step();
        for _ in 0..BLOCK_SIZE {
            sched.append_token(SequenceId(1), 0);
        }
        assert_eq!(sched.swapped.read().len(), 1);

        sched.step();

        assert!(
            sched.swapped.read().is_empty(),
            "step() must drain the swapped queue back into waiting every cycle"
        );
    }
}
