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

