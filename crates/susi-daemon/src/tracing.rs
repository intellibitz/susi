//! Universal Execution Tracing (Swarm OS Bullet 47)
//!
//! A standard tracing format (similar to OpenTelemetry) for cells
//! to report their logical inference steps and reasoning graphs.

use serde::Serialize;
use std::sync::RwLock;

#[derive(Debug, Clone, Serialize)]
pub struct TraceSpan {
    pub trace_id: String,
    pub cell_id: String,
    pub operation: String,
    pub duration_ms: u64,
}

pub struct TracingCollector {
    spans: RwLock<Vec<TraceSpan>>,
}

impl Default for TracingCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl TracingCollector {
    pub fn new() -> Self {
        Self {
            spans: RwLock::new(Vec::new()),
        }
    }

    pub fn record_span(&self, span: TraceSpan) {
        let mut logs = self.spans.write().unwrap_or_else(|e| e.into_inner());
        logs.push(span);
    }
}
