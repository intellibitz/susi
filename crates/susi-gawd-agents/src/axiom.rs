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

    #[test]
    fn ingest_constitution_agents_summary_bullet_lines_match_rules_count_and_format() {
        let (agents_summary, _) = AxiomSubstrate::ingest_constitution(Path::new("."));
        let mut lines = agents_summary.lines();
        let header = lines.next().expect("agents_summary must have header");
        assert!(header.starts_with("# Compiled Genome ("));
        let bullets: Vec<&str> = lines.filter(|l| l.starts_with("- **")).collect();
        assert!(bullets.len() >= AlphaSelf::RULES.len());
        for bullet in &bullets {
            assert!(bullet.contains(": "));
        }
        for rule in AlphaSelf::RULES {
            let prefix = format!("- **{}** {}: ", rule.cite, rule.title);
            assert!(agents_summary.contains(&prefix), "missing prefix {prefix}");
        }
    }

    #[test]
    fn ingest_constitution_projects_summary_bullet_lines_match_components_count_and_format() {
        let (_, projects_summary) = AxiomSubstrate::ingest_constitution(Path::new("."));
        let mut lines = projects_summary.lines();
        let header = lines.next().expect("projects_summary must have header");
        assert_eq!(header, "# Compiled Binary Component Topology");
        let bullets: Vec<&str> = lines.collect();
        assert_eq!(bullets.len(), AlphaSelf::COMPONENTS.len());
        for bullet in bullets {
            assert!(bullet.starts_with("- **"));
            assert!(bullet.contains(": "));
        }
    }

    #[test]
    fn ingest_constitution_returns_non_empty_trimmed_summaries_ending_with_newline() {
        let (agents_summary, projects_summary) =
            AxiomSubstrate::ingest_constitution(Path::new("."));
        assert!(agents_summary.ends_with('\n'));
        assert!(projects_summary.ends_with('\n'));
        assert!(!agents_summary.trim().is_empty());
        assert!(!projects_summary.trim().is_empty());
    }
}
