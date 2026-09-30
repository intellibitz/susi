//! Structured audit export for SIEM.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub ts_unix: u64,
    pub action: String,
    pub actor: String,
    pub outcome: String,
}

/// Export events as newline-delimited JSON for SIEM ingest.
#[must_use]
pub fn export_ndjson(events: &[AuditEvent]) -> String {
    events
        .iter()
        .map(|e| {
            json!({
                "ts": e.ts_unix,
                "action": e.action,
                "actor": e.actor,
                "outcome": e.outcome,
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse one NDJSON line back (round-trip helper).
pub fn parse_ndjson_line(line: &str) -> Result<Value, String> {
    serde_json::from_str(line).map_err(|e| e.to_string())
}

#[cfg(test)]
mod audit_export_tests {
    use super::*;

    #[test]
    fn audit_export_ndjson_for_siem() {
        let nd = export_ndjson(&[AuditEvent {
            ts_unix: 1,
            action: "start".into(),
            actor: "daemon".into(),
            outcome: "ok".into(),
        }]);
        let v = parse_ndjson_line(&nd).unwrap();
        assert_eq!(v["action"], "start");
        assert_eq!(v["outcome"], "ok");
    }
}
