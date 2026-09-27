// Watches distillation_staged.jsonl and kicks off training once it crosses
// the configured sample threshold.

use crate::susi_core::plane_bus::gemi::SusiAlphaModel;
use crate::susi_error::{EaiError, EaiResult};
use std::path::Path;

pub struct ReflexTrainer;

impl ReflexTrainer {
    /// Checks whether the staged-sample count has crossed the training threshold.
    pub fn audit_distillation_state(workspace: &Path) -> EaiResult<String> {
        let staged_file = workspace.join(".susi/distillation_staged.jsonl");

        let count = match staged_sample_count(&staged_file)? {
            Some(count) => count,
            None => return Ok("Reflex substrate optimal.".into()),
        };
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global()?;

        if count >= cfg.reflex_training_threshold() {
            eprintln!("[Reflex Trainer] Wisdom buffer saturated ({count} samples). Triggering native distillation...");
            let report = SusiAlphaModel::train_on_staged_data(workspace)
                .map_err(|e| EaiError::inference(format!("reflex distillation failed: {e}")))?;
            std::fs::remove_file(&staged_file).map_err(|e| {
                EaiError::io(format!(
                    "retire consumed reflex samples {}: {e}",
                    staged_file.display()
                ))
            })?;
            return Ok(report);
        }

        Ok("Reflex substrate optimal.".into())
    }

    pub fn force_train(workspace: &Path) -> EaiResult<String> {
        SusiAlphaModel::train_on_staged_data(workspace)
            .map_err(|e| crate::susi_error::EaiError::inference(e.to_string()))
    }
}

fn staged_sample_count(path: &Path) -> EaiResult<Option<usize>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content.lines().count())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(EaiError::io(format!(
            "read staged reflex samples {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::staged_sample_count;

    #[test]
    fn missing_staging_is_idle() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            staged_sample_count(&dir.path().join("missing.jsonl")).unwrap(),
            None
        );
    }

    #[test]
    fn counts_complete_and_final_unterminated_samples() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("staged.jsonl");
        std::fs::write(&path, "{\"sample\":1}\n{\"sample\":2}").unwrap();
        assert_eq!(staged_sample_count(&path).unwrap(), Some(2));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_staging_is_not_reported_as_idle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("staged.jsonl");
        std::fs::create_dir(&path).unwrap();
        let error = staged_sample_count(&path).unwrap_err();
        assert!(error.to_string().contains("read staged reflex samples"));
    }
}
