//! Time and size budgets for MCP tool calls.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpCallBudget {
    pub timeout_ms: u64,
    pub max_response_bytes: u64,
}

/// Default budgets; tighten for untrusted servers.
#[must_use]
pub fn budget_for(trusted: bool) -> McpCallBudget {
    if trusted {
        McpCallBudget {
            timeout_ms: 30_000,
            max_response_bytes: 16 * 1024 * 1024,
        }
    } else {
        McpCallBudget {
            timeout_ms: 5_000,
            max_response_bytes: 1024 * 1024,
        }
    }
}

#[must_use]
pub fn within_budget(budget: McpCallBudget, elapsed_ms: u64, bytes: u64) -> bool {
    elapsed_ms <= budget.timeout_ms && bytes <= budget.max_response_bytes
}

#[cfg(test)]
mod mcp_call_budgets_tests {
    use super::*;

    #[test]
    fn mcp_call_budgets_tighten_for_untrusted() {
        let t = budget_for(true);
        let u = budget_for(false);
        assert!(t.timeout_ms > u.timeout_ms);
        assert!(within_budget(u, 100, 100));
        assert!(!within_budget(u, 10_000, 0));
    }
}
