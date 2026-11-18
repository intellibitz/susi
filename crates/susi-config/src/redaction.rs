//! Credential redaction delegating to `susi_error::redact_for_metrics`.
//! Host-override pattern loading lives in Susi Commercial Apps (`susi-config-runtime`).

#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct GovernancePatterns {
    #[serde(default)]
    pub secret_tokens: Vec<String>,
}

/// Redact credential patterns and held environment secrets from `text`.
#[must_use]
pub fn redact_credentials(text: &str) -> String {
    susi_error::redact_for_metrics(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_credentials_masks_bundled_governance_tokens() {
        let out =
            redact_credentials("token sk-1234567890abcdef1234567890abcdef and AIzaSyDemoKey123456");
        assert!(
            !out.contains("sk-1234567890abcdef1234567890abcdef"),
            "{out}"
        );
        assert!(!out.contains("AIzaSyDemoKey123456"), "{out}");
    }
}
