//! ACP capabilities for susi-gemi
//!
//! Defines the capabilities that susi-gemi advertises to ACP clients during
//! the initialization phase.

use agent_client_protocol::schema::v1::AgentCapabilities;

/// susi-gemi's advertised capabilities
#[derive(Debug, Clone)]
pub struct GemiCapabilities {
    /// Basic agent capabilities
    pub agent: AgentCapabilities,
}

impl Default for GemiCapabilities {
    fn default() -> Self {
        Self {
            agent: AgentCapabilities::new().load_session(true),
        }
    }
}

impl GemiCapabilities {
    /// Create capabilities for a local-first, no-auth agent
    pub fn local_first() -> Self {
        Self {
            agent: AgentCapabilities::new().load_session(true),
        }
    }

    /// Get agent capabilities for initialization response
    pub fn as_agent_capabilities(&self) -> AgentCapabilities {
        self.agent.clone()
    }
}
