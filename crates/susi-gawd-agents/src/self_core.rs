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

/// Which governance source a compiled rule comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    /// identity.json Pillar I (DNA) mandate.
    Mandate,
    /// identity.json Pillar IV (engine) protocol.
    EngineProtocol,
    /// roadmap.json vector.
    RoadmapVector,
    /// evidence.json ledger entry.
    Evidence,
}

#[derive(Debug, Clone)]
pub struct SusiAxiomRule {
    pub kind: RuleKind,
    /// Canonical citation: `Mandate N`, `Pillar IV item N`, or the roadmap
    /// vector / ledger entry ID. Only a `Mandate` is ever a mandate.
    pub cite: &'static str,
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

    /// Compiled rules of one kind.
    pub fn rules_of(kind: RuleKind) -> impl Iterator<Item = &'static SusiAxiomRule> {
        Self::RULES.iter().filter(move |r| r.kind == kind)
    }

    /// Rule counts by source, e.g. "56 mandates, 7 engine protocols, 2
    /// roadmap vectors, 282 ledger entries". One total would present ledger
    /// entries as rules they are not.
    pub fn genome_summary() -> String {
        format!(
            "{} mandates, {} engine protocols, {} roadmap vectors, {} ledger entries",
            Self::rules_of(RuleKind::Mandate).count(),
            Self::rules_of(RuleKind::EngineProtocol).count(),
            Self::rules_of(RuleKind::RoadmapVector).count(),
            Self::rules_of(RuleKind::Evidence).count(),
        )
    }

    pub fn inspect_compiled_binary_instructions() -> String {
        let mut out = format!(
            "SUSI Substrate Compiled Binary Instructions:\n- Version: {}\n- Paradigm: {}\n- Compiled Genome: {}\n",
            Self::VERSION,
            Self::CORE_PARADIGM,
            Self::genome_summary(),
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

/// Substrate identity report: compiled axiom inventory + live brain context.
/// Was `CoreTools::identity` in susi-gmcp — lives here so the MCP layer never
/// imports the swarm host.
pub fn identity_report(workspace: &std::path::Path) -> String {
    let brain = crate::brain::AlphaBrainContext::initialize(workspace);
    let mut report = String::new();
    report.push_str("# susi Substrate - Identity Report\n\n");
    report.push_str("## 1. CORE CONFIGURATION (Compiled Binary Axiomatic Core)\n");
    report.push_str(&format!("- Version: {}\n", AlphaSelf::VERSION));
    report.push_str(&format!("- Core Paradigm: {}\n", AlphaSelf::CORE_PARADIGM));
    report.push_str(&format!(
        "- Compiled Genome: {}\n",
        AlphaSelf::genome_summary()
    ));
    for (label, comps) in [
        ("AoA Pillar", AlphaSelf::AOA_COMPONENTS),
        ("Agents Pillar", AlphaSelf::AGENT_COMPONENTS),
        ("Engines Pillar", AlphaSelf::ENGINE_COMPONENTS),
        ("Models Pillar", AlphaSelf::MODEL_COMPONENTS),
        ("MCPs Pillar", AlphaSelf::MCP_COMPONENTS),
        ("Realized Capabilities", AlphaSelf::REALIZED_COMPONENTS),
    ] {
        report.push_str(&AlphaSelf::format_pillar_inventory(label, comps));
        report.push('\n');
    }
    report.push('\n');
    report.push_str("## 2. SYSTEM ENVIRONMENT\n");
    report.push_str(&format!(
        "- CPUs: {}\n- RAM: {}GB\n- Workspace: {}\n",
        brain.system_cpus,
        brain.system_ram_gb,
        brain.workspace_path.display()
    ));
    report
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
        assert!(summary.contains("Compiled Genome:"));
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

    #[test]
    fn every_rule_is_cited_by_its_real_source() {
        let mut seen = std::collections::HashSet::new();
        for rule in AlphaSelf::RULES {
            assert!(seen.insert(rule.cite), "duplicate citation {}", rule.cite);
            let ok = match rule.kind {
                RuleKind::Mandate => rule.cite.starts_with("Mandate "),
                RuleKind::EngineProtocol => rule.cite.starts_with("Pillar IV item "),
                RuleKind::RoadmapVector => !rule.cite.starts_with("Mandate "),
                RuleKind::Evidence => rule.cite.starts_with("EV-"),
            };
            assert!(ok, "{:?} rule mis-cited as {}", rule.kind, rule.cite);
        }
        let mandates: Vec<_> = AlphaSelf::rules_of(RuleKind::Mandate).collect();
        assert!(mandates.iter().any(|r| r.cite == "Mandate 1"));
        assert!(!mandates.iter().any(|r| r.title.contains("RSI iteration")));
    }
}
