// Converts the compiled-in rules/axioms/components (AlphaSelf) into synthetic
// Q&A training samples for the local Tier 2 reasoning model.

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

impl GenomeDistiller {
    /// Distills the hard-compiled .md files into synthetic Q&A pairs for model training.
    pub fn distill_genome_to_experience(_workspace: &Path) -> EaiResult<usize> {
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

        // 4. Append to reasoning_experience.jsonl for later training
        let global_dir = crate::susi_paths::SusiDirs::config_dir();
        if !global_dir.exists() {
            fs::create_dir_all(&global_dir)?;
        }
        let exp_file = global_dir.join("reasoning_experience.jsonl");
        let mut count = 0;
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(exp_file)
        {
            use std::io::Write;
            for sample in samples {
                if let Ok(json) = serde_json::to_string(&sample) {
                    let _ = writeln!(f, "{}", json);
                    count += 1;
                }
            }
        }

        Ok(count)
    }
}
