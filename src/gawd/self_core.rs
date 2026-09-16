// SUSI Core Substrate: Compiled Binary Instructions Core
// Eliminates runtime string parsing by encoding axioms, agent rules, and component topologies
// directly into strongly-typed compiled Rust data structures and enums.

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
    pub const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    pub const CORE_PARADIGM: &'static str =
        "EAI (Exponential Intelligence for Any AI) - Intelligence Reflex & Execution Substrate";

    pub const IDENTITY_MD: &'static str = include_str!("../../.agents/IDENTITY.md");
    pub const EVIDENCE_MD: &'static str = include_str!("../../.agents/EVIDENCE.md");
    pub const ROADMAP_MD: &'static str = include_str!("../../.agents/ROADMAP.md");

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
        assert_eq!(AlphaSelf::VERSION, env!("CARGO_PKG_VERSION"));
        assert!(
            !AlphaSelf::RULES.is_empty(),
            "IDENTITY.md axioms must be compiled into binary"
        );
        assert!(
            !AlphaSelf::COMPONENTS.is_empty(),
            "IDENTITY.md components must be compiled into binary"
        );
        assert!(
            !AlphaSelf::PULSE_AXIOMS.is_empty(),
            "EVIDENCE.md axioms must be compiled into binary"
        );

        let summary = AlphaSelf::inspect_compiled_binary_instructions();
        assert!(summary.contains(env!("CARGO_PKG_VERSION")));
        assert!(summary.contains("Hardcoded Axiom Rules:"));
    }
}
