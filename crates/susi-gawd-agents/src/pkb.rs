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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod contract_tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn workspace(tag: &str) -> std::path::PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi-pkb-contract-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn staged_lines(workspace: &Path) -> Vec<serde_json::Value> {
        let text =
            std::fs::read_to_string(workspace.join(".susi/distillation_staged.jsonl")).unwrap();
        assert!(text.ends_with('\n'), "every record is newline-terminated");
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn staging_creates_the_susi_directory_and_one_complete_record() {
        let ws = workspace("create");
        assert!(!ws.exists());
        ProtocolKnowledgeBase::stage_distillation_pair("plan", "act", &ws, None).unwrap();

        let records = staged_lines(&ws);
        assert_eq!(records.len(), 1);
        let record = records[0].as_object().unwrap();
        let mut keys: Vec<&str> = record.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["action", "intent", "performance_metadata", "timestamp"]
        );
        assert_eq!(record["intent"], json!("plan"));
        assert_eq!(record["action"], json!("act"));
        assert!(
            record["timestamp"].as_u64().unwrap() > 1_600_000_000,
            "a real unix timestamp, not the 0 fallback"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn staging_appends_in_order_and_keeps_metadata_or_null_per_record() {
        let ws = workspace("append");
        let metadata = json!({"latency_ms": 12, "tools": ["scan", "build"], "ok": true});
        ProtocolKnowledgeBase::stage_distillation_pair("i1", "a1", &ws, Some(metadata.clone()))
            .unwrap();
        ProtocolKnowledgeBase::stage_distillation_pair("i2", "a2", &ws, None).unwrap();
        ProtocolKnowledgeBase::stage_distillation_pair("i3", "a3", &ws, Some(json!({}))).unwrap();

        let records = staged_lines(&ws);
        assert_eq!(records.len(), 3);
        let intents: Vec<_> = records.iter().map(|r| r["intent"].clone()).collect();
        assert_eq!(intents, [json!("i1"), json!("i2"), json!("i3")]);
        assert_eq!(records[0]["performance_metadata"], metadata);
        assert_eq!(records[1]["performance_metadata"], json!(null));
        assert_eq!(records[2]["performance_metadata"], json!({}));
        let times: Vec<u64> = records
            .iter()
            .map(|r| r["timestamp"].as_u64().unwrap())
            .collect();
        assert!(times.windows(2).all(|pair| pair[0] <= pair[1]));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn staging_escapes_newlines_so_each_record_stays_on_one_line() {
        let ws = workspace("escape");
        let intent = "line one\nline two \"quoted\" \\ backslash — ünï 数据";
        let action = "{\"nested\": [1, 2]}\r\nsecond";
        ProtocolKnowledgeBase::stage_distillation_pair(intent, action, &ws, None).unwrap();
        ProtocolKnowledgeBase::stage_distillation_pair("next", "next", &ws, None).unwrap();

        let raw = std::fs::read_to_string(ws.join(".susi/distillation_staged.jsonl")).unwrap();
        assert_eq!(
            raw.lines().count(),
            2,
            "embedded newlines must not split a record"
        );
        let records = staged_lines(&ws);
        assert_eq!(records[0]["intent"], json!(intent));
        assert_eq!(records[0]["action"], json!(action));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn concurrent_staging_never_tears_interleaves_or_loses_an_acknowledged_record() {
        // The staging lock has a bounded wait, so under heavy machine load a
        // contender may time out and report an error instead of appending.
        // What must hold regardless: every Ok(()) is on disk exactly once and
        // intact, no line is torn or interleaved, and nothing unacknowledged
        // appears.
        let ws = workspace("concurrent");
        std::fs::create_dir_all(ws.join(".susi")).unwrap();
        let handles: Vec<_> = (0..8)
            .map(|n| {
                let ws = ws.clone();
                std::thread::spawn(move || {
                    let payload = "x".repeat(2000);
                    let outcome = ProtocolKnowledgeBase::stage_distillation_pair(
                        &format!("intent-{n}"),
                        &payload,
                        &ws,
                        None,
                    );
                    (n, outcome.is_ok())
                })
            })
            .collect();
        let mut acknowledged: Vec<String> = Vec::new();
        for handle in handles {
            let (n, ok) = handle.join().unwrap();
            if ok {
                acknowledged.push(format!("intent-{n}"));
            }
        }
        assert!(
            !acknowledged.is_empty(),
            "at least one writer must win the lock"
        );

        let path = ws.join(".susi/distillation_staged.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'));
        let mut on_disk: Vec<String> = text
            .lines()
            .map(|line| {
                let record: serde_json::Value = serde_json::from_str(line).unwrap();
                assert_eq!(
                    record["action"].as_str().unwrap().len(),
                    2000,
                    "intact payload"
                );
                record["intent"].as_str().unwrap().to_string()
            })
            .collect();
        on_disk.sort();
        acknowledged.sort();
        assert_eq!(on_disk, acknowledged);
        let _ = std::fs::remove_dir_all(&ws);
    }
}
