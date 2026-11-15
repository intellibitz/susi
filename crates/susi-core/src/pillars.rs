//! Canonical architectural pillars. Protocols and execution engines are capabilities,
//! not additional pillars (CEO Step 1 directive, 2026-11-15).
//! This taxonomy does not change protocol addresses, crate names or permissions.

/// The complete set of Susi core architectural pillars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CorePillar {
    Gawd,
    Gemi,
    Gmcp,
}

impl CorePillar {
    pub const ALL: [Self; 3] = [Self::Gawd, Self::Gemi, Self::Gmcp];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gawd => "gawd",
            Self::Gemi => "gemi",
            Self::Gmcp => "gmcp",
        }
    }

    /// Resolve an existing protocol/capability label without removing legacy inputs.
    pub fn from_capability(name: &str) -> Option<Self> {
        match name {
            "gawd" | "a2a" | "acp" | "harness" | "swarm" => Some(Self::Gawd),
            "gemi" | "inference" => Some(Self::Gemi),
            "gmcp" | "mcp" | "jsonrpc" => Some(Self::Gmcp),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CorePillar;

    #[test]
    fn exactly_three_canonical_pillars() {
        assert_eq!(
            CorePillar::ALL.map(CorePillar::as_str),
            ["gawd", "gemi", "gmcp"]
        );
    }

    #[test]
    fn gawd_owns_a2a_acp_and_parallel_harness_capabilities() {
        for name in ["gawd", "a2a", "acp", "harness", "swarm"] {
            assert_eq!(CorePillar::from_capability(name), Some(CorePillar::Gawd));
        }
    }

    #[test]
    fn model_and_tool_planes_remain_distinct() {
        assert_eq!(
            CorePillar::from_capability("inference"),
            Some(CorePillar::Gemi)
        );
        assert_eq!(CorePillar::from_capability("mcp"), Some(CorePillar::Gmcp));
        assert_eq!(CorePillar::from_capability("enterprise-ui"), None);
    }
}
