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

    /// Every span recorded for `cell_id`, oldest first.
    pub fn spans_for_cell(&self, cell_id: &str) -> Vec<TraceSpan> {
        self.spans
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|s| s.cell_id == cell_id)
            .cloned()
            .collect()
    }

    /// Every span recorded, across all cells, oldest first.
    pub fn all_spans(&self) -> Vec<TraceSpan> {
        self.spans.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(cell_id: &str, operation: &str) -> TraceSpan {
        TraceSpan {
            trace_id: "t1".to_string(),
            cell_id: cell_id.to_string(),
            operation: operation.to_string(),
            duration_ms: 5,
        }
    }

    #[test]
    fn spans_for_cell_filters_to_that_cell_only() {
        let collector = TracingCollector::new();
        collector.record_span(span("cell-a", "infer"));
        collector.record_span(span("cell-b", "infer"));
        collector.record_span(span("cell-a", "tool_call"));

        let a_spans = collector.spans_for_cell("cell-a");
        assert_eq!(a_spans.len(), 2);
        assert!(a_spans.iter().all(|s| s.cell_id == "cell-a"));
    }

    #[test]
    fn all_spans_returns_everything_recorded() {
        let collector = TracingCollector::new();
        collector.record_span(span("cell-a", "infer"));
        collector.record_span(span("cell-b", "infer"));
        assert_eq!(collector.all_spans().len(), 2);
    }
}
