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

#[cfg(test)]
mod prop_tests {
    use super::redact_patterns;
    use proptest::prelude::*;

    proptest! {
        // Property: a pattern followed by token-shaped characters is always
        // redacted — no secret suffix survives in the output.
        #[test]
        fn pattern_tail_never_survives(
            pat in "[a-z]{2,6}-",
            tail in "[A-Za-z0-9_-]{1,16}",
            tail2 in "[A-Za-z0-9_-]{0,8}",
        ) {
            let input = format!("x {pat}{tail} y {pat}{tail2} z");
            let out = redact_patterns(std::slice::from_ref(&pat), &input);
            let needle1 = format!("{pat}{tail}");
            let needle2 = format!("{pat}{tail2}");
            prop_assert!(!out.contains(&needle1));
            prop_assert!(!out.contains(&needle2));
        }

        // Property: redaction is idempotent for patterns that cannot appear
        // inside the "[REDACTED]" marker itself.
        #[test]
        fn redaction_is_idempotent(
            pat in "[a-z]{2,6}-",
            text in "[ -~]{0,200}",
        ) {
            let once = redact_patterns(std::slice::from_ref(&pat), &text);
            let twice = redact_patterns(&[pat], &once);
            prop_assert_eq!(once, twice);
        }
    }
}
