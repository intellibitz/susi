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

#[cfg(test)]
mod inline_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fresh_temp_workspace(tag: &str) -> std::path::PathBuf {
        static NONCE: AtomicU64 = AtomicU64::new(0);
        let n = NONCE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "susi-reason-trainer-{tag}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn audit_reasoning_substrate_distills_genome_when_experience_buffer_missing() {
        let ws = fresh_temp_workspace("missing");
        let summary = ReasoningTrainer::audit_reasoning_substrate(&ws).unwrap();
        assert!(summary.contains("samples ("));
        assert!(!summary.contains("(0 distilled from the compiled genome this run)"));
        assert!(crate::genome_distiller::experience_path(&ws).exists());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn audit_reasoning_substrate_is_idempotent_on_populated_experience_buffer() {
        let ws = fresh_temp_workspace("idempotent");
        let _ = ReasoningTrainer::audit_reasoning_substrate(&ws).unwrap();
        let second = ReasoningTrainer::audit_reasoning_substrate(&ws).unwrap();
        assert!(second.contains("(0 distilled from the compiled genome this run)"));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn audit_reasoning_substrate_distills_when_experience_file_is_whitespace_only() {
        let ws = fresh_temp_workspace("blank");
        let exp = crate::genome_distiller::experience_path(&ws);
        std::fs::create_dir_all(exp.parent().unwrap()).unwrap();
        std::fs::write(&exp, "\n   \n").unwrap();

        let summary = ReasoningTrainer::audit_reasoning_substrate(&ws).unwrap();
        assert!(!summary.contains("(0 distilled from the compiled genome this run)"));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn audit_reasoning_substrate_preserves_custom_non_empty_experience_lines() {
        let ws = fresh_temp_workspace("custom");
        let exp = crate::genome_distiller::experience_path(&ws);
        std::fs::create_dir_all(exp.parent().unwrap()).unwrap();
        std::fs::write(&exp, "{\"sample\":1}\n{\"sample\":2}\n").unwrap();

        let summary = ReasoningTrainer::audit_reasoning_substrate(&ws).unwrap();
        assert!(summary.contains("2 samples (0 distilled from the compiled genome this run)"));
        let _ = std::fs::remove_dir_all(&ws);
    }
}
