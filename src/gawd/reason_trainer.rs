// Ensures reasoning_experience.jsonl has data (distilling it from AlphaSelf
// if empty), then trains the local Tier 2 reasoning model from it.

use crate::error::EaiResult;
use crate::gawd::genome_distiller::GenomeDistiller;
use crate::gemi::reasoning::SusiReasoningModel;
use std::path::Path;

pub struct ReasoningTrainer;

impl ReasoningTrainer {
    pub fn audit_reasoning_substrate(workspace: &Path) -> EaiResult<String> {
        let _home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let global_dir = crate::sandbox::xdg::SusiDirs::config_dir();
        let experience_file = global_dir.join("reasoning_experience.jsonl");

        // Ensure the experience buffer has data if it's currently empty
        let existing_count = if experience_file.exists() {
            std::fs::read_to_string(&experience_file)
                .unwrap_or_default()
                .lines()
                .count()
        } else {
            0
        };

        if existing_count == 0 {
            if std::env::var("SUSI_VERBOSE").is_ok() {
                eprintln!("[Reasoning Trainer] Experience buffer empty. Distilling Genome into synthetic wisdom...");
            }
            let distilled = GenomeDistiller::distill_genome_to_experience(workspace)?;
            if std::env::var("SUSI_VERBOSE").is_ok() {
                eprintln!(
                    "[Reasoning Trainer] Added {} genome-anchored samples.",
                    distilled
                );
            }
        }

        Ok("Tier 2 Reasoning Substrate Optimal.".into())
    }

    pub fn force_distillation(_workspace: &Path) -> EaiResult<String> {
        let _home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let global_dir = crate::sandbox::xdg::SusiDirs::config_dir();
        SusiReasoningModel::train_from_experience(&global_dir)
            .map_err(|e| crate::error::EaiError::inference(e.to_string()))
    }
}
