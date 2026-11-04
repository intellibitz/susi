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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gawd_capabilities_default_orchestrator_and_agent_capabilities_mapping() {
        let def = GawdCapabilities::default();
        assert!(def.streaming);
        assert!(def.push_notifications);

        let orch = GawdCapabilities::orchestrator();
        assert!(orch.streaming);
        assert!(orch.push_notifications);

        let mapped = orch.as_capabilities();
        assert_eq!(mapped.streaming, Some(true));
        assert_eq!(mapped.push_notifications, Some(true));

        let custom = GawdCapabilities {
            streaming: false,
            push_notifications: true,
        };
        let custom_mapped = custom.as_capabilities();
        assert_eq!(custom_mapped.streaming, Some(false));
        assert_eq!(custom_mapped.push_notifications, Some(true));
    }
}
