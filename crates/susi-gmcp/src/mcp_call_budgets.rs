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

    #[test]
    fn trusted_budget_matches_expected_timeout_and_byte_limits() {
        let b = budget_for(true);
        assert_eq!(b.timeout_ms, 30_000);
        assert_eq!(b.max_response_bytes, 16 * 1024 * 1024);
    }

    #[test]
    fn untrusted_budget_matches_expected_timeout_and_byte_limits() {
        let b = budget_for(false);
        assert_eq!(b.timeout_ms, 5_000);
        assert_eq!(b.max_response_bytes, 1024 * 1024);
    }

    #[test]
    fn within_budget_enforces_both_elapsed_and_byte_ceilings_inclusively() {
        let b = McpCallBudget {
            timeout_ms: 500,
            max_response_bytes: 2048,
        };
        assert!(within_budget(b, 500, 2048));
        assert!(!within_budget(b, 501, 2048));
        assert!(!within_budget(b, 500, 2049));
    }

    #[test]
    fn mcp_call_budget_round_trips_through_serde_json() {
        let budget = budget_for(true);
        let encoded = serde_json::to_string(&budget).expect("serialize McpCallBudget");
        let decoded: McpCallBudget =
            serde_json::from_str(&encoded).expect("deserialize McpCallBudget");
        assert_eq!(decoded, budget);
        assert!(within_budget(decoded, 0, 0));
    }
}
