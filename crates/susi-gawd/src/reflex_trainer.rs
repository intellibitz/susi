// Watches distillation_staged.jsonl and kicks off training once it crosses
// the configured sample threshold.

use crate::susi_core::plane_bus::gemi::SusiAlphaModel;
use crate::susi_error::{EaiError, EaiResult};
use std::path::{Path, PathBuf};

pub struct ReflexTrainer;

impl ReflexTrainer {
    /// Checks whether the staged-sample count has crossed the training threshold.
    pub fn audit_distillation_state(workspace: &Path) -> EaiResult<String> {
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global()?;
        let Some(claim) = claim_staged_samples(workspace, cfg.reflex_training_threshold())? else {
            return Ok("Reflex substrate optimal.".into());
        };
        eprintln!(
            "[Reflex Trainer] Wisdom buffer saturated ({} samples). Triggering native distillation...",
            claim.sample_count
        );
        let report = match SusiAlphaModel::train_on_staged_data(&claim.root) {
            Ok(report) => report,
            Err(error) => {
                restore_claim(&claim)?;
                return Err(EaiError::inference(format!(
                    "reflex distillation failed: {error}"
                )));
            }
        };
        retire_claim(&claim)?;
        Ok(report)
    }

    pub fn force_train(workspace: &Path) -> EaiResult<String> {
        let claim = claim_staged_samples(workspace, 1)?
            .ok_or_else(|| EaiError::inference("No staged distillation data found."))?;
        match SusiAlphaModel::train_on_staged_data(&claim.root) {
            Ok(report) => {
                retire_claim(&claim)?;
                Ok(report)
            }
            Err(error) => {
                restore_claim(&claim)?;
                Err(EaiError::inference(error.to_string()))
            }
        }
    }
}

struct TrainingClaim {
    root: PathBuf,
    path: PathBuf,
    staged_path: PathBuf,
    sample_count: usize,
}

fn claim_staged_samples(workspace: &Path, threshold: usize) -> EaiResult<Option<TrainingClaim>> {
    let susi_dir = workspace.join(".susi");
    let _lock = crate::susi_config::file_lock::FileLock::acquire(&susi_dir, "distillation_staged")
        .ok_or_else(|| EaiError::io("acquire distillation staging lock"))?;
    let staged_path = susi_dir.join("distillation_staged.jsonl");
    let Some(sample_count) = staged_sample_count(&staged_path)? else {
        return Ok(None);
    };
    if sample_count < threshold {
        return Ok(None);
    }
    let generation = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let root = susi_dir.join(format!("reflex-claim-{generation}-{}", std::process::id()));
    let claim_dir = root.join(".susi");
    std::fs::create_dir_all(&claim_dir)?;
    let path = claim_dir.join("distillation_staged.jsonl");
    std::fs::rename(&staged_path, &path)?;
    Ok(Some(TrainingClaim {
        root,
        path,
        staged_path,
        sample_count,
    }))
}

fn restore_claim(claim: &TrainingClaim) -> EaiResult<()> {
    let susi_dir = claim
        .staged_path
        .parent()
        .ok_or_else(|| EaiError::io("staging path has no parent"))?;
    let _lock = crate::susi_config::file_lock::FileLock::acquire(susi_dir, "distillation_staged")
        .ok_or_else(|| EaiError::io("acquire distillation restore lock"))?;
    let mut restored = std::fs::read(&claim.path)?;
    if !restored.is_empty() && !restored.ends_with(b"\n") {
        restored.push(b'\n');
    }
    match std::fs::read(&claim.staged_path) {
        Ok(current) => restored.extend_from_slice(&current),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    crate::susi_config::atomic_write_bytes(&claim.staged_path, &restored)?;
    retire_claim(claim)
}

fn retire_claim(claim: &TrainingClaim) -> EaiResult<()> {
    match std::fs::remove_file(&claim.path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    std::fs::remove_dir_all(&claim.root)?;
    Ok(())
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
    use super::{claim_staged_samples, restore_claim, retire_claim, staged_sample_count};

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

    #[test]
    fn claim_isolates_samples_appended_while_training() {
        let dir = tempfile::tempdir().unwrap();
        let susi_dir = dir.path().join(".susi");
        std::fs::create_dir_all(&susi_dir).unwrap();
        let staged = susi_dir.join("distillation_staged.jsonl");
        std::fs::write(&staged, "old\n").unwrap();
        let claim = claim_staged_samples(dir.path(), 1).unwrap().unwrap();
        std::fs::write(&staged, "new\n").unwrap();
        assert_eq!(std::fs::read_to_string(&claim.path).unwrap(), "old\n");
        assert_eq!(std::fs::read_to_string(&staged).unwrap(), "new\n");
        retire_claim(&claim).unwrap();
        assert_eq!(std::fs::read_to_string(staged).unwrap(), "new\n");
    }

    #[test]
    fn failed_training_restores_claim_before_new_samples() {
        let dir = tempfile::tempdir().unwrap();
        let susi_dir = dir.path().join(".susi");
        std::fs::create_dir_all(&susi_dir).unwrap();
        let staged = susi_dir.join("distillation_staged.jsonl");
        std::fs::write(&staged, "old\n").unwrap();
        let claim = claim_staged_samples(dir.path(), 1).unwrap().unwrap();
        std::fs::write(&staged, "new\n").unwrap();
        restore_claim(&claim).unwrap();
        assert_eq!(std::fs::read_to_string(staged).unwrap(), "old\nnew\n");
        assert!(!claim.root.exists());
    }
}
