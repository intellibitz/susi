//! Cost Analyzer (Swarm OS Bullet 73)
//!
//! Breaks spend down by cell, model, and tool. `metrics_aggregator.rs`
//! (Bullet 97) tracks a single global inference/token counter; this
//! records the same kind of event per dimension so spend can be
//! attributed to who or what actually incurred it, not just totaled.

use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CostBreakdown {
    pub tokens: u64,
    pub cost_usd: f64,
}

pub struct CostAnalyzer {
    price_per_1k_tokens_usd: f64,
    by_cell: RwLock<HashMap<String, CostBreakdown>>,
    by_model: RwLock<HashMap<String, CostBreakdown>>,
    by_tool: RwLock<HashMap<String, CostBreakdown>>,
}

impl CostAnalyzer {
    pub fn new(price_per_1k_tokens_usd: f64) -> Self {
        Self {
            price_per_1k_tokens_usd,
            by_cell: RwLock::new(HashMap::new()),
            by_model: RwLock::new(HashMap::new()),
            by_tool: RwLock::new(HashMap::new()),
        }
    }

    fn accumulate(
        map: &RwLock<HashMap<String, CostBreakdown>>,
        key: &str,
        tokens: u64,
        cost_usd: f64,
    ) {
        let mut map = map.write().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(key.to_string()).or_default();
        entry.tokens += tokens;
        entry.cost_usd += cost_usd;
    }

    /// Records an inference's token spend against both the cell that ran
    /// it and the model that served it.
    pub fn record_inference(&self, cell_id: &str, model: &str, tokens: u64) {
        let cost_usd = (tokens as f64 / 1000.0) * self.price_per_1k_tokens_usd;
        Self::accumulate(&self.by_cell, cell_id, tokens, cost_usd);
        Self::accumulate(&self.by_model, model, tokens, cost_usd);
    }

    /// Records a tool call's token spend (e.g. a tool-augmented completion)
    /// against the tool that was invoked.
    pub fn record_tool_call(&self, tool: &str, tokens: u64) {
        let cost_usd = (tokens as f64 / 1000.0) * self.price_per_1k_tokens_usd;
        Self::accumulate(&self.by_tool, tool, tokens, cost_usd);
    }

    pub fn cell_breakdown(&self, cell_id: &str) -> CostBreakdown {
        self.by_cell
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(cell_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn model_breakdown(&self, model: &str) -> CostBreakdown {
        self.by_model
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(model)
            .cloned()
            .unwrap_or_default()
    }

    pub fn tool_breakdown(&self, tool: &str) -> CostBreakdown {
        self.by_tool
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(tool)
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inference_spend_attributes_to_both_cell_and_model() {
        let analyzer = CostAnalyzer::new(2.0); // $2 / 1k tokens
        analyzer.record_inference("cell-a", "gemi-7b", 1000);
        analyzer.record_inference("cell-a", "gemi-7b", 500);
        analyzer.record_inference("cell-b", "gemi-70b", 1000);

        let cell_a = analyzer.cell_breakdown("cell-a");
        assert_eq!(cell_a.tokens, 1500);
        assert!((cell_a.cost_usd - 3.0).abs() < 1e-9);

        let model_7b = analyzer.model_breakdown("gemi-7b");
        assert_eq!(model_7b.tokens, 1500);

        let cell_b = analyzer.cell_breakdown("cell-b");
        assert_eq!(cell_b.tokens, 1000);
        assert!((cell_b.cost_usd - 2.0).abs() < 1e-9);
    }

    #[test]
    fn tool_calls_are_tracked_separately_from_inference() {
        let analyzer = CostAnalyzer::new(1.0);
        analyzer.record_tool_call("web_search", 300);
        assert_eq!(analyzer.tool_breakdown("web_search").tokens, 300);
        assert_eq!(
            analyzer.cell_breakdown("web_search"),
            CostBreakdown::default()
        );
    }

    #[test]
    fn unknown_key_returns_zeroed_breakdown() {
        let analyzer = CostAnalyzer::new(1.0);
        assert_eq!(
            analyzer.cell_breakdown("never-seen"),
            CostBreakdown::default()
        );
    }
}
