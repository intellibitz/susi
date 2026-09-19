// Pure, config-agnostic secret-token redaction (Mandate 10: No Secret
// Leaks). Callers own loading the actual configured patterns (from
// `SusiConfig`) so this primitive has no dependency on the config/sandbox
// layer — that's what let `gawd::security` and `sandbox::manager`'s audit
// logger share it without a cross-crate cycle between them.

/// Replaces every occurrence of a configured secret-token pattern (and its
/// trailing token-shaped characters) in `text` with `[REDACTED]`.
pub fn redact_patterns(patterns: &[String], text: &str) -> String {
    let mut redacted = text.to_string();
    for pattern in patterns {
        if pattern.is_empty() {
            continue;
        }
        if let Ok(re) = regex::Regex::new(&format!("{}[A-Za-z0-9_-]*", regex::escape(pattern))) {
            redacted = re.replace_all(&redacted, "[REDACTED]").to_string();
        }
    }
    redacted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_masks_token_not_just_prefix() {
        let patterns = vec!["sk-".to_string()];
        let redacted = redact_patterns(&patterns, "token=sk-proj12345abcXYZ rest of log");
        assert!(!redacted.contains("proj12345"));
        assert!(redacted.contains("[REDACTED]"));
        assert!(redacted.contains("rest of log"));
    }

    #[test]
    fn test_empty_pattern_is_skipped() {
        let patterns = vec![String::new(), "secret".to_string()];
        let redacted = redact_patterns(&patterns, "the secret123 value");
        assert!(redacted.contains("[REDACTED]"));
    }
}
