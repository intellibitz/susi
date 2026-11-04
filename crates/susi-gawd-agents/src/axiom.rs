// Formats the compiled-in rules and components from `AlphaSelf` (plain Rust
// consts, not a parsed config file) into summary strings.

use crate::self_core::AlphaSelf;
use std::path::Path;

pub struct AxiomSubstrate;

impl AxiomSubstrate {
    /// Renders `AlphaSelf::RULES` and `AlphaSelf::COMPONENTS` as markdown summaries.
    pub fn ingest_constitution(_workspace: &Path) -> (String, String) {
        let mut agents_summary = String::new();
        agents_summary.push_str(&format!(
            "# Compiled Genome ({})\n",
            AlphaSelf::genome_summary()
        ));
        for rule in AlphaSelf::RULES {
            agents_summary.push_str(&format!(
                "- **{}** {}: {}\n",
                rule.cite, rule.title, rule.imperative
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn ingest_constitution_renders_all_compiled_rules_and_components() {
        let (agents_summary, projects_summary) =
            AxiomSubstrate::ingest_constitution(Path::new("."));
        assert!(agents_summary.starts_with(&format!(
            "# Compiled Genome ({})\n",
            AlphaSelf::genome_summary()
        )));
        for rule in AlphaSelf::RULES {
            assert!(
                agents_summary.contains(rule.cite),
                "missing rule cite {}",
                rule.cite
            );
            assert!(
                agents_summary.contains(rule.title),
                "missing rule title {}",
                rule.title
            );
        }

        assert!(projects_summary.starts_with("# Compiled Binary Component Topology\n"));
        for comp in AlphaSelf::COMPONENTS {
            assert!(
                projects_summary.contains(comp.name),
                "missing component {}",
                comp.name
            );
            assert!(
                projects_summary.contains(comp.description),
                "missing component description for {}",
                comp.name
            );
        }
    }

    #[test]
    fn ingest_constitution_line_counts_imperatives_tiers_and_workspace_invariance() {
        let (rules_a, comps_a) = AxiomSubstrate::ingest_constitution(Path::new("/alpha/ws"));
        let (rules_b, comps_b) = AxiomSubstrate::ingest_constitution(Path::new("relative/beta"));
        assert_eq!(rules_a, rules_b);
        assert_eq!(comps_a, comps_b);

        assert!(rules_a.lines().count() > AlphaSelf::RULES.len());
        assert_eq!(comps_a.lines().count(), 1 + AlphaSelf::COMPONENTS.len());

        for rule in AlphaSelf::RULES {
            let expected_line = format!("- **{}** {}: {}", rule.cite, rule.title, rule.imperative);
            assert!(
                rules_a.contains(&expected_line),
                "missing formatted rule entry for {}",
                rule.cite
            );
        }
        for comp in AlphaSelf::COMPONENTS {
            let tier_tag = format!("({:?})", comp.tier);
            assert!(
                comps_a.contains(&tier_tag),
                "missing tier tag {tier_tag} for {}",
                comp.name
            );
        }
    }
}
