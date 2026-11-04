// Converts the compiled-in rules/axioms/components (AlphaSelf) into synthetic
// Q&A samples in the workspace experience buffer, where semantic search
// indexes them alongside recorded interactions.

use crate::self_core::AlphaSelf;
use crate::susi_error::EaiResult;
use std::fs;
use std::path::Path;
#[derive(Clone, serde::Serialize)]
struct ReasoningSample {
    intent: String,
    blackboard_context: String,
    successful_outcome: String,
    timestamp: u64,
}

pub struct GenomeDistiller;

/// The workspace experience buffer — the file interaction capture appends
/// to and semantic search indexes (`<workspace>/.susi/reasoning_experience.jsonl`).
pub fn experience_path(workspace: &Path) -> std::path::PathBuf {
    workspace.join(".susi").join("reasoning_experience.jsonl")
}

impl GenomeDistiller {
    /// Appends the compiled genome as synthetic Q&A samples to the
    /// workspace experience buffer; returns how many were written.
    pub fn distill_genome_to_experience(workspace: &Path) -> EaiResult<usize> {
        let mut samples = Vec::new();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // 1. Distill identity.json (Governance Axiom Rules)
        for rule in AlphaSelf::RULES {
            samples.push(ReasoningSample {
                intent: format!("What does {} ({}) say?", rule.cite, rule.title),
                blackboard_context: "susi_genome_audit".to_string(),
                successful_outcome: format!("{}: {}. {}", rule.cite, rule.title, rule.imperative),
                timestamp,
            });
        }

        // 3. Distill PULSE_AXIOMS
        for axiom in AlphaSelf::PULSE_AXIOMS {
            samples.push(ReasoningSample {
                intent: format!("What does ledger entry {} record?", axiom.cite),
                blackboard_context: "susi_pulse_axioms".to_string(),
                successful_outcome: format!(
                    "Ledger entry {}: {}. {}",
                    axiom.cite, axiom.title, axiom.imperative
                ),
                timestamp,
            });
        }

        // 4. Distill identity.json Components
        for comp in AlphaSelf::COMPONENTS {
            samples.push(ReasoningSample {
                intent: format!("What is the role of {} in the substrate?", comp.name),
                blackboard_context: "topology_lookup".to_string(),
                successful_outcome: format!(
                    "{} is a Tier {:?} component. Description: {}",
                    comp.name, comp.tier, comp.description
                ),
                timestamp,
            });
        }

        // 4. Append to the workspace experience buffer (previously a
        // config-dir file that nothing read).
        let exp_file = experience_path(workspace);
        if let Some(parent) = exp_file.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&exp_file)?;
        use std::io::Write;
        let mut count = 0;
        for sample in samples {
            if let Ok(json) = serde_json::to_string(&sample) {
                f.write_all(format!("{json}\n").as_bytes())?;
                count += 1;
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn experience_path_points_to_workspace_dot_susi_reasoning_experience_jsonl() {
        let ws = Path::new("/tmp/test-susi-ws");
        assert_eq!(
            experience_path(ws),
            ws.join(".susi").join("reasoning_experience.jsonl")
        );
    }

    #[test]
    fn distill_genome_to_experience_writes_expected_sample_count_and_creates_dir() {
        let tmp = std::env::temp_dir().join(format!(
            "susi_genome_distiller_count_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        let written = GenomeDistiller::distill_genome_to_experience(&tmp).expect("distill genome");
        let expected =
            AlphaSelf::RULES.len() + AlphaSelf::PULSE_AXIOMS.len() + AlphaSelf::COMPONENTS.len();
        assert_eq!(written, expected);
        assert!(experience_path(&tmp).is_file());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn distill_genome_to_experience_emits_valid_jsonl_covering_rules_axioms_and_components() {
        let tmp = std::env::temp_dir().join(format!(
            "susi_genome_distiller_jsonl_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        GenomeDistiller::distill_genome_to_experience(&tmp).expect("distill genome");
        let content = fs::read_to_string(experience_path(&tmp)).expect("read jsonl");
        let lines: Vec<&str> = content.lines().collect();
        assert!(!lines.is_empty());

        let mut contexts = std::collections::HashSet::new();
        for line in lines {
            let val: serde_json::Value = serde_json::from_str(line).expect("valid json line");
            let ctx = val
                .get("blackboard_context")
                .and_then(|v| v.as_str())
                .expect("blackboard_context");
            contexts.insert(ctx.to_string());
        }
        assert!(contexts.contains("susi_genome_audit"));
        assert!(contexts.contains("susi_pulse_axioms"));
        assert!(contexts.contains("topology_lookup"));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn distill_genome_to_experience_appends_on_subsequent_invocations() {
        let tmp = std::env::temp_dir().join(format!(
            "susi_genome_distiller_append_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        let first = GenomeDistiller::distill_genome_to_experience(&tmp).expect("first distill");
        let second = GenomeDistiller::distill_genome_to_experience(&tmp).expect("second distill");
        assert_eq!(first, second);

        let content = fs::read_to_string(experience_path(&tmp)).expect("read jsonl");
        assert_eq!(content.lines().count(), first * 2);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn distill_genome_to_experience_entries_have_non_empty_intent_and_outcome() {
        let tmp = std::env::temp_dir().join(format!(
            "susi_genome_distiller_fields_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        GenomeDistiller::distill_genome_to_experience(&tmp).expect("distill genome");
        let content = fs::read_to_string(experience_path(&tmp)).expect("read jsonl");
        for line in content.lines() {
            let val: serde_json::Value = serde_json::from_str(line).expect("json line");
            let intent = val
                .get("intent")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let outcome = val
                .get("successful_outcome")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            assert!(!intent.is_empty());
            assert!(!outcome.is_empty());
            assert!(val.get("timestamp").and_then(|v| v.as_u64()).is_some());
        }
        let _ = fs::remove_dir_all(&tmp);
    }
}
