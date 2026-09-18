use std::collections::VecDeque;
use parking_lot::RwLock;

/// Size of each token block inside the PagedAttention memory layout.
pub const BLOCK_SIZE: usize = 16; 

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PhysicalBlockId(pub usize);

#[derive(Debug)]
pub struct BlockAllocator {
    pub total_blocks: usize,
    pub free_blocks: RwLock<VecDeque<PhysicalBlockId>>,
}

impl BlockAllocator {
    pub fn new(vram_pool_gb: usize, num_layers: usize, head_dim: usize, num_kv_heads: usize) -> Self {
        // Compute how many blocks we can fit natively into the configured VRAM pool.
        // Each token requires 2 bytes (f16) * head_dim * num_kv_heads * 2 (K and V).
        let bytes_per_token = 2 * head_dim * num_kv_heads * 2;
        let bytes_per_layer_block = bytes_per_token * BLOCK_SIZE;
        let total_block_bytes = bytes_per_layer_block * num_layers;
        
        let pool_bytes = vram_pool_gb * 1024 * 1024 * 1024;
        let total_blocks = pool_bytes / total_block_bytes;
        
        // Retain 10% for engine weights, allocate 90% for KV cache
        let usable_blocks = (total_blocks as f64 * 0.90) as usize;
        
        let mut queue = VecDeque::with_capacity(usable_blocks);
        for i in 0..usable_blocks {
            queue.push_back(PhysicalBlockId(i));
        }

        Self {
            total_blocks: usable_blocks,
            free_blocks: RwLock::new(queue),
        }
    }

    pub fn allocate(&self) -> Option<PhysicalBlockId> {
        self.free_blocks.write().pop_front()
    }

    pub fn free(&self, block: PhysicalBlockId) {
        self.free_blocks.write().push_back(block);
    }
    
    pub fn get_available_blocks(&self) -> usize {
        self.free_blocks.read().len()
    }
}
