//! A2A capabilities for susi-gawd
//!
//! Defines the capabilities that susi-gawd advertises to other A2A agents.

/// susi-gawd's advertised A2A capabilities
#[derive(Debug, Clone)]
pub struct GawdCapabilities {
    /// Streaming support
    pub streaming: bool,
    /// Push notifications support
    pub push_notifications: bool,
}

impl Default for GawdCapabilities {
    fn default() -> Self {
        Self {
            streaming: true,
            push_notifications: true,
        }
    }
}

impl GawdCapabilities {
    /// Create capabilities for susi-gawd orchestrator
    pub fn orchestrator() -> Self {
        Self {
            streaming: true,
            push_notifications: true,
        }
    }

    /// Get A2A capabilities for agent card
    pub fn as_capabilities(&self) -> ra2a::types::AgentCapabilities {
        ra2a::types::AgentCapabilities {
            streaming: Some(self.streaming),
            push_notifications: Some(self.push_notifications),
            ..Default::default()
        }
    }
}
