// Watches distillation_staged.jsonl and kicks off training once it crosses
// the configured sample threshold.

use std::path::Path;
use susi_error::EaiResult;
use susi_gemi::alpha::SusiAlphaModel;

pub struct ReflexTrainer;

impl ReflexTrainer {
    /// Checks whether the staged-sample count has crossed the training threshold.
    pub fn audit_distillation_state(_workspace: &Path) -> EaiResult<String> {
        let _home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let global_dir = susi_paths::SusiDirs::config_dir();
        let staged_file = global_dir.join("distillation_staged.jsonl");

        if staged_file.exists() {
            let content = std::fs::read_to_string(&staged_file).unwrap_or_default();
            let count = content.lines().count();
            let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();

            if count >= cfg.reflex_training_threshold() {
                eprintln!("[Reflex Trainer] Wisdom buffer saturated ({} samples). Triggering native distillation...", count);
                match SusiAlphaModel::train_on_staged_data(&global_dir) {
                    Ok(report) => {
                        // Clear the buffer after successful evolution
                        let _ = std::fs::remove_file(&staged_file);
                        return Ok(report);
                    }
                    Err(e) => return Ok(format!("Distillation Failure: {}", e)),
                }
            }
        }

        Ok("Reflex substrate optimal.".into())
    }

    pub fn force_train(_workspace: &Path) -> EaiResult<String> {
        let _home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let global_dir = susi_paths::SusiDirs::config_dir();
        SusiAlphaModel::train_on_staged_data(&global_dir)
            .map_err(|e| susi_error::EaiError::inference(e.to_string()))
    }
}
