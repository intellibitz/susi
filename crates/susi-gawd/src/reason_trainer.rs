// Ensures the workspace experience buffer has data (distilling it from
// AlphaSelf if empty) and reports what it holds. No model is trained here.

use crate::genome_distiller::GenomeDistiller;
use crate::susi_error::EaiResult;
use std::path::Path;

pub struct ReasoningTrainer;

impl ReasoningTrainer {
    pub fn audit_reasoning_substrate(workspace: &Path) -> EaiResult<String> {
        let experience_file = crate::genome_distiller::experience_path(workspace);
        let count = || {
            std::fs::read_to_string(&experience_file)
                .map(|text| text.lines().filter(|l| !l.trim().is_empty()).count())
                .unwrap_or(0)
        };
        let existing = count();
        let distilled = if existing == 0 {
            GenomeDistiller::distill_genome_to_experience(workspace)?
        } else {
            0
        };
        Ok(format!(
            "Experience buffer {}: {} samples ({} distilled from the compiled genome this run). \
             Indexed by semantic search; no model is trained by this audit.",
            experience_file.display(),
            count(),
            distilled
        ))
    }
}
