//! Schema-drift alerts -> task records (VC-201-088 / T-CLAUDE-341).
//!
//! When a live response no longer matches its recorded profile the mismatch
//! is drift: a field vanished, a new field appeared, an error shape changed.
//! Each drift lowers confidence in the profile and becomes a task record —
//! a reviewable unit carrying the diff and an acceptance check — never an
//! in-place rewrite.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One field-level difference between a live response and its profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftItem {
    /// `missing-field`, `unexpected-field`, `new-error-code`.
    pub kind: String,
    /// Dotted field path or error code.
    pub field: String,
}

/// A drift observation for one profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriftAlert {
    pub profile_id: String,
    pub observed_at: String,
    pub items: Vec<DriftItem>,
    /// How much confidence the profile keeps (`high` -> `low` after drift).
    pub confidence_after: String,
}

/// Diff a live response's top-level fields against the declared set.
/// `declared` uses the shape `fields` vocabulary (dotted prefixes allowed).
#[must_use]
pub fn detect_drift(
    profile_id: &str,
    declared: &[String],
    response: &serde_json::Value,
    observed_at: &str,
) -> Option<DriftAlert> {
    let fields: BTreeSet<String> = response
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    let mut items = Vec::new();
    for d in declared {
        if !fields
            .iter()
            .any(|f| f == d || f.starts_with(&format!("{d}.")))
        {
            items.push(DriftItem {
                kind: "missing-field".into(),
                field: d.clone(),
            });
        }
    }
    for f in &fields {
        if !declared
            .iter()
            .any(|d| f == d || f.starts_with(&format!("{d}.")))
        {
            items.push(DriftItem {
                kind: "unexpected-field".into(),
                field: f.clone(),
            });
        }
    }
    if items.is_empty() {
        return None;
    }
    Some(DriftAlert {
        profile_id: profile_id.to_string(),
        observed_at: observed_at.to_string(),
        items,
        confidence_after: "low".into(),
    })
}

/// A reviewable task record for one alert — the same shape `susi tasks`
/// consumes: title, goal and an acceptance command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftTask {
    pub title: String,
    pub goal: String,
    /// The check a fix must satisfy (re-run the profile validation).
    pub accept: Vec<String>,
}

/// Render an alert as a task record.
#[must_use]
pub fn to_task(alert: &DriftAlert) -> DriftTask {
    let fields: Vec<String> = alert
        .items
        .iter()
        .map(|i| format!("{}:{}", i.kind, i.field))
        .collect();
    DriftTask {
        title: format!("drift on {}", alert.profile_id),
        goal: format!(
            "Profile {} drifted at {}: {}. Update the profile or record a deviation, then re-validate.",
            alert.profile_id,
            alert.observed_at,
            fields.join(", ")
        ),
        accept: vec![
            "cargo".into(),
            "test".into(),
            "-p".into(),
            "susi-vendor-models".into(),
            "eco_profile".into(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eco_drift_alerts_detects_missing_and_new_fields() {
        let alert = detect_drift(
            "openai-chat-completions",
            &["id".into(), "choices".into(), "usage".into()],
            &serde_json::json!({"id": "x", "choices": [], "surprise": true}),
            "2026-09-01",
        )
        .expect("drift detected");
        let kinds: Vec<_> = alert.items.iter().map(|i| i.kind.as_str()).collect();
        assert!(kinds.contains(&"missing-field")); // usage gone
        assert!(kinds.contains(&"unexpected-field")); // surprise new
        assert_eq!(alert.confidence_after, "low");
    }

    #[test]
    fn eco_drift_alerts_none_when_response_matches() {
        let alert = detect_drift(
            "p",
            &["id".into()],
            &serde_json::json!({"id": "x", "id.sub": 1}),
            "2026-09-01",
        );
        assert!(alert.is_none());
    }

    #[test]
    fn eco_drift_alerts_become_tasks_with_acceptance() {
        let alert = detect_drift(
            "openai-chat-completions",
            &["id".into(), "choices".into()],
            &serde_json::json!({"id": "x"}),
            "2026-09-01",
        )
        .unwrap();
        let task = to_task(&alert);
        assert!(task.title.contains("openai-chat-completions"));
        assert!(task.goal.contains("missing-field:choices"));
        assert_eq!(task.accept[0], "cargo");
        assert!(task.accept.iter().any(|a| a.contains("eco_profile")));
    }
}
