//! A2A conformance suite for the server (minimal contract checks).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConformanceCheck {
    pub name: String,
    pub ok: bool,
}

/// Run local conformance assertions against advertised capabilities.
#[must_use]
pub fn a2a_conformance(
    has_agent_card: bool,
    has_jsonrpc: bool,
    has_sse: bool,
) -> Vec<ConformanceCheck> {
    vec![
        ConformanceCheck {
            name: "agent_card".into(),
            ok: has_agent_card,
        },
        ConformanceCheck {
            name: "jsonrpc".into(),
            ok: has_jsonrpc,
        },
        ConformanceCheck {
            name: "sse".into(),
            ok: has_sse,
        },
    ]
}

#[must_use]
pub fn all_ok(checks: &[ConformanceCheck]) -> bool {
    checks.iter().all(|c| c.ok)
}

#[cfg(test)]
mod a2a_conformance_tests {
    use super::*;

    #[test]
    fn a2a_conformance_requires_card_rpc_sse() {
        let ok = a2a_conformance(true, true, true);
        assert!(all_ok(&ok));
        assert!(!all_ok(&a2a_conformance(true, true, false)));
    }
}
