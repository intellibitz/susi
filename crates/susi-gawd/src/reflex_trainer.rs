// Watches distillation_staged.jsonl and kicks off training once it crosses
// the configured sample threshold.

use crate::susi_core::plane_bus::gemi::SusiAlphaModel;
use crate::susi_error::{EaiError, EaiResult};
use std::path::{Path, PathBuf};

pub struct ReflexTrainer;

impl ReflexTrainer {
    /// Checks whether the staged-sample count has crossed the training threshold.
    pub fn audit_distillation_state(workspace: &Path) -> EaiResult<String> {
        let susi_dir = workspace.join(".susi");
        let _cycle_lock =
            crate::susi_config::file_lock::FileLock::acquire(&susi_dir, "reflex_claim_training")
                .ok_or_else(|| EaiError::io("acquire reflex training-cycle lock"))?;
        recover_orphaned_claims(workspace)?;
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global()?;
        let Some(claim) = claim_staged_samples(workspace, cfg.reflex_training_threshold())? else {
            return Ok(format!(
                "Reflex substrate optimal.\n{}",
                staging_health_line(workspace)
            ));
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
        Ok(format!("{report}\n{}", staging_health_line(workspace)))
    }

    pub fn force_train(workspace: &Path) -> EaiResult<String> {
        let susi_dir = workspace.join(".susi");
        let _cycle_lock =
            crate::susi_config::file_lock::FileLock::acquire(&susi_dir, "reflex_claim_training")
                .ok_or_else(|| EaiError::io("acquire reflex training-cycle lock"))?;
        recover_orphaned_claims(workspace)?;
        let claim = claim_staged_samples(workspace, 1)?
            .ok_or_else(|| EaiError::inference("No staged distillation data found."))?;
        match SusiAlphaModel::train_on_staged_data(&claim.root) {
            Ok(report) => {
                retire_claim(&claim)?;
                Ok(format!("{report}\n{}", staging_health_line(workspace)))
            }
            Err(error) => {
                restore_claim(&claim)?;
                Err(EaiError::inference(error.to_string()))
            }
        }
    }
}

fn staging_health_line(workspace: &Path) -> String {
    crate::susi_core::receipt_archive::ReceiptArchive::staging_health_summary(workspace).summary()
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
    // Sanitize the claimed buffer: a crash-interrupted append can leave a
    // truncated trailing JSON line that causes parse_training_entries to
    // reject the entire batch. Keep only lines that parse as valid JSON.
    let validated = sanitize_claimed_buffer(&path)?;
    let claim = TrainingClaim {
        root,
        path,
        staged_path,
        sample_count: validated,
    };
    if validated == 0 {
        // Nothing trainable survived. Retire the claim so a missing file
        // cannot fail distillation and then fail the restore.
        retire_claim(&claim)?;
        return Ok(None);
    }
    if validated < threshold {
        // Torn lines inflated the pre-claim count. Put the survivors back
        // so they accumulate toward the real threshold.
        let bytes = std::fs::read(&claim.path)?;
        crate::susi_config::atomic_write_bytes(&claim.staged_path, &bytes)?;
        retire_claim(&claim)?;
        return Ok(None);
    }
    Ok(Some(claim))
}

/// Rewrite a claimed staging file, keeping only complete JSON lines.
/// Blank lines are dropped too: `parse_training_entries` rejects the whole
/// batch on an empty record. Returns the validated line count. An entirely
/// empty result removes the file and returns 0.
fn sanitize_claimed_buffer(path: &Path) -> EaiResult<usize> {
    let content = std::fs::read_to_string(path)?;
    let mut valid = Vec::new();
    let mut dropped = 0usize;
    for line in content.lines() {
        if line.trim().is_empty() {
            dropped += 1;
            continue;
        }
        if serde_json::from_str::<serde_json::Value>(line).is_ok() {
            valid.push(line);
        } else {
            dropped += 1;
        }
    }
    if dropped == 0 {
        return Ok(valid.len());
    }
    eprintln!(
        "[Reflex Trainer] Sanitized claim: dropped {dropped} malformed line(s), {} valid samples retained.",
        valid.len()
    );
    // Construction records the drop in the typed error-metrics sink.
    let _emit = crate::susi_error::EaiError::io(format!(
        "sanitized {dropped} malformed staged training line(s)"
    ));
    if valid.is_empty() {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        return Ok(0);
    }
    let mut buf = valid.join("\n");
    buf.push('\n');
    crate::susi_config::atomic_write_bytes(path, buf.as_bytes())?;
    Ok(valid.len())
}

fn claim_generation(name: &str) -> Option<u128> {
    let suffix = name.strip_prefix("reflex-claim-")?;
    let (generation, pid) = suffix.rsplit_once('-')?;
    pid.parse::<u32>().ok()?;
    generation.parse::<u128>().ok()
}

fn recover_orphaned_claims(workspace: &Path) -> EaiResult<usize> {
    let susi_dir = workspace.join(".susi");
    let _staging_lock =
        crate::susi_config::file_lock::FileLock::acquire(&susi_dir, "distillation_staged")
            .ok_or_else(|| EaiError::io("acquire orphan-claim recovery lock"))?;
    let mut claims = Vec::new();
    match std::fs::read_dir(&susi_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                let Some(generation) = claim_generation(name) else {
                    continue;
                };
                let path = entry.path().join(".susi/distillation_staged.jsonl");
                if path.is_file() {
                    claims.push((generation, entry.path(), path));
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    }
    if claims.is_empty() {
        return Ok(0);
    }
    claims.sort_by_key(|(generation, _, _)| *generation);
    let mut restored = Vec::new();
    for (_, _, path) in &claims {
        let bytes = std::fs::read(path)?;
        restored.extend_from_slice(&bytes);
        if !restored.is_empty() && !restored.ends_with(b"\n") {
            restored.push(b'\n');
        }
    }
    let staged_path = susi_dir.join("distillation_staged.jsonl");
    match std::fs::read(&staged_path) {
        Ok(current) => restored.extend_from_slice(&current),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    crate::susi_config::atomic_write_bytes(&staged_path, &restored)?;
    for (_, root, _) in &claims {
        std::fs::remove_dir_all(root)?;
    }
    Ok(claims.len())
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
    use super::{
        claim_staged_samples, recover_orphaned_claims, restore_claim, retire_claim,
        sanitize_claimed_buffer, staged_sample_count,
    };

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
        std::fs::write(
            &staged,
            "{\"intent\":\"old\",\"action\":\"a\",\"timestamp\":1}\n",
        )
        .unwrap();
        let claim = claim_staged_samples(dir.path(), 1).unwrap().unwrap();
        std::fs::write(
            &staged,
            "{\"intent\":\"new\",\"action\":\"a\",\"timestamp\":2}\n",
        )
        .unwrap();
        let claimed = std::fs::read_to_string(&claim.path).unwrap();
        assert!(claimed.contains("\"old\""));
        let live = std::fs::read_to_string(&staged).unwrap();
        assert!(live.contains("\"new\""));
        retire_claim(&claim).unwrap();
        assert!(std::fs::read_to_string(staged).unwrap().contains("\"new\""));
    }

    #[test]
    fn failed_training_restores_claim_before_new_samples() {
        let dir = tempfile::tempdir().unwrap();
        let susi_dir = dir.path().join(".susi");
        std::fs::create_dir_all(&susi_dir).unwrap();
        let staged = susi_dir.join("distillation_staged.jsonl");
        std::fs::write(
            &staged,
            "{\"intent\":\"old\",\"action\":\"a\",\"timestamp\":1}\n",
        )
        .unwrap();
        let claim = claim_staged_samples(dir.path(), 1).unwrap().unwrap();
        std::fs::write(
            &staged,
            "{\"intent\":\"new\",\"action\":\"a\",\"timestamp\":2}\n",
        )
        .unwrap();
        restore_claim(&claim).unwrap();
        let restored = std::fs::read_to_string(staged).unwrap();
        assert!(
            restored.contains("\"old\""),
            "old samples must be restored first"
        );
        assert!(
            restored.contains("\"new\""),
            "new samples must follow restored"
        );
        assert!(!claim.root.exists());
    }

    #[test]
    fn orphan_claims_recover_oldest_first_before_live_samples() {
        let dir = tempfile::tempdir().unwrap();
        let susi_dir = dir.path().join(".susi");
        std::fs::create_dir_all(&susi_dir).unwrap();
        for (name, body) in [
            ("reflex-claim-20-7", "second\n"),
            ("reflex-claim-10-7", "first\n"),
        ] {
            let claim_dir = susi_dir.join(name).join(".susi");
            std::fs::create_dir_all(&claim_dir).unwrap();
            std::fs::write(claim_dir.join("distillation_staged.jsonl"), body).unwrap();
        }
        std::fs::write(susi_dir.join("distillation_staged.jsonl"), "live\n").unwrap();

        assert_eq!(recover_orphaned_claims(dir.path()).unwrap(), 2);
        assert_eq!(
            std::fs::read_to_string(susi_dir.join("distillation_staged.jsonl")).unwrap(),
            "first\nsecond\nlive\n"
        );
        assert!(!susi_dir.join("reflex-claim-10-7").exists());
        assert!(!susi_dir.join("reflex-claim-20-7").exists());
    }

    #[test]
    fn sanitize_strips_malformed_trailing_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claimed.jsonl");
        // Two valid lines, a blank record, and one truncated trailing line.
        std::fs::write(
            &path,
            "{\"intent\":\"a\",\"action\":\"b\",\"timestamp\":1}\n\n{\"intent\":\"c\",\"action\":\"d\",\"timestamp\":2}\n{\"intent\":\"truncated-tail\",\"actio",
        )
        .unwrap();
        let count = sanitize_claimed_buffer(&path).unwrap();
        assert_eq!(count, 2, "only the two valid lines should survive");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"a\""));
        assert!(content.contains("\"c\""));
        assert!(
            !content.contains("truncated-tail"),
            "truncated line must be stripped"
        );
        assert!(
            !content.lines().any(|line| line.trim().is_empty()),
            "blank records must be stripped"
        );
    }

    #[test]
    fn sanitize_entirely_malformed_yields_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claimed.jsonl");
        std::fs::write(&path, "not json at all\n").unwrap();
        let count = sanitize_claimed_buffer(&path).unwrap();
        assert_eq!(count, 0);
        assert!(!path.exists(), "empty result must remove the file");
    }

    #[test]
    fn claim_trains_only_when_valid_samples_meet_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let susi_dir = dir.path().join(".susi");
        std::fs::create_dir_all(&susi_dir).unwrap();
        let staged = susi_dir.join("distillation_staged.jsonl");
        std::fs::write(
            &staged,
            "{\"intent\":\"keep\",\"action\":\"a\",\"timestamp\":1}\n{\"intent\":\"truncated-tail\",\"actio",
        )
        .unwrap();

        let below = claim_staged_samples(dir.path(), 2).unwrap();
        assert!(
            below.is_none(),
            "a torn line must not inflate the threshold"
        );
        let restored = std::fs::read_to_string(&staged).unwrap();
        assert!(restored.contains("\"keep\""));
        assert!(!restored.contains("truncated-tail"));
        assert!(!susi_dir.read_dir().unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("reflex-claim-")
        }));

        let claim = claim_staged_samples(dir.path(), 1).unwrap().unwrap();
        assert_eq!(claim.sample_count, 1);
        let claimed = std::fs::read_to_string(&claim.path).unwrap();
        assert!(claimed.contains("\"keep\""));
        assert!(!claimed.contains("truncated-tail"));
        retire_claim(&claim).unwrap();
    }

    #[test]
    fn claim_all_malformed_leaves_no_claim() {
        let dir = tempfile::tempdir().unwrap();
        let susi_dir = dir.path().join(".susi");
        std::fs::create_dir_all(&susi_dir).unwrap();
        let staged = susi_dir.join("distillation_staged.jsonl");
        std::fs::write(&staged, "{\"intent\":\"truncated-tail\",\"actio").unwrap();
        assert!(claim_staged_samples(dir.path(), 1).unwrap().is_none());
        assert!(!staged.exists());
        assert!(!susi_dir.read_dir().unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("reflex-claim-")
        }));
    }
}
