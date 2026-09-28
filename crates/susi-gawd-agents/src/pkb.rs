// Builds/exports the training data (reflex records) used by the local
// Tier 0 model: bootstrap examples plus ones mined from the audit log.

use crate::susi_error::EaiResult;
use std::path::Path;

pub struct ProtocolKnowledgeBase;

impl ProtocolKnowledgeBase {
    /// Stages a reasoning pair for autonomous distillation into local reflexes
    pub fn stage_distillation_pair(
        intent: &str,
        action: &str,
        workspace: &Path,
        metadata: Option<serde_json::Value>,
    ) -> EaiResult<()> {
        let susi_dir = workspace.join(".susi");
        std::fs::create_dir_all(&susi_dir)?;
        let distillation_file = susi_dir.join("distillation_staged.jsonl");
        let entry = serde_json::json!({
            "intent": intent,
            "action": action,
            "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            "performance_metadata": metadata,
        });

        let mut line = serde_json::to_vec(&entry)?;
        line.push(b'\n');
        // Same lock as `ReflexTrainer`'s claim/restore/recover: they read the
        // buffer and atomically replace it, so an unlocked append landing in
        // between writes to the orphaned inode and the sample is lost.
        let _lock =
            crate::susi_config::file_lock::FileLock::acquire(&susi_dir, "distillation_staged")
                .ok_or_else(|| {
                    crate::susi_error::EaiError::io("acquire distillation staging lock")
                })?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(distillation_file)?;
        use std::io::Write;
        file.write_all(&line)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_reports_persistence_failure() {
        let workspace =
            std::env::temp_dir().join(format!("susi-pkb-write-error-{}", std::process::id()));
        let path = workspace.join(".susi/distillation_staged.jsonl");
        std::fs::create_dir_all(&path).unwrap();

        assert!(ProtocolKnowledgeBase::stage_distillation_pair(
            "intent", "action", &workspace, None
        )
        .is_err());
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn staging_waits_for_the_trainer_lock() {
        let workspace = std::env::temp_dir().join(format!("susi-pkb-lock-{}", std::process::id()));
        let susi_dir = workspace.join(".susi");
        std::fs::create_dir_all(&susi_dir).unwrap();
        // Hold the trainer's lock: a claim is mid-rewrite of the buffer.
        let held =
            crate::susi_config::file_lock::FileLock::acquire(&susi_dir, "distillation_staged")
                .unwrap();
        let ws = workspace.clone();
        let writer = std::thread::spawn(move || {
            ProtocolKnowledgeBase::stage_distillation_pair("intent", "action", &ws, None)
        });
        std::thread::sleep(std::time::Duration::from_millis(150));
        let path = susi_dir.join("distillation_staged.jsonl");
        assert!(!path.exists(), "append must not bypass a held staging lock");
        drop(held);
        writer.join().unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
        let _ = std::fs::remove_dir_all(workspace);
    }
}
