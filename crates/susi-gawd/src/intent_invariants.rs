//! Property / metamorphic intent checks for pure reflexes (VC-201-008).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentInvariant {
    pub name: String,
    pub holds: bool,
    pub counterexample: Option<String>,
}

/// Metamorphic: same action for whitespace-normalized inputs.
#[must_use]
pub fn metamorphic_whitespace(
    action_for: impl Fn(&str) -> String,
    a: &str,
    b: &str,
) -> IntentInvariant {
    let left = action_for(a.trim());
    let right = action_for(b.trim());
    if left == right {
        IntentInvariant {
            name: "whitespace_normalize".into(),
            holds: true,
            counterexample: None,
        }
    } else {
        IntentInvariant {
            name: "whitespace_normalize".into(),
            holds: false,
            counterexample: Some(format!("{a:?} → {left}; {b:?} → {right}")),
        }
    }
}

/// Pure reflex must not invent actions outside an allowlist.
#[must_use]
pub fn action_in_allowlist(action: &str, allow: &[&str]) -> IntentInvariant {
    let holds = allow.contains(&action);
    IntentInvariant {
        name: "allowlist".into(),
        holds,
        counterexample: if holds { None } else { Some(action.into()) },
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn metamorphic_whitespace_holds_when_trimmed_inputs_produce_identical_action() {
        let inv = metamorphic_whitespace(|s| s.to_ascii_lowercase(), "  STATUS ", "\tstatus\n");
        assert_eq!(inv.name, "whitespace_normalize");
        assert!(inv.holds);
        assert_eq!(inv.counterexample, None);
    }

    #[test]
    fn metamorphic_whitespace_fails_with_counterexample_when_actions_diverge() {
        let inv = metamorphic_whitespace(|s| s.to_string(), "status", "deploy");
        assert_eq!(inv.name, "whitespace_normalize");
        assert!(!inv.holds);
        assert!(inv
            .counterexample
            .as_deref()
            .unwrap_or_default()
            .contains("status"));
    }

    #[test]
    fn action_in_allowlist_holds_for_permitted_action_and_clears_counterexample() {
        let inv = action_in_allowlist("read_file", &["read_file", "list_dir"]);
        assert_eq!(inv.name, "allowlist");
        assert!(inv.holds);
        assert_eq!(inv.counterexample, None);
    }

    #[test]
    fn action_in_allowlist_fails_and_records_offending_action_as_counterexample() {
        let inv = action_in_allowlist("drop_table", &["read_file", "list_dir"]);
        assert_eq!(inv.name, "allowlist");
        assert!(!inv.holds);
        assert_eq!(inv.counterexample, Some("drop_table".to_string()));
    }

    #[test]
    fn intent_invariant_round_trips_through_serde_json_and_empty_allowlist_rejects() {
        let denied = action_in_allowlist("noop", &[]);
        assert!(!denied.holds);
        assert_eq!(denied.counterexample.as_deref(), Some("noop"));

        let json = serde_json::to_string(&denied).unwrap();
        let decoded: IntentInvariant = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, denied);
    }
}
