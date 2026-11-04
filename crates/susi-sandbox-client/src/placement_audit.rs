//! Audited inference-placement decisions, looked up by decision id.
//!
//! Every placement the router takes is written to the workspace audit log
//! as an `INFERENCE_PLACEMENT` entry whose details carry
//! `contract.decision_id`. One lookup serves both the 9091
//! `/runtime/placement/decisions/{id}` route and the MCP
//! `placement_decision` tool (plan B4 group 12), so the two surfaces can
//! never disagree about which record an id names.

use std::path::Path;

use crate::manager::SusiAuditLogger;

/// How many trailing audit entries a decision lookup scans.
pub const PLACEMENT_LOOKUP_WINDOW: usize = 20_000;

/// Why a decision lookup returned no record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementLookupError {
    /// The id is not `placement-<pid>-<unix>-<seq>`; the audit log was not read.
    InvalidId,
    /// No `INFERENCE_PLACEMENT` entry with that id in the scanned window.
    NotFound,
}

impl PlacementLookupError {
    /// The caller-facing message (the route's error body, the tool's error).
    pub fn message(self) -> String {
        match self {
            Self::InvalidId => "invalid placement decision id".to_string(),
            Self::NotFound => format!(
                "no INFERENCE_PLACEMENT audit entry with that decision id in the last {PLACEMENT_LOOKUP_WINDOW} audit entries"
            ),
        }
    }
}

/// Decision ids are `placement-<pid>-<unix>-<seq>`; anything else is refused
/// before touching the audit log.
pub fn valid_placement_id(id: &str) -> bool {
    id.len() <= 64
        && id.strip_prefix("placement-").is_some_and(|rest| {
            let parts: Vec<&str> = rest.split('-').collect();
            parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// The newest audited placement decision with `id` in `audit_log` (one JSON
/// entry per line), plus the audit entry's timestamp and hash so the record
/// can be checked against the chain.
pub fn find_placement_decision(audit_log: &str, id: &str) -> Option<serde_json::Value> {
    audit_log.lines().rev().find_map(|line| {
        let entry: serde_json::Value = serde_json::from_str(line).ok()?;
        if entry.get("type")?.as_str()? != "INFERENCE_PLACEMENT" {
            return None;
        }
        let decision: serde_json::Value =
            serde_json::from_str(entry.get("details")?.as_str()?).ok()?;
        (decision.pointer("/contract/decision_id")?.as_str()? == id).then(|| {
            serde_json::json!({
                "decision": decision,
                "audit": {
                    "ts": entry.get("ts"),
                    "entry_hash": entry.get("entry_hash"),
                },
            })
        })
    })
}

/// Validate `id`, then find its decision in the last
/// [`PLACEMENT_LOOKUP_WINDOW`] entries of `workspace`'s audit log.
pub fn lookup_placement_decision(
    workspace: &Path,
    id: &str,
) -> Result<serde_json::Value, PlacementLookupError> {
    if !valid_placement_id(id) {
        return Err(PlacementLookupError::InvalidId);
    }
    let log = SusiAuditLogger::read_audit_log(workspace, PLACEMENT_LOOKUP_WINDOW);
    find_placement_decision(&log, id).ok_or(PlacementLookupError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &str, id: &str, target: &str, hash: &str) -> String {
        serde_json::json!({
            "type": kind,
            "ts": 1,
            "entry_hash": hash,
            "details": serde_json::json!({
                "target": target,
                "contract": { "decision_id": id },
            })
            .to_string(),
        })
        .to_string()
    }

    #[test]
    fn decisions_are_found_by_id_newest_first() {
        let log = [
            entry("INFERENCE_PLACEMENT", "placement-1-2-3", "local", "old"),
            entry("OTHER", "placement-1-2-3", "local", "other"),
            entry("INFERENCE_PLACEMENT", "placement-1-2-3", "cloud", "new"),
        ]
        .join("\n");
        let found = find_placement_decision(&log, "placement-1-2-3").unwrap();
        assert_eq!(found["decision"]["target"], "cloud");
        assert_eq!(found["audit"]["entry_hash"], "new");
        assert!(find_placement_decision(&log, "placement-4-4-4").is_none());
    }

    #[test]
    fn ids_outside_the_decision_shape_are_refused() {
        assert!(valid_placement_id("placement-12-1790000000-3"));
        for bad in [
            "placement-1-2",
            "placement-a-2-3",
            "../audit.log",
            "placement-1-2-3-4",
            "",
        ] {
            assert!(!valid_placement_id(bad), "{bad}");
        }
    }

    #[test]
    fn lookup_reads_the_workspace_audit_log() {
        let ws = std::env::temp_dir().join(format!(
            "susi-placement-audit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(ws.join(".susi")).unwrap();
        std::fs::write(
            ws.join(".susi/audit.log"),
            entry("INFERENCE_PLACEMENT", "placement-7-8-9", "local", "h1") + "\n",
        )
        .unwrap();
        let found = lookup_placement_decision(&ws, "placement-7-8-9").unwrap();
        assert_eq!(found["decision"]["target"], "local");
        assert_eq!(found["audit"]["entry_hash"], "h1");
        assert_eq!(
            lookup_placement_decision(&ws, "placement-7-8-0"),
            Err(PlacementLookupError::NotFound)
        );
        assert_eq!(
            lookup_placement_decision(&ws, "../audit.log"),
            Err(PlacementLookupError::InvalidId)
        );
        assert!(PlacementLookupError::NotFound
            .message()
            .contains(&PLACEMENT_LOOKUP_WINDOW.to_string()));
        let _ = std::fs::remove_dir_all(&ws);
    }
}
