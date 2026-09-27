//! Append-only tool-receipt audit archive under `{workspace}/.susi/receipt_archive.jsonl`.
//!
//! **Not a truth authority.** Lines are forensic/audit records only. Mission
//! completion (`EvidenceSession::verify_answer` / `resolve` / `bind_receipt`)
//! must re-validate against the *live* in-memory ledger and workspace — never
//! by reconstituting citations from this file.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::susi_core::capture::ToolReceipt;

pub const ARCHIVE_REL: &str = ".susi/receipt_archive.jsonl";
pub const ARCHIVE_SCHEMA: &str = "susi/receipt_archive/v1";
const ROTATE_BYTES: u64 = 16 * 1024 * 1024;
const KEEP_GENERATIONS: u32 = 8;

/// One append-only audit line. Response bodies are omitted — hashes + provenance only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchivedReceipt {
    pub schema: String,
    pub kind: String,
    pub session_id: String,
    pub mission_goal_hash: String,
    /// Redacted mission text used only as local classifier training input.
    /// Older v1 records predate this additive field and remain readable.
    #[serde(default)]
    pub training_intent: Option<String>,
    pub receipt_id: String,
    pub tool: String,
    pub arguments: String,
    pub observed_at: u64,
    pub output_hash: String,
    pub successful: bool,
    pub archived_at: u64,
    /// Whether the training sample derived from this receipt was persisted
    /// to the staging buffer. `Some(true)` = persisted, `Some(false)` =
    /// staging failed (error entered the typed metrics), `None` = no staging
    /// attempted (unsuccessful receipt or empty intent/action).
    /// Additive: older v1 records predate this field and deserialize as `None`.
    #[serde(default)]
    pub training_staged: Option<bool>,
}

pub struct ReceiptArchive;

impl ReceiptArchive {
    pub fn path(workspace: &Path) -> PathBuf {
        workspace.join(ARCHIVE_REL)
    }

    /// Best-effort append. Archive write failure never fails the mission.
    /// The archived record now includes `training_staged` so operators can
    /// observe whether each receipt's training sample was actually persisted.
    pub fn append(
        workspace: &Path,
        session_id: &str,
        mission_goal: &str,
        training_intent: &str,
        receipt: &ToolReceipt,
    ) {
        // Determine staging eligibility before building the record.
        let staging_eligible = receipt.successful
            && !training_intent.trim().is_empty()
            && !receipt.tool.trim().is_empty();
        let mut record = ArchivedReceipt {
            schema: ARCHIVE_SCHEMA.into(),
            kind: "tool_receipt".into(),
            session_id: session_id.to_string(),
            mission_goal_hash: hex::encode(Sha256::digest(mission_goal.as_bytes())),
            training_intent: Some(training_intent.to_string()),
            receipt_id: receipt.id.clone(),
            tool: receipt.tool.clone(),
            arguments: receipt.arguments.clone(),
            observed_at: receipt.observed_at,
            output_hash: receipt.output_hash.clone(),
            successful: receipt.successful,
            archived_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            // Populated after the staging attempt; `None` for ineligible receipts.
            training_staged: None,
        };
        let path = Self::path(workspace);
        let Some(parent) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let _guard = archive_lock().lock();
        // The CLI and daemon can archive into the same workspace. Rotation
        // and append must be one cross-process operation or one writer can
        // append to the generation another writer is renaming.
        let Some(_file_lock) =
            crate::susi_core::commit_log::FileLock::acquire(parent, "receipt_archive")
        else {
            return;
        };
        Self::rotate_if_large(&path);
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
            // Stage the training sample first so the outcome is recorded in
            // the archive line — the archive line is the single observable
            // record of the staging attempt.
            if staging_eligible {
                record.training_staged = Some(Self::append_training_sample(
                    parent,
                    training_intent,
                    receipt,
                ));
            }
            let Ok(line) = serde_json::to_string(&record) else {
                return;
            };
            let mut bytes = line.into_bytes();
            bytes.push(b'\n');
            let _ = file.write_all(&bytes);
        }
    }

    /// Stage a training sample derived from a successful receipt. Returns
    /// `true` when the sample was durably written, `false` on any failure.
    /// Failures now emit a typed `EaiError::io` so they enter the
    /// `error_metrics.jsonl` sink instead of disappearing.
    fn append_training_sample(dir: &Path, training_intent: &str, receipt: &ToolReceipt) -> bool {
        let record = serde_json::json!({
            "intent": training_intent,
            "action": receipt.tool,
            "timestamp": receipt.observed_at,
            "performance_metadata": {
                "source": "tool_receipt",
                "receipt_id": receipt.id,
                "output_hash": receipt.output_hash,
            },
        });
        let bytes = match serde_json::to_vec(&record) {
            Ok(mut b) => {
                b.push(b'\n');
                b
            }
            Err(err) => {
                let _emit = crate::susi_error::EaiError::io(format!(
                    "training sample serialization failed for receipt {}: {err}",
                    receipt.id
                ));
                return false;
            }
        };
        let Some(_lock) =
            crate::susi_core::commit_log::FileLock::acquire(dir, "distillation_staged")
        else {
            let _emit = crate::susi_error::EaiError::io(format!(
                "training sample staging lock acquisition failed for receipt {}",
                receipt.id
            ));
            return false;
        };
        let staged_path = dir.join("distillation_staged.jsonl");
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&staged_path)
        {
            Ok(mut file) => match file.write_all(&bytes) {
                Ok(()) => true,
                Err(err) => {
                    let _emit = crate::susi_error::EaiError::io(format!(
                        "training sample write failed for receipt {}: {err}",
                        receipt.id
                    ));
                    false
                }
            },
            Err(err) => {
                let _emit = crate::susi_error::EaiError::io(format!(
                    "training sample staging file open failed for receipt {}: {err}",
                    receipt.id
                ));
                false
            }
        }
    }

    /// Audit lines are forensic — completeness, not recency, is the value,
    /// so the bound is generational rotation rather than tail truncation:
    /// past `ROTATE_BYTES` the live file becomes `.1`, prior generations
    /// shift up, and the oldest beyond `KEEP_GENERATIONS` is dropped.
    /// Total footprint stays under `ROTATE_BYTES * (KEEP_GENERATIONS + 1)`.
    fn rotate_if_large(path: &Path) {
        let oversized = std::fs::metadata(path)
            .map(|m| m.len() > ROTATE_BYTES)
            .unwrap_or(false);
        if !oversized {
            return;
        }
        let oldest = path.with_file_name(format!(
            "{}.{KEEP_GENERATIONS}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
        ));
        let _ = std::fs::remove_file(&oldest);
        for g in (1..KEEP_GENERATIONS).rev() {
            let from = path.with_file_name(format!(
                "{}.{g}",
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
            ));
            if from.exists() {
                let to = path.with_file_name(format!(
                    "{}.{}",
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default(),
                    g + 1
                ));
                let _ = std::fs::rename(&from, &to);
            }
        }
        let first = path.with_file_name(format!(
            "{}.1",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
        ));
        let _ = std::fs::rename(path, &first);
    }

    /// Read archived lines for audit tooling. Does **not** restore live ledger
    /// authority — callers must not feed these into citation resolution.
    pub fn load_audit_lines(workspace: &Path) -> Vec<ArchivedReceipt> {
        let live = Self::path(workspace);
        let name = live
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let mut paths = (1..=KEEP_GENERATIONS)
            .rev()
            .map(|generation| live.with_file_name(format!("{name}.{generation}")))
            .collect::<Vec<_>>();
        paths.push(live);
        paths
            .into_iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .flat_map(|text| {
                text.lines()
                    .filter(|line| !line.trim().is_empty())
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

fn archive_lock() -> &'static parking_lot::Mutex<()> {
    static LOCK: OnceLock<parking_lot::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| parking_lot::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::susi_core::capture::EvidenceSession;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    struct Workspace(PathBuf);
    impl Workspace {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "susi-archive-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn session(workspace: &Workspace) -> Arc<EvidenceSession> {
        EvidenceSession::new("archive-mission", &workspace.0, |s| s.to_string()).unwrap()
    }

    #[test]
    fn capture_appends_audit_line_without_output_body() {
        let ws = Workspace::new();
        let session = session(&ws);
        let _activation = EvidenceSession::activate(&session);
        EvidenceSession::capture_call(
            "exec_command",
            &serde_json::json!({"cmd": "uname"}),
            &ws.0,
            || Ok("Linux host".to_string()),
        )
        .unwrap();
        let lines = ReceiptArchive::load_audit_lines(&ws.0);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].schema, ARCHIVE_SCHEMA);
        assert_eq!(lines[0].tool, "exec_command");
        assert_eq!(lines[0].training_intent.as_deref(), Some("archive-mission"));
        assert_eq!(lines[0].output_hash.len(), 64);
        assert!(lines[0].successful);
        assert_eq!(
            lines[0].training_staged,
            Some(true),
            "successful receipt must record training_staged=true"
        );
        let staged = std::fs::read_to_string(ws.0.join(".susi/distillation_staged.jsonl")).unwrap();
        let sample: serde_json::Value = serde_json::from_str(staged.trim()).unwrap();
        assert_eq!(sample["intent"], "archive-mission");
        assert_eq!(sample["action"], "exec_command");
        assert_eq!(
            sample["performance_metadata"]["receipt_id"],
            lines[0].receipt_id
        );
        let raw = std::fs::read_to_string(ReceiptArchive::path(&ws.0)).unwrap();
        assert!(!raw.contains("Linux host"));
    }

    #[test]
    fn rotate_shifts_generations_and_drops_oldest() {
        let ws = Workspace::new();
        let path = ReceiptArchive::path(&ws.0);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();
        // Seed generations .1 and .2 plus an oversized live file.
        std::fs::write(path.with_file_name(format!("{name}.1")), "gen1\n").unwrap();
        std::fs::write(path.with_file_name(format!("{name}.2")), "gen2\n").unwrap();
        std::fs::write(&path, "x".repeat(16 * 1024 * 1024 + 8)).unwrap();

        ReceiptArchive::rotate_if_large(&path);

        assert_eq!(
            std::fs::read_to_string(path.with_file_name(format!("{name}.1"))).unwrap(),
            "x".repeat(16 * 1024 * 1024 + 8),
            "the live file must become generation .1"
        );
        assert_eq!(
            std::fs::read_to_string(path.with_file_name(format!("{name}.2"))).unwrap(),
            "gen1\n"
        );
        assert_eq!(
            std::fs::read_to_string(path.with_file_name(format!("{name}.3"))).unwrap(),
            "gen2\n"
        );
        assert!(
            !path.exists(),
            "rotation leaves no live file to append over"
        );
    }

    #[test]
    fn load_reads_rotated_generations_oldest_first() {
        let ws = Workspace::new();
        let session = session(&ws);
        let _activation = EvidenceSession::activate(&session);
        EvidenceSession::capture_call("oldest", &serde_json::json!({}), &ws.0, || {
            Ok("one".to_string())
        })
        .unwrap();
        let live = ReceiptArchive::path(&ws.0);
        let name = live.file_name().unwrap().to_str().unwrap();
        std::fs::rename(&live, live.with_file_name(format!("{name}.2"))).unwrap();
        EvidenceSession::capture_call("middle", &serde_json::json!({}), &ws.0, || {
            Ok("two".to_string())
        })
        .unwrap();
        std::fs::rename(&live, live.with_file_name(format!("{name}.1"))).unwrap();
        EvidenceSession::capture_call("newest", &serde_json::json!({}), &ws.0, || {
            Ok("three".to_string())
        })
        .unwrap();

        let tools = ReceiptArchive::load_audit_lines(&ws.0)
            .into_iter()
            .map(|receipt| receipt.tool)
            .collect::<Vec<_>>();
        assert_eq!(tools, ["oldest", "middle", "newest"]);
    }

    #[test]
    fn archive_cannot_satisfy_citation_after_live_session_ends() {
        let ws = Workspace::new();
        let session = session(&ws);
        let activation = EvidenceSession::activate(&session);
        EvidenceSession::capture_call("t", &serde_json::json!(null), &ws.0, || {
            Ok("observed".into())
        })
        .unwrap();
        let receipt_id = session.receipts()[0].id.clone();
        assert!(!ReceiptArchive::load_audit_lines(&ws.0).is_empty());
        drop(activation);
        drop(session);
        let cited =
            format!(r#"{{"citations":[{{"receipt_id":"{receipt_id}","json_pointer":null}}]}}"#);
        let err = EvidenceSession::verify_answer(&cited, &ws.0)
            .expect("citation attempt")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no live mission session") || err.contains("TRUTH_UNVERIFIED"),
            "archive must not restore authority: {err}"
        );
    }

    #[test]
    fn append_refuses_to_race_a_cross_process_archive_operation() {
        let ws = Workspace::new();
        let susi_dir = ws.0.join(".susi");
        std::fs::create_dir_all(&susi_dir).unwrap();
        let _file_lock =
            crate::susi_core::commit_log::FileLock::acquire(&susi_dir, "receipt_archive").unwrap();
        let session = session(&ws);
        let _activation = EvidenceSession::activate(&session);
        EvidenceSession::capture_call("exec_command", &serde_json::json!({}), &ws.0, || {
            Ok("observed".to_string())
        })
        .unwrap();

        assert!(!ReceiptArchive::path(&ws.0).exists());
    }

    #[test]
    fn unsuccessful_receipt_records_no_staging_attempt() {
        let ws = Workspace::new();
        let session = session(&ws);
        let _activation = EvidenceSession::activate(&session);
        EvidenceSession::capture_call(
            "exec_command",
            &serde_json::json!({"cmd": "false"}),
            &ws.0,
            || Err(crate::susi_error::EaiError::io("command failed")),
        )
        .unwrap_err();
        let lines = ReceiptArchive::load_audit_lines(&ws.0);
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].successful);
        assert_eq!(
            lines[0].training_staged, None,
            "unsuccessful receipts must not attempt staging"
        );
        assert!(
            !ws.0.join(".susi/distillation_staged.jsonl").exists(),
            "no staging file should exist for unsuccessful receipts"
        );
    }

    #[cfg(unix)]
    #[test]
    fn staging_failure_records_training_staged_false() {
        let ws = Workspace::new();
        let susi_dir = ws.0.join(".susi");
        std::fs::create_dir_all(&susi_dir).unwrap();
        // Block staging by creating a directory where the file should be —
        // open() will fail on a directory path.
        std::fs::create_dir_all(susi_dir.join("distillation_staged.jsonl")).unwrap();
        let session = session(&ws);
        let _activation = EvidenceSession::activate(&session);
        EvidenceSession::capture_call("test_tool", &serde_json::json!({}), &ws.0, || {
            Ok("success".to_string())
        })
        .unwrap();
        let lines = ReceiptArchive::load_audit_lines(&ws.0);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].successful);
        assert_eq!(
            lines[0].training_staged,
            Some(false),
            "staging failure must be recorded as training_staged=false"
        );
    }

    #[test]
    fn v1_records_without_training_staged_deserialize() {
        // Older v1 records predate the training_staged field. They must
        // deserialize as None so load_audit_lines never rejects them.
        let v1 = serde_json::json!({
            "schema": ARCHIVE_SCHEMA,
            "kind": "tool_receipt",
            "session_id": "s1",
            "mission_goal_hash": "abc",
            "training_intent": "test",
            "receipt_id": "r1",
            "tool": "exec_command",
            "arguments": "{}",
            "observed_at": 1000,
            "output_hash": "def",
            "successful": true,
            "archived_at": 1001
        });
        let parsed: ArchivedReceipt = serde_json::from_value(v1).unwrap();
        assert_eq!(
            parsed.training_staged, None,
            "v1 records must deserialize training_staged as None"
        );
    }
}
