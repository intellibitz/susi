//! Shared CLI JSON printing with secret redaction.

use crate::anyhow::Result;

/// Pretty-print a serializable value to stdout after redacting secret-shaped tokens.
pub fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!(
        "{}",
        susi_config::redact_credentials(&serde_json::to_string_pretty(value)?)
    );
    Ok(())
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    #[test]
    fn print_json_succeeds_for_structured_object() {
        let payload = serde_json::json!({
            "status": "ok",
            "count": 4,
            "nested": { "active": true }
        });
        assert!(print_json(&payload).is_ok());
    }

    #[test]
    fn print_json_succeeds_for_array_and_scalar_values() {
        let list = vec!["alpha", "beta", "gamma"];
        assert!(print_json(&list).is_ok());
        assert!(print_json(&42_u64).is_ok());
    }

    #[test]
    fn redact_pipeline_masks_secret_shaped_tokens_in_pretty_json() {
        let raw_token = format!("sk-{}", "a".repeat(32));
        let payload = serde_json::json!({
            "provider": "openai",
            "Authorization": format!("Bearer {raw_token}"),
        });
        let pretty = serde_json::to_string_pretty(&payload).unwrap();
        let redacted = susi_config::redact_credentials(&pretty);
        assert!(!redacted.contains(&raw_token));
        assert!(redacted.contains("\"provider\": \"openai\""));
        assert!(print_json(&payload).is_ok());
    }

    #[test]
    fn pretty_serialization_preserves_non_secret_fields() {
        let payload = serde_json::json!({
            "service": "susi-gmcp",
            "healthy": true,
            "ports": [18080, 18081]
        });
        let pretty = serde_json::to_string_pretty(&payload).unwrap();
        let redacted = susi_config::redact_credentials(&pretty);
        let parsed: serde_json::Value = serde_json::from_str(&redacted).unwrap();
        assert_eq!(parsed["service"], "susi-gmcp");
        assert_eq!(parsed["healthy"], true);
        assert_eq!(parsed["ports"][0], 18080);
    }
}
