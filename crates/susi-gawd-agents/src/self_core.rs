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

    /// One inventory line: `- Label (N): name1, name2, …` (or `(0): (none)`).
    pub fn format_pillar_inventory(label: &str, comps: &[SusiComponentSpec]) -> String {
        if comps.is_empty() {
            format!("- {} (0): (none)", label)
        } else {
            let names = comps.iter().map(|c| c.name).collect::<Vec<_>>().join(", ");
            format!("- {} ({}): {}", label, comps.len(), names)
        }
    }

    #[allow(dead_code)]
    pub fn inspect_compiled_binary_instructions() -> String {
        let mut out = format!(
            "SUSI Substrate Compiled Binary Instructions:\n- Version: {}\n- Paradigm: {}\n- Hardcoded Axiom Rules: {}\n",
            Self::VERSION,
            Self::CORE_PARADIGM,
            Self::RULES.len(),
        );
        out.push_str(&Self::format_pillar_inventory(
            "AoA Pillar",
            Self::AOA_COMPONENTS,
        ));
        out.push('\n');
        out.push_str(&Self::format_pillar_inventory(
            "Agents Pillar",
            Self::AGENT_COMPONENTS,
        ));
        out.push('\n');
        out.push_str(&Self::format_pillar_inventory(
            "Engines Pillar",
            Self::ENGINE_COMPONENTS,
        ));
        out.push('\n');
        out.push_str(&Self::format_pillar_inventory(
            "Models Pillar",
            Self::MODEL_COMPONENTS,
        ));
        out.push('\n');
        out.push_str(&Self::format_pillar_inventory(
            "MCPs Pillar",
            Self::MCP_COMPONENTS,
        ));
        out.push('\n');
        out.push_str(&Self::format_pillar_inventory(
            "Realized Capabilities",
            Self::REALIZED_COMPONENTS,
        ));
        out
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
        assert!(
            summary.contains("AoA Pillar ("),
            "pillar lines must include counts in parentheses"
        );
        // Names — not just counts — so install/identity output is actionable.
        assert!(
            !AlphaSelf::AOA_COMPONENTS.is_empty()
                && summary.contains(AlphaSelf::AOA_COMPONENTS[0].name),
            "AoA pillar must list component names, got:\n{summary}"
        );
        assert!(
            !AlphaSelf::AGENT_COMPONENTS.is_empty()
                && summary.contains(AlphaSelf::AGENT_COMPONENTS[0].name),
            "Agents pillar must list component names, got:\n{summary}"
        );
        assert!(
            !AlphaSelf::MODEL_COMPONENTS.is_empty()
                && summary.contains(AlphaSelf::MODEL_COMPONENTS[0].name),
            "Models pillar must list component names (identity from source), got:\n{summary}"
        );
        assert!(
            summary.contains("CodingModelManager")
                || summary.contains("FrontierManager")
                || summary.contains("OpenWeightManager"),
            "Models pillar must include curated model managers, got:\n{summary}"
        );
    }
}
