//! Audited inference-placement decisions, looked up by decision id.
//!
//! Every placement the router takes is written to the workspace audit log
//! as an `INFERENCE_PLACEMENT` entry whose details carry
//! `contract.decision_id`. This one lookup serves the MCP
//! `placement_decision` tool (plan B4 group 12), so every caller agrees
//! about which record an id names.

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

/// Headroom under the legacy audit detail cap for credential masking, which
/// may lengthen a value it rewrites.
const AUDIT_RECORD_HEADROOM: usize = 64;

/// The `INFERENCE_PLACEMENT` audit details for a router `placement`: a
/// compact JSON record of the decision (contract, target, provider, reason,
/// policy, requires, max_cost, allow_cloud) that always fits under the
/// legacy audit detail cap. The full placement document (candidate lists,
/// local stats) routinely exceeds that cap, and the cap truncates it into
/// JSON that [`find_placement_decision`] cannot parse, so a decision id a
/// completion returned would never resolve. When even the compact record is
/// too long, `reason`, then `policy`, then `provider`, then `target` are
/// shortened, each with a visible `…` marker.
pub fn placement_audit_details(placement: &serde_json::Value) -> String {
    let budget = crate::audit_fidelity::LEGACY_MAX_CHARS - AUDIT_RECORD_HEADROOM;
    let pick = |key: &str| {
        placement
            .get(key)
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };
    let mut record = serde_json::json!({
        "contract": pick("contract"),
        "target": pick("target"),
        "provider": pick("provider"),
        "reason": pick("reason"),
        "policy": pick("policy"),
        "requires": pick("requires"),
        "max_cost": pick("max_cost"),
        "allow_cloud": pick("allow_cloud"),
    });
    for key in ["reason", "policy", "provider", "target"] {
        let over = record.to_string().chars().count().saturating_sub(budget);
        if over == 0 {
            break;
        }
        if let Some(text) = record.get(key).and_then(serde_json::Value::as_str) {
            let keep = text.chars().count().saturating_sub(over + 1);
            let shortened: String = text.chars().take(keep).chain(['…']).collect();
            record[key] = serde_json::Value::String(shortened);
        }
    }
    record.to_string()
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

    /// A full router placement is larger than the legacy audit cap. Written
    /// through the real audit logger, its compact record still resolves.
    #[test]
    fn a_full_placement_written_to_the_audit_log_resolves_by_id() {
        let ws = std::env::temp_dir().join(format!(
            "susi-placement-record-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(ws.join(".susi")).unwrap();
        let candidates: Vec<String> = (0..40).map(|i| format!("zhipu-glm-{i}")).collect();
        let placement = serde_json::json!({
            "contract": {
                "schema": "susi/placement/v1",
                "decision_id": "placement-5264-1793776765-1",
                "decided_at_unix": 1_793_776_765u64,
            },
            "target": "cloud",
            "provider": "openrouter-free",
            "reason": "local engine not ready; ".repeat(40),
            "policy": "cost-first",
            "local_model": null,
            "local_ready": false,
            "local_stats": {"model_id": "m", "ema_tokens_per_sec": 0.0},
            "cloud_candidates": candidates,
            "cooled_candidates": [],
            "requires": "chat",
            "max_cost": 0.5,
            "allow_cloud": true,
        });
        assert!(placement.to_string().chars().count() > crate::audit_fidelity::LEGACY_MAX_CHARS);
        let details = placement_audit_details(&placement);
        assert!(
            details.chars().count()
                <= crate::audit_fidelity::LEGACY_MAX_CHARS - AUDIT_RECORD_HEADROOM
        );
        SusiAuditLogger::log_event(&ws, "INFERENCE_PLACEMENT", &details);
        let found = lookup_placement_decision(&ws, "placement-5264-1793776765-1").unwrap();
        let decision = &found["decision"];
        assert_eq!(decision["target"], "cloud");
        assert_eq!(decision["provider"], "openrouter-free");
        assert_eq!(decision["requires"], "chat");
        assert_eq!(decision["allow_cloud"], true);
        assert!(decision["reason"].as_str().is_some_and(|reason| reason
            .starts_with("local engine not ready")
            && reason.ends_with('…')));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn a_small_placement_record_is_kept_whole() {
        let placement = serde_json::json!({
            "contract": {"decision_id": "placement-1-2-3"},
            "target": "local",
            "provider": null,
            "reason": "local model ready",
            "policy": "local-first",
            "requires": null,
            "max_cost": null,
            "allow_cloud": false,
        });
        let record: serde_json::Value =
            serde_json::from_str(&placement_audit_details(&placement)).unwrap();
        assert_eq!(record, placement);
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
