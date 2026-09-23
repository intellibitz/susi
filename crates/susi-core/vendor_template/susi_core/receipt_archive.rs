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

/// One append-only audit line. Response bodies are omitted — hashes + provenance only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchivedReceipt {
    pub schema: String,
    pub kind: String,
    pub session_id: String,
    pub mission_goal_hash: String,
    pub receipt_id: String,
    pub tool: String,
    pub arguments: String,
    pub observed_at: u64,
    pub output_hash: String,
    pub successful: bool,
    pub archived_at: u64,
}

pub struct ReceiptArchive;

impl ReceiptArchive {
    pub fn path(workspace: &Path) -> PathBuf {
        workspace.join(ARCHIVE_REL)
    }

    /// Best-effort append. Archive write failure never fails the mission.
    pub fn append(workspace: &Path, session_id: &str, mission_goal: &str, receipt: &ToolReceipt) {
        let record = ArchivedReceipt {
            schema: ARCHIVE_SCHEMA.into(),
            kind: "tool_receipt".into(),
            session_id: session_id.to_string(),
            mission_goal_hash: hex::encode(Sha256::digest(mission_goal.as_bytes())),
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
        };
        let Ok(line) = serde_json::to_string(&record) else {
            return;
        };
        let path = Self::path(workspace);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _guard = archive_lock().lock();
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(file, "{line}");
        }
    }

    /// Read archived lines for audit tooling. Does **not** restore live ledger
    /// authority — callers must not feed these into citation resolution.
    pub fn load_audit_lines(workspace: &Path) -> Vec<ArchivedReceipt> {
        let Ok(text) = std::fs::read_to_string(Self::path(workspace)) else {
            return Vec::new();
        };
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
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
        assert_eq!(lines[0].output_hash.len(), 64);
        assert!(lines[0].successful);
        let raw = std::fs::read_to_string(ReceiptArchive::path(&ws.0)).unwrap();
        assert!(!raw.contains("Linux host"));
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
}
