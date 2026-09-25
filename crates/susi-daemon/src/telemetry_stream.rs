//! Dynamic Telemetry Streams (Swarm OS Bullet 94)
//!
//! Allows external operators to attach to any running cell's output stream
//! dynamically via a CLI pipe or web interface.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// Represents a chunk of telemetry data from a cell.
#[derive(Debug, Clone)]
pub struct TelemetryChunk {
    pub cell_id: String,
    pub stream_type: StreamType,
    pub payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamType {
    Stdout,
    Stderr,
    SyscallTrace,
}

/// A handle for a connected operator listening to a stream.
pub type OperatorId = String;

/// Manages dynamic telemetry attachments.
pub struct TelemetryManager {
    /// Maps a cell ID to a set of connected operator IDs watching its telemetry.
    attachments: RwLock<HashMap<String, HashSet<OperatorId>>>,

    /// Abstracted output buffer for operators (maps OperatorId to their received chunks)
    operator_buffers: RwLock<HashMap<OperatorId, Vec<TelemetryChunk>>>,
}

impl Default for TelemetryManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryManager {
    pub fn new() -> Self {
        Self {
            attachments: RwLock::new(HashMap::new()),
            operator_buffers: RwLock::new(HashMap::new()),
        }
    }

    /// Attaches an operator to a specific cell's telemetry stream.
    pub fn attach(&self, operator_id: &str, cell_id: &str) {
        let mut map = self.attachments.write().unwrap_or_else(|e| e.into_inner());
        map.entry(cell_id.to_string())
            .or_default()
            .insert(operator_id.to_string());

        // Ensure operator buffer exists
        let mut buffs = self
            .operator_buffers
            .write()
            .unwrap_or_else(|e| e.into_inner());
        buffs.entry(operator_id.to_string()).or_default();
    }

    /// Detaches an operator from a specific cell's telemetry stream.
    pub fn detach(&self, operator_id: &str, cell_id: &str) {
        let mut map = self.attachments.write().unwrap_or_else(|e| e.into_inner());
        if let Some(ops) = map.get_mut(cell_id) {
            ops.remove(operator_id);
        }
    }

    /// Dispatches a telemetry chunk to all attached operators.
    pub fn emit(&self, chunk: TelemetryChunk) {
        let map = self.attachments.read().unwrap_or_else(|e| e.into_inner());

        if let Some(operators) = map.get(&chunk.cell_id) {
            let mut buffs = self
                .operator_buffers
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for op_id in operators {
                if let Some(buffer) = buffs.get_mut(op_id) {
                    buffer.push(chunk.clone());
                }
            }
        }
    }

    /// Flushes and retrieves the current buffer for a specific operator.
    pub fn flush_buffer(&self, operator_id: &str) -> Vec<TelemetryChunk> {
        let mut buffs = self
            .operator_buffers
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(buffer) = buffs.get_mut(operator_id) {
            let chunks = buffer.clone();
            buffer.clear();
            chunks
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_attachment() {
        let manager = TelemetryManager::new();

        // Operator attaches to cell-x
        manager.attach("cli-user-1", "cell-x");

        // Emit events
        manager.emit(TelemetryChunk {
            cell_id: "cell-x".to_string(),
            stream_type: StreamType::Stdout,
            payload: "Generated 50 tokens".to_string(),
        });

        manager.emit(TelemetryChunk {
            cell_id: "cell-y".to_string(), // Operator not attached here
            stream_type: StreamType::Stderr,
            payload: "Error".to_string(),
        });

        // Flush buffer for operator
        let chunks = manager.flush_buffer("cli-user-1");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].payload, "Generated 50 tokens");
        assert_eq!(chunks[0].stream_type, StreamType::Stdout);

        // Second flush should be empty
        assert_eq!(manager.flush_buffer("cli-user-1").len(), 0);

        // Detach
        manager.detach("cli-user-1", "cell-x");
        manager.emit(TelemetryChunk {
            cell_id: "cell-x".to_string(),
            stream_type: StreamType::Stdout,
            payload: "Generated 100 tokens".to_string(),
        });
        assert_eq!(manager.flush_buffer("cli-user-1").len(), 0);
    }
}
