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

    #[test]
    fn gawd_capabilities_boolean_matrix_clone_debug_and_unset_history_default() {
        for (streaming, push_notifications) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let caps = GawdCapabilities {
                streaming,
                push_notifications,
            };
            let cloned = caps.clone();
            assert_eq!(cloned.streaming, streaming);
            assert_eq!(cloned.push_notifications, push_notifications);

            let dbg = format!("{cloned:?}");
            assert!(dbg.contains("GawdCapabilities"));

            let a2a_caps = cloned.as_capabilities();
            let default_a2a = ra2a::types::AgentCapabilities::default();
            assert_eq!(a2a_caps.streaming, Some(streaming));
            assert_eq!(a2a_caps.push_notifications, Some(push_notifications));
            assert_eq!(a2a_caps.extensions.len(), default_a2a.extensions.len());
            assert_eq!(
                a2a_caps.extended_agent_card,
                default_a2a.extended_agent_card
            );
        }
    }
}
