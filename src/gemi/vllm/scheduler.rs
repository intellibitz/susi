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
        if let Some(seq) = running.iter_mut().find(|s| s.id == seq_id) {
            seq.logical_token_ids.push(token_id);
            // If we crossed a block boundary, allocate a new physical block
            if seq.logical_token_ids.len() > seq.physical_blocks.len() * BLOCK_SIZE {
                if let Some(new_block) = self.allocator.allocate() {
                    seq.physical_blocks.push(new_block);
                } else {
                    seq.state = SequenceState::Swapped;
                    // In a real implementation we would move it to self.swapped queue
                    // and trigger CPU offload for the KV cache tensor memory mapping.
                }
            }
        }
    }
}
