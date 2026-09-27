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
                intent: format!("What is the mandate for rule {}?", rule.title),
                blackboard_context: "susi_genome_audit".to_string(),
                successful_outcome: format!(
                    "Rule {}: {}. Imperative: {}",
                    rule.id, rule.title, rule.imperative
                ),
                timestamp,
            });
        }

        // 3. Distill PULSE_AXIOMS
        for axiom in AlphaSelf::PULSE_AXIOMS {
            samples.push(ReasoningSample {
                intent: format!("What is pulse axiom {}?", axiom.title),
                blackboard_context: "susi_pulse_axioms".to_string(),
                successful_outcome: format!(
                    "Pulse Axiom {}: {}. Imperative: {}",
                    axiom.id, axiom.title, axiom.imperative
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
                writeln!(f, "{json}")?;
                count += 1;
            }
        }
        Ok(count)
    }
}
