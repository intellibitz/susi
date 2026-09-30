//! Import external agent results into mission traces.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedTrace {
    pub agent: String,
    pub summary: String,
    pub ok: bool,
}

/// Normalize an external agent result into a mission-trace entry.
#[must_use]
pub fn import_trace(agent: &str, summary: &str, exit_ok: bool) -> ImportedTrace {
    ImportedTrace {
        agent: agent.into(),
        summary: summary.into(),
        ok: exit_ok,
    }
}

#[cfg(test)]
mod agent_trace_import_tests {
    use super::*;

    #[test]
    fn agent_trace_import_records_outcome() {
        let t = import_trace("cursor", "patched", true);
        assert!(t.ok);
        assert_eq!(t.agent, "cursor");
    }
}
