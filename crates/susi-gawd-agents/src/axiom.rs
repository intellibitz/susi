// Formats the compiled-in rules and components from `AlphaSelf` (plain Rust
// consts, not a parsed config file) into summary strings.

use crate::self_core::AlphaSelf;
use std::path::Path;

pub struct AxiomSubstrate;

impl AxiomSubstrate {
    /// Renders `AlphaSelf::RULES` and `AlphaSelf::COMPONENTS` as markdown summaries.
    pub fn ingest_constitution(_workspace: &Path) -> (String, String) {
        let mut agents_summary = String::new();
        agents_summary.push_str("# Compiled Binary Axiom Rules\n");
        for rule in AlphaSelf::RULES {
            agents_summary.push_str(&format!(
                "{}. **{}**: {}\n",
                rule.id, rule.title, rule.imperative
            ));
        }

        let mut projects_summary = String::new();
        projects_summary.push_str("# Compiled Binary Component Topology\n");
        for comp in AlphaSelf::COMPONENTS {
            projects_summary.push_str(&format!(
                "- **{}** ({:?}): {}\n",
                comp.name, comp.tier, comp.description
            ));
        }

        (agents_summary, projects_summary)
    }

    #[allow(dead_code)]
    pub fn get_substrate_summary() -> String {
        AlphaSelf::inspect_compiled_binary_instructions()
    }
}
