// Build-time-generated rule/component tables (see build.rs), compiled in as
// static Rust data rather than parsed from config at runtime.

#[derive(Debug, Clone, Copy)]
pub enum SusiCoreTier {
    Tier0Reflex,
    Tier1Swarm,
    Tier2Reasoning,
}

#[derive(Debug, Clone)]
pub struct SusiComponentSpec {
    pub name: &'static str,
    pub tier: SusiCoreTier,
    pub description: &'static str,
}

#[derive(Debug, Clone)]
pub struct SusiAxiomRule {
    pub id: usize,
    pub title: &'static str,
    pub imperative: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/generated_axioms.rs"));

pub struct AlphaSelf;

impl AlphaSelf {
    /// Root `susi` / engine version from workspace `Cargo.toml` (build.rs),
    /// not this leaf crate's `0.1.0`.
    pub const VERSION: &'static str = GEN_ENGINE_VERSION;
    pub const CORE_PARADIGM: &'static str =
        "susi — evidence-gated intelligence reflex & execution substrate (GAWD / GEMI / GMCP)";

    /// Agent-governance identity ledger (`susi/identity/v1`) — not end-user docs.
    pub const IDENTITY_JSON: &'static str = include_str!("../../../.agents/identity.json");
    /// Agent-governance evidence ledger (`susi/evidence/v1`) — not end-user docs.
    pub const EVIDENCE_JSON: &'static str = include_str!("../../../.agents/evidence.json");
    /// Agent-governance roadmap ledger (`susi/roadmap/v1`) — not end-user docs.
    pub const ROADMAP_JSON: &'static str = include_str!("../../../.agents/roadmap.json");

    pub const RULES: &[SusiAxiomRule] = GEN_RULES;
    pub const PULSE_AXIOMS: &[SusiAxiomRule] = GEN_PULSE_AXIOMS;

    // 6 Pillar Component Topology
    pub const AOA_COMPONENTS: &[SusiComponentSpec] = GEN_AOA_COMPONENTS;
    pub const AGENT_COMPONENTS: &[SusiComponentSpec] = GEN_AGENT_COMPONENTS;
    pub const ENGINE_COMPONENTS: &[SusiComponentSpec] = GEN_ENGINE_COMPONENTS;
    pub const MODEL_COMPONENTS: &[SusiComponentSpec] = GEN_MODEL_COMPONENTS;
    pub const MCP_COMPONENTS: &[SusiComponentSpec] = GEN_MCP_COMPONENTS;
    pub const REALIZED_COMPONENTS: &[SusiComponentSpec] = GEN_REALIZED_COMPONENTS;
    pub const COMPONENTS: &[SusiComponentSpec] = GEN_COMPONENTS;

    #[allow(dead_code)]
    pub fn inspect_compiled_binary_instructions() -> String {
        format!(
            "SUSI Substrate Compiled Binary Instructions:\n- Version: {}\n- Paradigm: {}\n- Hardcoded Axiom Rules: {}\n- AoA Pillar: {}\n- Agents Pillar: {}\n- Engines Pillar: {}\n- Models Pillar: {}\n- MCPs Pillar: {}\n- Realized Capabilities: {}",
            Self::VERSION,
            Self::CORE_PARADIGM,
            Self::RULES.len(),
            Self::AOA_COMPONENTS.len(),
            Self::AGENT_COMPONENTS.len(),
            Self::ENGINE_COMPONENTS.len(),
            Self::MODEL_COMPONENTS.len(),
            Self::MCP_COMPONENTS.len(),
            Self::REALIZED_COMPONENTS.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compiled_genome_accuracy() {
        assert_eq!(AlphaSelf::VERSION, GEN_ENGINE_VERSION);
        assert!(
            !AlphaSelf::RULES.is_empty(),
            "identity.json axioms must be compiled into binary"
        );
        assert!(
            !AlphaSelf::COMPONENTS.is_empty(),
            "identity.json components must be compiled into binary"
        );
        assert!(
            !AlphaSelf::PULSE_AXIOMS.is_empty(),
            "evidence.json axioms must be compiled into binary"
        );
        assert!(
            AlphaSelf::IDENTITY_JSON.contains("susi/identity/v1"),
            "identity.json must declare schema"
        );
        assert!(
            AlphaSelf::EVIDENCE_JSON.contains("susi/evidence/v1"),
            "evidence.json must declare schema"
        );
        assert!(
            AlphaSelf::ROADMAP_JSON.contains("susi/roadmap/v1"),
            "roadmap.json must declare schema"
        );

        let summary = AlphaSelf::inspect_compiled_binary_instructions();
        assert!(summary.contains(GEN_ENGINE_VERSION));
        assert!(summary.contains("Hardcoded Axiom Rules:"));
    }
}
