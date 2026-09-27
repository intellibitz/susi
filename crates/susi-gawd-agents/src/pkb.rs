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
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(distillation_file)?;
        use std::io::Write;
        file.write_all(&line)?;
        Ok(())
    }

    /// Stages successful, receipt-backed tool executions as classifier pairs.
    pub fn consolidate_verified_receipts(workspace: &Path) -> EaiResult<usize> {
        let mut count = 0;
        for entry in crate::susi_core::receipt_archive::ReceiptArchive::load_audit_lines(workspace)
        {
            let intent = entry.training_intent.as_deref().unwrap_or("").trim();
            let action = entry.tool.trim();
            let successful = entry.successful;
            if successful && !intent.is_empty() && !action.is_empty() {
                Self::stage_distillation_pair(
                    intent,
                    action,
                    workspace,
                    Some(serde_json::json!({
                        "source": "tool_receipt",
                        "receipt_id": entry.receipt_id,
                        "output_hash": entry.output_hash,
                    })),
                )?;
                count += 1;
            }
        }
        Ok(count)
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
    fn consolidation_uses_only_successful_receipt_backed_actions() {
        let dir = tempfile::tempdir().unwrap();
        let susi_dir = dir.path().join(".susi");
        std::fs::create_dir_all(&susi_dir).unwrap();
        std::fs::write(
            susi_dir.join("receipt_archive.jsonl"),
            concat!(
                "{\"schema\":\"susi/receipt_archive/v1\",\"kind\":\"tool_receipt\",\"session_id\":\"s1\",\"mission_goal_hash\":\"mh1\",\"training_intent\":\"inspect repo\",\"receipt_id\":\"r1\",\"tool\":\"list_directory\",\"arguments\":\"{}\",\"observed_at\":1,\"output_hash\":\"h1\",\"successful\":true,\"archived_at\":1}\n",
                "{\"schema\":\"susi/receipt_archive/v1\",\"kind\":\"tool_receipt\",\"session_id\":\"s2\",\"mission_goal_hash\":\"mh2\",\"training_intent\":\"delete repo\",\"receipt_id\":\"r2\",\"tool\":\"write_file\",\"arguments\":\"{}\",\"observed_at\":2,\"output_hash\":\"h2\",\"successful\":false,\"archived_at\":2}\n"
            ),
        )
        .unwrap();

        assert_eq!(
            ProtocolKnowledgeBase::consolidate_verified_receipts(dir.path()).unwrap(),
            1
        );
        let staged = std::fs::read_to_string(susi_dir.join("distillation_staged.jsonl")).unwrap();
        let record: serde_json::Value = serde_json::from_str(staged.trim()).unwrap();
        assert_eq!(record["intent"], "inspect repo");
        assert_eq!(record["action"], "list_directory");
        assert_eq!(record["performance_metadata"]["receipt_id"], "r1");
    }

    #[test]
    fn consolidation_ignores_legacy_receipts_without_training_intent() {
        let dir = tempfile::tempdir().unwrap();
        let susi_dir = dir.path().join(".susi");
        std::fs::create_dir_all(&susi_dir).unwrap();
        std::fs::write(
            susi_dir.join("receipt_archive.jsonl"),
            "{\"schema\":\"susi/receipt_archive/v1\",\"kind\":\"tool_receipt\",\"session_id\":\"s1\",\"mission_goal_hash\":\"mh1\",\"receipt_id\":\"r1\",\"tool\":\"status\",\"arguments\":\"{}\",\"observed_at\":1,\"output_hash\":\"h1\",\"successful\":true,\"archived_at\":1}\n",
        )
        .unwrap();
        assert_eq!(
            ProtocolKnowledgeBase::consolidate_verified_receipts(dir.path()).unwrap(),
            0
        );
        assert!(!susi_dir.join("distillation_staged.jsonl").exists());
    }
}
