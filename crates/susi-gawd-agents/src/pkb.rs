// Builds/exports the training data (reflex records) used by the local
// Tier 0 model: bootstrap examples plus ones mined from the audit log.

use std::path::Path;
use susi_error::EaiResult;

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
        if !susi_dir.exists() {
            let _ = std::fs::create_dir_all(&susi_dir);
        }
        let distillation_file = susi_dir.join("distillation_staged.jsonl");
        let entry = serde_json::json!({
            "intent": intent,
            "action": action,
            "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            "performance_metadata": metadata,
        });

        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(distillation_file)
        {
            use std::io::Write;
            let _ = writeln!(f, "{}", entry);
        }
        Ok(())
    }

    /// Stages successful, non-trivial past interactions (outcome > 100 chars,
    /// not marked `[FAIL]`) from memory.jsonl as distillation pairs.
    pub fn consolidate_recent_interactions(workspace: &Path) -> EaiResult<usize> {
        let memory_file = workspace.join(".susi/memory.jsonl");
        if !memory_file.exists() {
            return Ok(0);
        }

        let content = std::fs::read_to_string(&memory_file)?;
        let mut count = 0;
        for line in content.lines() {
            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
                let intent = entry["intent"].as_str().unwrap_or("");
                let outcome = entry["outcome"].as_str().unwrap_or("");

                // Only consolidate successful, complex reasoning ( > 100 chars )
                if outcome.len() > 100 && !outcome.contains("[FAIL]") {
                    Self::stage_distillation_pair(intent, outcome, workspace, None)?;
                    count += 1;
                }
            }
        }
        Ok(count)
    }
}
